use super::lock;
use super::registry::{Registry, TabRecord};
use super::session::{self, PtySession};
use crate::status::{AgentKind, SessionBinding};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error(transparent)]
    Session(#[from] session::SessionError),
    #[error("tab not found: {0}")]
    NotFound(String),
}

/// Owns the PTY sessions. Tab metadata lives in the shared [`Registry`], which
/// the reader threads update directly, so a webview reload can restore titles
/// and working directories instead of resetting them.
pub struct TabManager {
    sessions: Mutex<HashMap<String, Arc<PtySession>>>,
    registry: Arc<Registry>,
    app: AppHandle,
}

impl TabManager {
    pub fn new(app: AppHandle) -> Self {
        // The registry persists the tab snapshot next to `last-cwd` (same
        // config dir). Without one, snapshots silently skip and restore
        // degrades to today's last-cwd startup.
        let registry = match crate::last_cwd::dir(&app) {
            Some(dir) => Registry::with_persist_dir(dir),
            None => Registry::new(),
        };
        Self {
            sessions: Mutex::new(HashMap::new()),
            registry: Arc::new(registry),
            app,
        }
    }

    /// `cwd` is the directory the new tab should start in — typically the
    /// working directory of the tab that was active when the user asked for a
    /// new one. `None` (app startup) or a directory that no longer exists
    /// falls back to the default.
    pub fn create_tab(&self, cwd: Option<String>) -> Result<TabRecord, ManagerError> {
        let tab_id = uuid::Uuid::new_v4().to_string();
        let cwd = resolve_cwd(cwd).to_string_lossy().to_string();

        // Register the tab *before* spawning the shell: the reader thread can
        // emit a title/cwd within milliseconds of the process starting, and
        // that update must not be dropped because the tab was not known yet.
        let record = self.registry.insert(tab_id.clone(), cwd.clone());

        let session = match PtySession::new(
            tab_id.clone(),
            cwd.clone(),
            self.app.clone(),
            Arc::clone(&self.registry),
        ) {
            Ok(session) => Arc::new(session),
            Err(e) => {
                self.registry.remove(&tab_id);
                return Err(e.into());
            }
        };

        lock(&self.sessions).insert(tab_id.clone(), session);
        // A new tab is by definition the one the user is about to look at.
        self.registry.set_active(&tab_id);

        // Opening the tab counts as using its directory: a Finder-service tab
        // becomes the last cwd even if the user never cds, and re-saving the
        // restored startup cwd is a harmless same-value write. Only after a
        // successful spawn — a failed one never became a working directory.
        if let Some(dir) = crate::last_cwd::dir(&self.app) {
            crate::last_cwd::save(&dir, &cwd);
        }

        // Read the record back rather than returning the pre-spawn snapshot: by
        // now the shell may already have reported its title and directory, and
        // the `tab-title` / `tab-cwd` events for those would have been dropped
        // by a renderer that does not know this tab exists yet.
        Ok(self.registry.get(&tab_id).unwrap_or(record))
    }

    /// Close a tab. Dropping the last session handle kills the child process.
    pub fn close_tab(&self, tab_id: &str) -> Result<(), ManagerError> {
        let session = lock(&self.sessions).remove(tab_id);
        let session = session.ok_or_else(|| ManagerError::NotFound(tab_id.to_string()))?;

        session.kill();
        self.registry.remove(tab_id);
        crate::attention::update_badge(&self.app, &self.registry);
        Ok(())
    }

    /// Forget a session whose shell exited by itself (typed `exit`, crash, ...).
    pub fn remove_session(&self, tab_id: &str) {
        lock(&self.sessions).remove(tab_id);
        self.registry.remove(tab_id);
        crate::attention::update_badge(&self.app, &self.registry);
    }

    /// Copy the handle out so the map lock is not held while we use it.
    fn session(&self, tab_id: &str) -> Result<Arc<PtySession>, ManagerError> {
        lock(&self.sessions)
            .get(tab_id)
            .cloned()
            .ok_or_else(|| ManagerError::NotFound(tab_id.to_string()))
    }

    pub fn write_input(&self, tab_id: &str, data: &[u8]) -> Result<(), ManagerError> {
        self.session(tab_id)?.write(data)?;
        // Typing into the tab is the answer: it leaves the triage queue...
        crate::attention::respond(&self.app, &self.registry, tab_id);
        // ...and it has seen any pending notice. Answering a permission
        // dialog fires no hook event (PreToolUse ran before the dialog, and
        // PostToolUse stays silent for non-AskUserQuestion tools), so without
        // this the ⚠ text would linger until the next tool/prompt/stop. The
        // snapshot reveals the turn state underneath, per `Notice`'s contract.
        if self.registry.clear_notice(tab_id) {
            PtySession::emit_status(&self.app, &self.registry, tab_id);
        }
        Ok(())
    }

