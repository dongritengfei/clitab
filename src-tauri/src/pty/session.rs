use super::lock;
use super::registry::{Registry, StopOutcome};
use super::shell_integration;
use crate::osc::{self, OscEvent, OscParser};
use crate::status::{self, StatusEvent};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Size the PTY starts with, until the frontend reports the real one.
const INITIAL_ROWS: u16 = 24;
const INITIAL_COLS: u16 = 80;
/// Bigger chunks mean fewer IPC round-trips on noisy commands.
const READ_CHUNK: usize = 16 * 1024;
/// Cap on the per-tab replay ring: the most recent bytes of PTY output, handed
/// to the renderer whenever a terminal view (re)attaches. Keeps a tab usable
/// across a webview reload instead of leaving it blank.
const REPLAY_LIMIT: usize = 256 * 1024;
/// A quiet period while an assistant turn is in flight means it needs input.
const TURN_IDLE: Duration = Duration::from_secs(2);
const WATCHER_POLL: Duration = Duration::from_millis(200);

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("PTY error: {0}")]
    Pty(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Routing of PTY output for one tab.
#[derive(Debug, Default)]
struct StreamState {
    /// A terminal view is attached: chunks are emitted live as well as ringed.
    attached: bool,
    /// Sliding window of the most recent output, replayed on attach.
    recent: VecDeque<u8>,
    /// Total bytes ever pushed into the ring. Every emitted chunk carries the
    /// position it starts at, and `attach_stream` reports the position the
    /// replay ends at, so a renderer that re-attaches (webview reload) can drop
    /// live chunks the replay already covered instead of writing them twice.
    position: u64,
}

/// Append to the replay ring, dropping from the front once it is full.
fn push_recent(ring: &mut VecDeque<u8>, data: &[u8]) {
    ring.extend(data.iter().copied());
    let overflow = ring.len().saturating_sub(REPLAY_LIMIT);
    for _ in 0..overflow {
        ring.pop_front();
    }
}

pub struct PtySession {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child_killer: Arc<Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// False once the child process is gone.
    running: Arc<AtomicBool>,
    stream: Arc<Mutex<StreamState>>,
    /// Deleting the per-tab shell rc files happens in `Drop`.
    integration: shell_integration::Prepared,
}

impl PtySession {
    /// Spawn `cwd`'s shell in a fresh PTY and start pumping its output.
    pub fn new(
        tab_id: String,
        cwd: String,
        app: AppHandle,
        registry: Arc<Registry>,
    ) -> Result<Self, SessionError> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows: INITIAL_ROWS,
                cols: INITIAL_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| SessionError::Pty(e.to_string()))?;

        // An unset (or empty) $SHELL must still produce a usable shell.
        let shell = std::env::var("SHELL").unwrap_or_default();
        let shell = if shell.is_empty() { "bash".to_string() } else { shell };
        let integration = shell_integration::prepare(&tab_id, &shell);
        let mut cmd = CommandBuilder::new(&integration.shell);
        for arg in &integration.args {
            cmd.arg(arg);
        }
        for (key, value) in &integration.env {
            cmd.env(key, value);
        }
        cmd.cwd(&cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "clitab");

        let mut child = match pair.slave.spawn_command(cmd) {
            Ok(child) => child,
            Err(e) => {
                integration.cleanup();
                return Err(SessionError::Pty(e.to_string()));
            }
        };

        // The master must hold the last handle to the slave side, otherwise the
        // reader never sees EOF when the shell exits.
        drop(pair.slave);

        // The child is already running: if grabbing the reader/writer fails we
        // must not leave an orphaned shell or its rc files behind.
        let handles = pair
            .master
            .try_clone_reader()
            .and_then(|reader| pair.master.take_writer().map(|writer| (reader, writer)))
            .map_err(|e| SessionError::Pty(e.to_string()));
        let (reader, writer) = match handles {
            Ok(handles) => handles,
            Err(e) => {
                let _ = child.kill();
                integration.cleanup();
                return Err(e);
            }
        };
        let killer = child.clone_killer();

        let master = Arc::new(Mutex::new(pair.master));
        let writer = Arc::new(Mutex::new(writer));
        let child_killer = Arc::new(Mutex::new(Some(killer)));
        let running = Arc::new(AtomicBool::new(true));
        let stream = Arc::new(Mutex::new(StreamState::default()));
        let last_activity = Arc::new(Mutex::new(Instant::now()));

        // Set while a program that owns the title (Claude Code) is in the
        // foreground, and cleared when its turn ends / the shell prompt returns.
        let program_active = Arc::new(AtomicBool::new(false));

        // The attention watcher needs the registry too (enter_waiting), and
        // the reader thread takes ownership of the original below.
        let watcher_registry = Arc::clone(&registry);

        // Set when the tab flashed for the current turn — shared between the
        // idle watcher and the hook protocol so an explicit `stop` event can
        // flash immediately without the watcher repeating it 2s later.
        let flashed = Arc::new(AtomicBool::new(false));

        // Reader thread: pump PTY output into the renderer + OSC parser.
        {
            let tab_id = tab_id.clone();
            let app = app.clone();
            let stream = Arc::clone(&stream);
            let running = Arc::clone(&running);
            let last_activity = Arc::clone(&last_activity);
            let program_active = Arc::clone(&program_active);
            let flashed = Arc::clone(&flashed);
            thread::spawn(move || {
                Self::read_loop(
                    tab_id,
                    app,
                    registry,
                    reader,
                    stream,
                    running,
                    last_activity,
                    program_active,
                    flashed,
                );
            });
        }

        // Attention watcher: an assistant turn that stops producing output for
        // a while is waiting for the user, so flash the tab (once per turn).
        {
            let tab_id = tab_id.clone();
            let app = app.clone();
            let running = Arc::clone(&running);
            let last_activity = Arc::clone(&last_activity);
            let program_active = Arc::clone(&program_active);
            let registry = watcher_registry;
            let flashed = Arc::clone(&flashed);
            thread::spawn(move || {
                while running.load(Ordering::Relaxed) {
                    thread::sleep(WATCHER_POLL);
                    if !program_active.load(Ordering::Relaxed) {
                        flashed.store(false, Ordering::Relaxed);
                        continue;
                    }
                    let idle = lock(&last_activity).elapsed();
                    if idle < TURN_IDLE {
                        continue; // still streaming
                    }
                    // Silence this long with no Stop means the turn is over
                    // without the hook having fired (an Esc interrupt fires
                    // none): demote the stale mid-turn record, or the next
                    // typed prompt lands in its queue as a phantom. The turn
                    // ended when the output did — backdate past the silence.
                    let ended_at = now_ms().saturating_sub(idle.as_millis() as u64);
                    if registry.reconcile_idle_turn(&tab_id, ended_at) {
                        Self::emit_status(&app, &registry, &tab_id);
                    }
                    // swap: only the first caller of a turn emits the flash.
                    if !flashed.swap(true, Ordering::Relaxed) {
                        crate::attention::enter_waiting(&app, &registry, &tab_id);
                        let _ = app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
                    }
                }
            });
        }

        // Exit watcher: report the child's status and stop every other thread.
        {
            let app = app.clone();
            let running = Arc::clone(&running);
            let program_active = Arc::clone(&program_active);
            thread::spawn(move || {
                // Even when wait() itself fails the child is gone as far as we
                // are concerned; skipping the event would strand a dead tab in
                // both the renderer and the session map.
                let code = child.wait().map(|status| status.exit_code()).unwrap_or(u32::MAX);
                program_active.store(false, Ordering::Relaxed);
                running.store(false, Ordering::SeqCst);
                let _ =
                    app.emit("tab-exit", serde_json::json!({ "tab_id": tab_id, "code": code }));
            });
        }

        Ok(Self {
            writer,
            child_killer,
            master,
            running,
            stream,
            integration,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn read_loop(
        tab_id: String,
        app: AppHandle,
        registry: Arc<Registry>,
        mut reader: Box<dyn Read + Send>,
        stream: Arc<Mutex<StreamState>>,
        running: Arc<AtomicBool>,
        last_activity: Arc<Mutex<Instant>>,
        program_active: Arc<AtomicBool>,
        flashed: Arc<AtomicBool>,
    ) {
        let mut buf = vec![0u8; READ_CHUNK];
        let mut parser = OscParser::new();

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF: the shell is gone
                Ok(n) => {
                    let data = &buf[..n];
                    // Silence before this chunk: how long the PTY was idle.
                    // `begin_turn` uses it to tell a prompt queued behind a
                    // live turn (spinner output keeps gaps tiny) from a fresh
                    // prompt after an Esc interrupt, which fires no Stop and
                    // leaves the record looking mid-turn (see IDLE_GAP_MS).
                    let idle_gap_ms = lock(&last_activity).elapsed().as_millis() as u64;
                    *lock(&last_activity) = Instant::now();

                    // Split the chunk at OSC event boundaries and emit each
                    // segment *before* announcing its event. `tab-status` is
                    // what binds a timeline entry to a terminal line in the
                    // renderer, and the marker only lands on the line where the
                    // OSC sequence actually appeared if the bytes preceding it
                    // were already on their way (the renderer fences marker
                    // registration behind its own write queue).
                    let mut cursor = 0;
                    for (end, event) in parser.parse_with_end(data) {
                        emit_segment(&app, &tab_id, &stream, &data[cursor..end]);
                        cursor = end;
                        Self::handle_osc(
                            &tab_id,
                            &app,
                            &registry,
                            &program_active,
                            &last_activity,
                            &flashed,
                            idle_gap_ms,
                            event,
                        );
                    }
                    emit_segment(&app, &tab_id, &stream, &data[cursor..]);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }

        running.store(false, Ordering::SeqCst);
    }

    fn handle_osc(
        tab_id: &str,
        app: &AppHandle,
        registry: &Registry,
        program_active: &AtomicBool,
        last_activity: &Mutex<Instant>,
        flashed: &AtomicBool,
        idle_gap_ms: u64,
        event: OscEvent,
    ) {
        match event {
            OscEvent::TitleChanged(title) => {
                // Claude Code names the session; a plain shell reports a path.
                let is_path = osc::looks_like_path(&title);
                let is_program_title = !is_path && title.chars().count() > 3;
                program_active.store(is_program_title, Ordering::Relaxed);
                registry.set_title(tab_id, &title, is_program_title);
                // The classification travels with the event so the renderer
                // does not have to mirror (and drift from) this heuristic.
                let _ = app.emit(
                    "tab-title",
                    serde_json::json!({
                        "tab_id": tab_id,
                        "title": title,
                        "program_title": is_program_title,
                    }),
                );
            }
            OscEvent::CwdChanged(cwd) => {
                // Persist only on a real change: OSC 7 fires on every prompt,
                // so an ungated save would hit the disk on every Enter.
                if registry.set_cwd(tab_id, &cwd) {
                    if let Some(dir) = crate::last_cwd::dir(app) {
                        crate::last_cwd::save(&dir, &cwd);
                    }
                }
                let _ = app.emit(
                    "tab-cwd",
                    serde_json::json!({ "tab_id": tab_id, "cwd": cwd }),
                );
            }
            OscEvent::Bell => {
                crate::attention::enter_waiting(app, registry, tab_id);
                let _ = app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
            }
            OscEvent::PromptReady => {
                // The assistant's turn ended and the prompt came back.
                program_active.store(false, Ordering::Relaxed);
                registry.clear_program_title(tab_id);
                *lock(last_activity) = Instant::now();
                // After clear_program_title: the notification then carries the
                // same title the tab bar shows (the cwd it reverted to).
                crate::attention::enter_waiting(app, registry, tab_id);
                let _ = app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
                let _ = app.emit("prompt-ready", serde_json::json!({ "tab_id": tab_id }));
            }
            OscEvent::Clitab(json) => {
                // Unknown kinds and malformed payloads decode to None and are
                // dropped: a hook emitting something newer than this build
                // must be a no-op, never an error.
                if let Some(event) = status::decode(&json) {
                    Self::handle_status(tab_id, app, registry, flashed, idle_gap_ms, event);
                }
            }
        }
    }

    /// Apply one hook-protocol event: registry transition, then broadcast the
    /// tab's full protocol state so the renderer replaces (not merges) it.
    fn handle_status(
        tab_id: &str,
        app: &AppHandle,
        registry: &Registry,
        flashed: &AtomicBool,
        idle_gap_ms: u64,
        event: StatusEvent,
    ) {
        let now = now_ms();
        match event {
            StatusEvent::Prompt { msg } => registry.begin_turn(tab_id, now, msg, idle_gap_ms),
            StatusEvent::Tool { name } => registry.set_tool(tab_id, &name, now),
            StatusEvent::Stop => {
                let outcome = registry.end_turn(tab_id, now);
                // An accepted stop with an empty queue beats the 2s idle
                // heuristic: flash now and suppress the watcher's duplicate.
                // Like every flash trigger, this enters the triage queue
                // (set_waiting's transition guard keeps badge/notification
                // exactly-once). Every other outcome stays silent: with a
                // prompt queued, Claude Code auto-submits it right now and
                // needs no user — the next stop with an empty queue flashes
                // instead — and a duplicate Stop (`Ignored`) must not
                // re-enter waiting while the auto turn is running.
                flashed.store(true, Ordering::Relaxed);
                if outcome == StopOutcome::Idle {
                    crate::attention::enter_waiting(app, registry, tab_id);
                    let _ = app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
                }
                // Deliberately does NOT touch program_active or the title:
                // Claude Code is still running between turns. (Title revert
                // stays owned by the shell integration's `claude-done`.)
                // Two ordered snapshots: the renderer derives the turn-end row
                // from the `Done` one, then flips the queued prompt's row to
                // executing on the auto-start (`Thinking { auto: true }`) —
                // unless the popped head is system-injected (`system: true`,
                // e.g. a task notification): that turn has no row to flip.
                Self::emit_status(app, registry, tab_id);
                if let StopOutcome::AutoSubmit(msg) = outcome {
                    registry.begin_auto_turn(tab_id, now, msg);
                    Self::emit_status(app, registry, tab_id);
                }
                return;
            }
            StatusEvent::Notify { msg } => {
                registry.set_notice(tab_id, msg, now);
                crate::attention::enter_waiting(app, registry, tab_id);
                let _ = app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
            }
            // An answer means the user is already at the keyboard: no flash,
            // no waiting-queue entry — just a timeline record.
            StatusEvent::Answer { msg } => registry.set_answer(tab_id, msg, now),
        }
        Self::emit_status(app, registry, tab_id);
    }

    /// Broadcast the tab's full protocol state (replacement, not merge).
    /// `pub(crate)` because the manager emits it too: typing into a tab
    /// clears a pending notice (the keystroke that answers a permission
    /// dialog is the only "answered" signal — no hook fires at that moment),
    /// and the renderer learns about it through this same snapshot.
    pub(crate) fn emit_status(app: &AppHandle, registry: &Registry, tab_id: &str) {
        if let Some(tab) = registry.get(tab_id) {
            let _ = app.emit(
                "tab-status",
                serde_json::json!({
                    "tab_id": tab_id,
                    "status": tab.status,
                    "notice": tab.notice,
                    "answer": tab.answer,
                }),
            );
        }
    }

    /// Attach this tab's stream to the renderer: everything currently in the
    /// replay ring, oldest first, plus the stream position the replay ends at.
    /// Later chunks are emitted live as well, each tagged with its position.
    pub fn attach_stream(&self) -> (Vec<u8>, u64) {
        let mut state = lock(&self.stream);
        state.attached = true;
        (state.recent.iter().copied().collect(), state.position)
    }

    /// Stop emitting to the renderer; the replay ring keeps filling regardless.
    /// Called when a tab's view unmounts (React remount, tab closed) so nothing
    /// is sent into a listener that no longer exists.
    pub fn detach(&self) {
        lock(&self.stream).attached = false;
    }

    pub fn write(&self, data: &[u8]) -> Result<(), SessionError> {
        let mut writer = lock(&self.writer);
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), SessionError> {
        let master = lock(&self.master);
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| SessionError::Pty(e.to_string()))?;
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn kill(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(mut killer) = lock(&self.child_killer).take() {
            let _ = killer.kill();
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Kill before deleting the rc files: a live shell can still re-exec
        // itself (`exec zsh`), and sourcing a ZDOTDIR/rcfile that teardown
        // just removed would bring it up without the clitab hooks. Once the
        // child is gone, nothing can observe the files disappearing.
        self.kill();
        self.integration.cleanup();
    }
}

/// Epoch milliseconds, the clock the whole protocol speaks: `Instant` would
/// not survive the IPC boundary or a webview reload.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Ring and forward one segment of a read chunk. The ring is filled whether or
/// not anyone is watching, so a later attach can rebuild the screen. Emitting
/// while still holding the lock is what keeps the byte stream ordered against
/// a concurrent `attach_stream`; chunks (or segments) that were in flight when
/// the renderer re-attached carry a position the replay already covers, and the
/// renderer dedups on it. An attach landing *between* segments of one chunk is
/// the same in-flight case — `position` advances per segment under the lock.
fn emit_segment(app: &AppHandle, tab_id: &str, stream: &Mutex<StreamState>, segment: &[u8]) {
    if segment.is_empty() {
        return;
    }
    let mut state = lock(stream);
    push_recent(&mut state.recent, segment);
    if state.attached {
        emit_output(app, tab_id, segment, state.position);
    }
    state.position += segment.len() as u64;
}

/// Forward raw PTY bytes. They travel base64-encoded because a Tauri event
/// payload is JSON: serialising bytes as a number array costs roughly four
/// times the bandwidth on every chunk of output.
fn emit_output(app: &AppHandle, tab_id: &str, data: &[u8], seq: u64) {
    let _ = app.emit(
        "pty-output",
        serde_json::json!({
            "tab_id": tab_id,
            "data": BASE64.encode(data),
            "seq": seq,
        }),
    );
}

/// Where a new tab starts. A bundled macOS app launched from Finder has `/` as
/// its working directory, which would otherwise open every tab at the root.
pub fn default_cwd() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(home);
        if path.is_dir() {
            return path;
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.is_dir() {
            return cwd;
        }
    }
    PathBuf::from("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_preserves_byte_order() {
        let mut ring = VecDeque::new();
        push_recent(&mut ring, b"abc");
        push_recent(&mut ring, b"\x1b]0;t\x07");
        let bytes: Vec<u8> = ring.into_iter().collect();
        assert_eq!(bytes, b"abc\x1b]0;t\x07");
    }

    #[test]
    fn ring_keeps_the_newest_bytes_when_full() {
        let chunk = vec![0xABu8; REPLAY_LIMIT / 2];
        let mut ring = VecDeque::new();
        for i in 0..6 {
            let mut block = chunk.clone();
            // Tag the tail of every block so we can identify the boundary.
            block[REPLAY_LIMIT / 2 - 1] = i;
            push_recent(&mut ring, &block);
        }

        // Never more than the cap, and what remains is the *newest* tail.
        assert_eq!(ring.len(), REPLAY_LIMIT);
        assert_eq!(ring.back(), Some(&5), "the newest block survives");
        assert_eq!(
            ring.iter().filter(|b| **b == 4).count(),
            1,
            "block 4 should still be there"
        );
        for evicted in 0..4u8 {
            assert_eq!(
                ring.iter().filter(|b| **b == evicted).count(),
                0,
                "block {evicted} should have been evicted"
            );
        }
    }

    /// Eviction must come off the front, never the back: the newest bytes are
    /// the ones a re-attaching terminal needs.
    #[test]
    fn a_full_ring_evicts_only_from_the_front() {
        let mut ring = VecDeque::new();
        push_recent(&mut ring, &vec![7u8; REPLAY_LIMIT]);
        push_recent(&mut ring, b"xy");

        assert_eq!(ring.len(), REPLAY_LIMIT, "the cap must hold exactly");
        assert_eq!(ring.front(), Some(&7u8), "oldest surviving byte");
        assert_eq!(ring.iter().filter(|b| **b == b'x').count(), 1);
        assert_eq!(ring.back(), Some(&b'y'), "newest byte last");
    }
}