    pub fn resize_pty(&self, tab_id: &str, rows: u16, cols: u16) -> Result<(), ManagerError> {
        // Ignore nonsense sizes: a hidden terminal can report 0x0 while the
        // window is being resized, which would break line wrapping for good.
        if rows == 0 || cols == 0 {
            return Ok(());
        }
        self.session(tab_id)?.resize(rows, cols)?;
        Ok(())
    }

    /// The tab's replay ring, base64 encoded, the stream position the replay
    /// ends at, and the switch to live streaming.
    pub fn attach_stream(&self, tab_id: &str) -> Result<(String, u64), ManagerError> {
        let (bytes, replay_end) = self.session(tab_id)?.attach_stream();
        Ok((BASE64.encode(bytes), replay_end))
    }

    /// The renderer stopped listening (tab view unmounted): stop emitting.
    pub fn detach(&self, tab_id: &str) {
        if let Ok(session) = self.session(tab_id) {
            session.detach();
        }
    }

    /// True only while the tab still has a live child process. The old version
    /// answered "does this tab exist", which made every close ask for
    /// confirmation.
    pub fn has_active_process(&self, tab_id: &str) -> bool {
        lock(&self.sessions)
            .get(tab_id)
            .map(|session| session.is_running())
            .unwrap_or(false)
    }

    /// The renderer acknowledges a tab's notice by switching to it. Unknown
    /// tabs are a silent no-op: the notice died with the tab.
    pub fn ack_notice(&self, tab_id: &str) {
        self.registry.clear_notice(tab_id);
    }

    pub fn list_tabs(&self) -> Vec<TabRecord> {
        self.registry.list()
    }

    /// Drag-reorder: move a tab to a new position in the registry's order.
    pub fn reorder_tab(&self, tab_id: &str, to_index: usize) -> Result<(), ManagerError> {
        if self.registry.reorder(tab_id, to_index) {
            Ok(())
        } else {
            Err(ManagerError::NotFound(tab_id.to_string()))
        }
    }

    /// The renderer switched tabs: remember which one the user is looking at
    /// (the snapshot's active index). Unknown tabs are ignored — a stale
    /// report is harmless, same contract as `ack_notice`.
    pub fn set_active(&self, tab_id: &str) {
        if self.registry.get(tab_id).is_some() {
            self.registry.set_active(tab_id);
        }
    }

    pub fn active_id(&self) -> Option<String> {
        self.registry.active_id()
    }

    pub fn record(&self, tab_id: &str) -> Option<TabRecord> {
        self.registry.get(tab_id)
    }

    /// Recreate the previous run's sidebar (spec Q3/Q6): same tabs, same
    /// order, same titles. The previously-active tab's agent session resumes
    /// right away (spec Q4 hybrid: active auto, the rest lazy on first
    /// activation). Returns the id of the tab to focus, or `None` when there
    /// is nothing to restore (no snapshot / empty / every spawn failed) —
    /// the caller then falls back to the last-cwd startup.
    pub fn restore_snapshot(&self) -> Option<String> {
        let dir = crate::last_cwd::dir(&self.app)?;
        let snapshot = crate::restore::load(&dir)?;
        if snapshot.tabs.is_empty() {
            return None;
        }

        let mut created: Vec<TabRecord> = Vec::with_capacity(snapshot.tabs.len());
        for tab in snapshot.tabs {
            match self.restore_tab(tab) {
                Ok(record) => created.push(record),
                // One unspawnable tab must not sink the whole restore.
                Err(e) => eprintln!("clitab: could not restore a tab: {e}"),
            }
        }
        if created.is_empty() {
            return None;
        }

        // If some spawns failed, indices shifted — clamping is the best
        // available guess at "the tab they were looking at".
        let active_index = snapshot.active.unwrap_or(0).min(created.len() - 1);
        let active_id = created[active_index].id.clone();
        self.registry.set_active(&active_id);
        // Resume BEFORE announcing: the `tab-created` payload must carry the
        // post-resume flags (`pendingResume` already false), so the renderer
        // does not try to resume the active tab a second time.
        if let Err(e) = self.resume_tab(&active_id) {
            eprintln!("clitab: could not resume the active tab's session: {e}");
        }

        {
            for (index, record) in created.iter().enumerate() {
                // Re-read each record: the resume and any early shell
                // integration output are already reflected in the payload.
                let current = self.registry.get(&record.id).unwrap_or_else(|| record.clone());
                let _ = self.app.emit(
                    "tab-created",
                    crate::TabResponse::from_record(current, index == active_index),
                );
            }
        }
        Some(active_id)
    }

    /// One restored tab: a fresh UUID and PTY, metadata from the snapshot.
    /// A resumable tab keeps its program title until the resume proves
    /// impossible (`cancel_resume` reverts it); a tab whose agent had already
    /// exited comes back as the plain shell it is, with its cwd title.
    fn restore_tab(
        &self,
        snapshot: crate::restore::SnapshotTab,
    ) -> Result<TabRecord, ManagerError> {
        let tab_id = uuid::Uuid::new_v4().to_string();
        let cwd = resolve_cwd(Some(snapshot.cwd)).to_string_lossy().to_string();
        // Restorable whenever the snapshot says the agent was alive: with a
        // binding it resumes the exact session, with only a launch signal it
        // relaunches the plain binary (`resume_line`'s tiers). The renderer's
        // history load still needs a binding, hence the separate flag.
        let resumable = snapshot.agent_running;
        let restored_session = resumable && snapshot.session.is_some();
        let keep_program_title = snapshot.has_program_title && resumable;
        let title = if keep_program_title { snapshot.title } else { cwd.clone() };

        let record = TabRecord {
            id: tab_id.clone(),
            title,
            cwd: cwd.clone(),
            has_program_title: keep_program_title,
            waiting: false,
            status: None,
            notice: None,
            answer: None,
            turn_start: None,
            prompt_queue: Default::default(),
            last_stop_ms: None,
            session: snapshot.session,
            // Restore does not claim the agent is running — it is not, yet.
            // The resume injection (`begin_resume`) sets the flag.
            agent_running: false,
            last_agent: snapshot.agent,
            resume_pending: resumable,
            restored_session,
        };
        let record = self.registry.insert_record(record);

        let session = match PtySession::new(
            tab_id.clone(),
            cwd,
            self.app.clone(),
            Arc::clone(&self.registry),
        ) {
            Ok(session) => Arc::new(session),
            Err(e) => {
                self.registry.remove(&tab_id);
                return Err(e.into());
            }
        };
        lock(&self.sessions).insert(tab_id.clone(), session);
        // Deliberately NO last_cwd save here: restored tabs re-open the
        // previous run's directories, they are not a fresh user choice —
        // the last-cwd file already reflects the last real one.
        Ok(record)
    }

    /// The line `resume_tab` injects, or `None` when the tab should stay a
    /// plain shell (spec D4's cancel path). Tiers: an exact session (live
    /// transcript, and no *newer* launch of a different agent) resumes via
    /// `--resume`; otherwise the agent is relaunched plain when its kind is
    /// known — from the shell's launch signal or, failing that, the binding
    /// whose transcript is gone. The sid's charset was validated at decode
    /// time (`decode_session`), so interpolating it is safe; `cmd()` is a
    /// static string.
    fn resume_line(
        session: Option<&SessionBinding>,
        has_transcript: bool,
        last_agent: Option<AgentKind>,
    ) -> Option<String> {
        match session {
            Some(b) if has_transcript && last_agent.map_or(true, |k| k == b.agent) => {
                Some(format!("{} --resume {}\n", b.agent.cmd(), b.sid))
            }
            _ => last_agent
                .or(session.map(|b| b.agent))
                .map(|kind| format!("{}\n", kind.cmd())),
        }
    }

    /// Issue the tab's pending resume: inject `resume_line`'s pick into its
    /// shell as ordinary input (spec D1 — keeps OSC 7 cwd reporting, title
    /// revert and the preexec machinery working; no `--fork-session`, so the
    /// transcript keeps growing under the same id). Degrades silently to a
    /// plain shell (spec D4) when no resume is pending or neither a binding
    /// nor a launch signal exists; a deleted transcript relaunches the plain
    /// agent instead of stranding the shell. Only a vanished TAB is NotFound.
    pub fn resume_tab(&self, tab_id: &str) -> Result<(), ManagerError> {
        let record = self
            .registry
            .get(tab_id)
            .ok_or_else(|| ManagerError::NotFound(tab_id.to_string()))?;
        if !record.resume_pending {
            return Ok(());
        }
        let has_transcript = record
            .session
            .as_ref()
            .and_then(|binding| crate::history::transcript_for(binding, &record.cwd))
            .map(|path| path.is_file())
            .unwrap_or(false);
        let Some(line) =
            Self::resume_line(record.session.as_ref(), has_transcript, record.last_agent)
        else {
            self.registry.cancel_resume(tab_id);
            Self::emit_cancelled_title(&self.app, self.registry.as_ref(), tab_id);
            return Ok(());
        };
        // `--resume` fires no hook (and a plain relaunch fires none until
        // the first prompt either): the registry must learn "agent alive"
        // from here, or an immediate quit would break the restore chain.
        // The flip is the single serialization point for the injection:
        // the PTY write happens only when this caller won it, so the
        // startup-race second caller cannot land a second `<agent>
        // --resume <sid>` in the just-launched agent's stdin as a
        // submitted prompt.
        if !self.registry.begin_resume(tab_id) {
            return Ok(());
        }
        self.session(tab_id)?.write(line.as_bytes())?;
        Ok(())
    }

    /// Emit a `tab-title` event for a tab whose resume was cancelled, so the
    /// renderer replaces the dead session's program title with the reverted
    /// cwd title. Mirrors the exact payload shape of the in-session.rs
    /// emitter — the renderer has a single shape to pattern-match.
    fn emit_cancelled_title(app: &AppHandle, registry: &Registry, tab_id: &str) {
        let Some(record) = registry.get(tab_id) else {
            return;
        };
        let _ = app.emit(
            "tab-title",
            serde_json::json!({
                "tab_id": tab_id,
                "title": record.title,
                "program_title": record.has_program_title,
            }),
        );
    }
}

/// Where a new tab starts: the requested directory if it still exists (the
/// source tab's cwd can be deleted out from under us), otherwise the default.
fn resolve_cwd(requested: Option<String>) -> PathBuf {
    if let Some(path) = requested {
        let path = PathBuf::from(path);
        if path.is_dir() {
            return path;
        }
    }
    session::default_cwd()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{AgentKind, SessionBinding};

    fn binding(agent: AgentKind, sid: &str) -> SessionBinding {
        SessionBinding { agent, sid: sid.into(), transcript: None }
    }

    /// Tier 1: a live binding whose agent matches the last launch resumes
    /// the exact session — the shipped restore behavior, unchanged.
    #[test]
    fn resume_line_prefers_a_live_transcript() {
        let b = binding(AgentKind::Claude, "s-1");
        assert_eq!(
            TabManager::resume_line(Some(&b), true, Some(AgentKind::Claude)),
            Some("claude --resume s-1\n".into())
        );
        // No launch signal at all (hook-only flow, or an older snapshot):
        // the binding alone still resumes.
        assert_eq!(
            TabManager::resume_line(Some(&b), true, None),
            Some("claude --resume s-1\n".into())
        );
    }

    /// Tier 2: plain relaunch when the exact session cannot be resumed —
    /// the never-prompted agent (no binding ever existed), the deleted
    /// transcript, or a binding left stale by a newer launch of the other
    /// agent.
    #[test]
    fn resume_line_falls_back_to_a_plain_relaunch() {
        // Never-prompted claude: only the shell's launch signal exists.
        assert_eq!(
            TabManager::resume_line(None, false, Some(AgentKind::Claude)),
            Some("claude\n".into())
        );
        // Transcript deleted between quit and relaunch: relaunch the agent
        // instead of stranding a plain shell (the binding names the kind).
        let b = binding(AgentKind::Qoder, "s-1");
        assert_eq!(
            TabManager::resume_line(Some(&b), false, None),
            Some("qoder\n".into())
        );
        // Stale binding: qoder session on record, but claude was the last
        // launch (it fired no hook) — resuming qoder would be the wrong
        // agent, so relaunch claude.
        assert_eq!(
            TabManager::resume_line(Some(&b), true, Some(AgentKind::Claude)),
            Some("claude\n".into())
        );
    }

    /// Tier 3: nothing to resume and no agent kind to relaunch → None, the
    /// cancel path (plain shell, title reverts).
    #[test]
    fn resume_line_without_any_agent_is_none() {
        assert_eq!(TabManager::resume_line(None, false, None), None);
    }

    #[test]
    fn requested_directory_wins_when_it_exists() {
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        assert_eq!(resolve_cwd(Some(dir.clone())), PathBuf::from(dir));
    }

    #[test]
    fn missing_or_absent_request_falls_back_to_default() {
        assert_eq!(
            resolve_cwd(Some("/clitab-no-such-dir".into())),
            session::default_cwd()
        );
        assert_eq!(resolve_cwd(None), session::default_cwd());
    }
}
