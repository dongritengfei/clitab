//! The authoritative list of tabs and their metadata.
//!
//! The PTY reader threads need to keep titles/cwd in sync, so this lives
//! behind its own lock instead of being buried inside `TabManager`: a session
//! only ever touches the registry, never the session map. That keeps lock
//! ordering trivial (session map and registry are never held at the same time)
//! and means `list_tabs` survives a webview reload with the right titles.

use super::lock;
use crate::status::{Answer, Notice, TabStatus};
use std::collections::VecDeque;
use std::sync::Mutex;

/// PTY silence that marks a "running" turn as stale. An Esc interrupt fires no
/// Stop hook (verified against a live PTY capture), so afterwards the record
/// still looks mid-turn while Claude actually sits at an idle prompt. A live
/// turn repaints its spinner several times a second, so its output gaps stay
/// far below this; a prompt arriving after this much silence starts a fresh
/// turn and drops the stale queue.
const IDLE_GAP_MS: u64 = 3000;

/// A Stop echoing the previous one within this window is the same hook firing
/// from two settings levels, not a turn that ended — ignore it, or it would
/// consume the auto-started turn and pop the queue a second time. A real turn
/// cannot begin and end this fast (even a trivial queued answer needs an API
/// round trip, observed ≥ ~600 ms).
const DUPLICATE_STOP_MS: u64 = 500;

#[derive(Debug, Clone)]
pub struct TabRecord {
    pub id: String,
    pub title: String,
    pub cwd: String,
    /// True when the title was set by a program (e.g. Claude Code) rather than
    /// derived from the working directory.
    pub has_program_title: bool,
    /// True while the tab is waiting for user input — the triage queue.
    /// Set at every `tab-flash` trigger; cleared only when the user types
    /// into the tab (`pty_input`). Switching tabs does NOT clear it.
    pub waiting: bool,
    /// Turn state reported via the OSC 7777 hook protocol; `None` until the
    /// tab's session first speaks it (plain shell tabs never do).
    pub status: Option<TabStatus>,
    /// A Notification-hook message awaiting the user; orthogonal to `status`.
    pub notice: Option<Notice>,
    /// The user's latest answer to an in-terminal question; a point-in-time
    /// record (see `status::Answer`), never cleared.
    pub answer: Option<Answer>,
    /// Epoch ms of the current turn's start, consumed by `end_turn`.
    pub turn_start: Option<u64>,
    /// Prompts submitted while a turn was in flight, oldest first. Claude
    /// Code auto-submits the head at the stop without re-firing the hook, so
    /// `end_turn` pops it and the caller models the submission
    /// (`begin_auto_turn`).
    pub prompt_queue: VecDeque<Option<String>>,
    /// Epoch ms of the last accepted `end_turn`; anchors the duplicate-stop
    /// burst guard.
    pub last_stop_ms: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Registry {
    tabs: Mutex<Vec<TabRecord>>,
}

/// What a Stop hook means for the caller (session.rs): `Idle` is the cue to
/// flash and enter the waiting queue; `AutoSubmit` carries the queued prompt
/// Claude Code is submitting right now (text is None when the hook had no jq
/// to extract it) — no flash, the caller models the turn via
/// `begin_auto_turn`; `Ignored` (duplicate, unknown tab) changes nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum StopOutcome {
    Ignored,
    Idle,
    AutoSubmit(Option<String>),
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fresh tab whose title starts out as its working directory.
    pub fn insert(&self, id: String, cwd: String) -> TabRecord {
        let record = TabRecord {
            id: id.clone(),
            title: cwd.clone(),
            cwd,
            has_program_title: false,
            waiting: false,
            status: None,
            notice: None,
            answer: None,
            turn_start: None,
            prompt_queue: VecDeque::new(),
            last_stop_ms: None,
        };
        lock(&self.tabs).push(record.clone());
        record
    }

    pub fn remove(&self, id: &str) -> bool {
        let mut tabs = lock(&self.tabs);
        let before = tabs.len();
        tabs.retain(|t| t.id != id);
        tabs.len() != before
    }

    /// Current metadata for `id`, as the reader threads last left it.
    pub fn get(&self, id: &str) -> Option<TabRecord> {
        lock(&self.tabs).iter().find(|t| t.id == id).cloned()
    }

    /// A program (shell prompt, TUI, `echo -e`) set the terminal title.
    pub fn set_title(&self, id: &str, title: &str, is_program_title: bool) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            tab.title = title.to_string();
            tab.has_program_title = is_program_title;
        }
    }

    /// OSC 7 reported a new working directory.
    pub fn set_cwd(&self, id: &str, cwd: &str) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            tab.cwd = cwd.to_string();
            if !tab.has_program_title {
                tab.title = cwd.to_string();
            }
        }
    }

    /// Forget a program title and fall back to the working directory.
    pub fn clear_program_title(&self, id: &str) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            tab.has_program_title = false;
            tab.title = tab.cwd.clone();
        }
    }

    /// Mark a tab as waiting for input. Returns true only on the
    /// false→true transition, so repeated signals (BEL spam, the idle
    /// watcher re-firing) notify exactly once per queue entry.
    pub fn set_waiting(&self, id: &str) -> bool {
        let mut tabs = lock(&self.tabs);
        match tabs.iter_mut().find(|t| t.id == id) {
            Some(tab) if !tab.waiting => {
                tab.waiting = true;
                true
            }
            _ => false,
        }
    }

    /// Take a tab out of the waiting queue (the user typed in it). Returns
    /// true only on the true→false transition.
    pub fn clear_waiting(&self, id: &str) -> bool {
        let mut tabs = lock(&self.tabs);
        match tabs.iter_mut().find(|t| t.id == id) {
            Some(tab) if tab.waiting => {
                tab.waiting = false;
                true
            }
            _ => false,
        }
    }

    /// How many tabs are in the triage queue; drives the Dock badge.
    pub fn waiting_count(&self) -> usize {
        lock(&self.tabs).iter().filter(|t| t.waiting).count()
    }

    /// UserPromptSubmit: a turn began — or, when one is already in flight, a
    /// prompt was queued (`idle_gap_ms` is the PTY silence before the chunk
    /// carrying the hook; see `IDLE_GAP_MS`). A queued prompt must NOT restart
    /// `turn_start`: the running turn's duration ends at its own stop. Any
    /// stale notice is by definition answered — the user just typed.
    pub fn begin_turn(&self, id: &str, now_ms: u64, msg: Option<String>, idle_gap_ms: u64) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            let running = tab.turn_start.is_some() && idle_gap_ms < IDLE_GAP_MS;
            if running {
                tab.prompt_queue.push_back(msg.clone());
            } else {
                // Fresh turn — also the self-heal after an Esc interrupt
                // (which fires no Stop): a gap this large means Claude is
                // genuinely idle, so the stale start mark is replaced and any
                // stale queue dropped. (An interrupt with a non-empty queue
                // never lands here: Claude auto-submits the head immediately,
                // its output keeps the gap small, and the next prompt
                // classifies as queued — which is what it is.)
                tab.prompt_queue.clear();
                tab.turn_start = Some(now_ms);
            }
            tab.status = Some(TabStatus::Thinking { since: now_ms, msg, auto: false });
            tab.notice = None;
        }
    }

    /// The stop popped Claude's auto-submit: model it. `since` must be the
    /// stop's own timestamp so the renderer sees `done.at == auto.since`.
    pub fn begin_auto_turn(&self, id: &str, now_ms: u64, msg: Option<String>) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            tab.status = Some(TabStatus::Thinking { since: now_ms, msg, auto: true });
            tab.turn_start = Some(now_ms);
        }
    }

    /// PreToolUse: a tool is running. When hooks are only partially installed
    /// (no UserPromptSubmit), the first tool starts the clock so `end_turn`
    /// can still report a duration.
    pub fn set_tool(&self, id: &str, name: &str, now_ms: u64) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            tab.status = Some(TabStatus::Tool {
                name: name.to_string(),
                since: now_ms,
            });
            if tab.turn_start.is_none() {
                tab.turn_start = Some(now_ms);
            }
            tab.notice = None;
        }
    }

    /// Stop: the turn ended. Duration is None when no start was ever
    /// observed; the start mark is consumed either way.
    ///
    /// A duplicate Stop (hooks fire once per settings level) must not
    /// clobber the first duration, pop the queue twice, nor re-trigger the
    /// caller's flash/waiting side effects — it reports `Ignored`, as does
    /// an unknown tab. The idle-duplicate is caught by the missing start
    /// mark + already-`Done` status, the burst (a second level's hook
    /// milliseconds later, now racing an auto-started turn) by
    /// `DUPLICATE_STOP_MS`.
    pub fn end_turn(&self, id: &str, now_ms: u64) -> StopOutcome {
        let mut tabs = lock(&self.tabs);
        let tab = match tabs.iter_mut().find(|t| t.id == id) {
            Some(tab) => tab,
            None => return StopOutcome::Ignored,
        };
        if let Some(last) = tab.last_stop_ms {
            if now_ms.saturating_sub(last) < DUPLICATE_STOP_MS {
                return StopOutcome::Ignored;
            }
        }
        if tab.turn_start.is_none() && matches!(tab.status, Some(TabStatus::Done { .. })) {
            return StopOutcome::Ignored;
        }
        let duration = tab.turn_start.map(|start| now_ms.saturating_sub(start));
        tab.status = Some(TabStatus::Done {
            duration,
            at: now_ms,
        });
        tab.turn_start = None;
        tab.notice = None;
        tab.last_stop_ms = Some(now_ms);
        match tab.prompt_queue.pop_front() {
            Some(msg) => StopOutcome::AutoSubmit(msg),
            None => StopOutcome::Idle,
        }
    }

    /// Notification: park a message for the user without touching the turn
    /// state underneath.
    pub fn set_notice(&self, id: &str, msg: Option<String>, now_ms: u64) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            tab.notice = Some(Notice { msg, at: now_ms });
        }
    }

    /// Record the user's answer to an in-terminal question.
    pub fn set_answer(&self, id: &str, msg: String, now_ms: u64) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            tab.answer = Some(Answer { msg, at: now_ms });
        }
    }

    /// The user switched to the tab and saw the notice.
    pub fn clear_notice(&self, id: &str) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            tab.notice = None;
        }
    }

    pub fn list(&self) -> Vec<TabRecord> {
        lock(&self.tabs).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_starts_with_cwd_title() {
        let registry = Registry::new();
        let record = registry.insert("t1".into(), "/tmp".into());
        assert_eq!(record.title, "/tmp");
        assert!(!record.has_program_title);
    }

    #[test]
    fn program_title_survives_cwd_change() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        registry.set_title("t1", "Claude Code", true);
        registry.set_cwd("t1", "/tmp/project");

        let tab = registry.get("t1").unwrap();
        assert_eq!(tab.title, "Claude Code");
        assert_eq!(tab.cwd, "/tmp/project");
    }

    #[test]
    fn plain_title_follows_cwd() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        registry.set_cwd("t1", "/tmp/project");
        assert_eq!(registry.get("t1").unwrap().title, "/tmp/project");
    }

    #[test]
    fn prompt_ready_restores_cwd_title() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        registry.set_title("t1", "Claude Code", true);
        registry.clear_program_title("t1");

        let tab = registry.get("t1").unwrap();
        assert!(!tab.has_program_title);
        assert_eq!(tab.title, "/tmp");
    }

    #[test]
    fn unknown_tab_is_ignored() {
        let registry = Registry::new();
        registry.set_title("nope", "x", true);
        registry.set_cwd("nope", "/x");
        registry.clear_program_title("nope");
        assert!(!registry.remove("nope"));
        assert!(registry.list().is_empty());
    }

    #[test]
    fn waiting_transitions_fire_only_once() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        assert!(registry.set_waiting("t1"));
        assert!(!registry.set_waiting("t1"), "second signal is not a transition");
        assert_eq!(registry.waiting_count(), 1);
        assert!(registry.clear_waiting("t1"));
        assert!(!registry.clear_waiting("t1"), "already out of the queue");
        assert_eq!(registry.waiting_count(), 0);
        assert!(
            registry.set_waiting("t1"),
            "responding and ringing again re-enters the queue"
        );
    }

    #[test]
    fn removed_tabs_leave_the_queue() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/a".into());
        registry.insert("t2".into(), "/b".into());
        registry.set_waiting("t1");
        registry.set_waiting("t2");
        registry.remove("t1");
        assert_eq!(registry.waiting_count(), 1);
    }

    #[test]
    fn unknown_tab_waiting_is_ignored() {
        let registry = Registry::new();
        assert!(!registry.set_waiting("nope"));
        assert!(!registry.clear_waiting("nope"));
        assert_eq!(registry.waiting_count(), 0);
    }

    #[test]
    fn list_keeps_insertion_order() {
        let registry = Registry::new();
        registry.insert("a".into(), "/a".into());
        registry.insert("b".into(), "/b".into());
        registry.remove("a");
        registry.insert("c".into(), "/c".into());
        assert_eq!(
            registry
                .list()
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "c"]
        );
    }

    use crate::status::{Answer, Notice, TabStatus};

    fn status_fixture() -> Registry {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        registry
    }

    #[test]
    fn turn_lifecycle_thinking_tool_done() {
        let r = status_fixture();
        r.begin_turn("t1", 1000, Some("fix the bug".into()), 0);
        let tab = r.get("t1").unwrap();
        assert_eq!(
            tab.status,
            Some(TabStatus::Thinking { since: 1000, msg: Some("fix the bug".into()), auto: false })
        );
        assert_eq!(tab.turn_start, Some(1000));

        r.set_tool("t1", "Bash", 1500);
        let tab = r.get("t1").unwrap();
        assert_eq!(
            tab.status,
            Some(TabStatus::Tool { name: "Bash".into(), since: 1500 })
        );
        assert_eq!(tab.turn_start, Some(1000), "tool must not restart the clock");

        assert_eq!(r.end_turn("t1", 4200), StopOutcome::Idle, "nothing queued to auto-submit");
        let tab = r.get("t1").unwrap();
        assert_eq!(
            tab.status,
            Some(TabStatus::Done { duration: Some(3200), at: 4200 })
        );
        assert_eq!(tab.turn_start, None, "end_turn consumes the start mark");
    }

    /// A prompt submitted mid-turn is queued by Claude Code: its hook fires at
    /// queue time, but the running turn keeps its own clock (the duration must
    /// cover the whole turn, not start over at every queued prompt).
    #[test]
    fn queued_prompt_keeps_the_running_turns_clock() {
        let r = status_fixture();
        r.begin_turn("t1", 1000, Some("A".into()), 0);
        r.begin_turn("t1", 5000, Some("B".into()), 100);
        let tab = r.get("t1").unwrap();
        assert_eq!(
            tab.status,
            Some(TabStatus::Thinking { since: 5000, msg: Some("B".into()), auto: false })
        );
        assert_eq!(tab.turn_start, Some(1000), "queued prompt must not restart the clock");
        assert_eq!(tab.prompt_queue.len(), 1);
    }

    /// Stop with a non-empty queue: Claude Code auto-submits the head without
    /// re-firing the hook, so end_turn hands it back and the caller models the
    /// submission (begin_auto_turn) after emitting the Done snapshot.
    #[test]
    fn stop_with_queue_hands_back_the_auto_submit() {
        let r = status_fixture();
        r.begin_turn("t1", 1000, Some("A".into()), 0);
        r.begin_turn("t1", 5000, Some("B".into()), 100);
        assert_eq!(r.end_turn("t1", 8000), StopOutcome::AutoSubmit(Some("B".into())));
        assert_eq!(
            r.get("t1").unwrap().status,
            Some(TabStatus::Done { duration: Some(7000), at: 8000 }),
            "duration covers the whole A turn"
        );

        r.begin_auto_turn("t1", 8000, Some("B".into()));
        let tab = r.get("t1").unwrap();
        assert_eq!(
            tab.status,
            Some(TabStatus::Thinking { since: 8000, msg: Some("B".into()), auto: true })
        );
        assert_eq!(tab.turn_start, Some(8000));

        assert_eq!(r.end_turn("t1", 12000), StopOutcome::Idle);
        assert_eq!(
            r.get("t1").unwrap().status,
            Some(TabStatus::Done { duration: Some(4000), at: 12000 }),
            "the auto-started turn gets its own duration and stop"
        );
    }

    /// The interrupt self-heal: Esc fires no Stop, so a stale "running" turn
    /// must not swallow the next prompt as queued. PTY silence before the
    /// prompt's chunk (idle_gap_ms) is the classifier — a live turn repaints
    /// its spinner several times a second and never goes silent this long.
    #[test]
    fn idle_gap_makes_a_prompt_fresh_and_drops_the_queue() {
        let r = status_fixture();
        r.begin_turn("t1", 1000, Some("A".into()), 0);
        r.begin_turn("t1", 2000, Some("B".into()), 100); // queued behind A
        // A was interrupted; 5s of silence later the user submits C.
        r.begin_turn("t1", 9000, Some("C".into()), 5000);
        let tab = r.get("t1").unwrap();
        assert_eq!(tab.turn_start, Some(9000), "fresh turn, stale clock replaced");
        assert!(tab.prompt_queue.is_empty(), "stale queue dropped");
        assert_eq!(r.end_turn("t1", 10000), StopOutcome::Idle);
        assert_eq!(
            r.get("t1").unwrap().status,
            Some(TabStatus::Done { duration: Some(1000), at: 10000 })
        );
    }

    /// Hooks merged across settings levels fire back-to-back. The burst must
    /// not consume the auto-started turn nor pop the queue a second time.
    #[test]
    fn burst_duplicate_stop_spares_the_auto_turn() {
        let r = status_fixture();
        r.begin_turn("t1", 1000, Some("A".into()), 0);
        r.begin_turn("t1", 2000, Some("B".into()), 100);
        assert_eq!(r.end_turn("t1", 5000), StopOutcome::AutoSubmit(Some("B".into())));
        r.begin_auto_turn("t1", 5000, Some("B".into()));
        assert_eq!(r.end_turn("t1", 5100), StopOutcome::Ignored, "burst duplicate ignored");
        assert_eq!(
            r.get("t1").unwrap().status,
            Some(TabStatus::Thinking { since: 5000, msg: Some("B".into()), auto: true }),
            "the auto turn is still running"
        );
        assert_eq!(r.end_turn("t1", 9000), StopOutcome::Idle, "its real stop, queue empty");
        assert_eq!(
            r.get("t1").unwrap().status,
            Some(TabStatus::Done { duration: Some(4000), at: 9000 })
        );
    }

    /// The caller flashes for an accepted stop with an empty queue but must
    /// stay silent for a duplicate (session.rs): a burst duplicate landing
    /// right after a queued-prompt stop would otherwise re-enter the waiting
    /// queue — badge, notification, flash — while the auto turn is running.
    /// So the two outcomes have to be tellable apart: `Ignored` vs `Idle`.
    #[test]
    fn accepted_stop_with_empty_queue_differs_from_a_duplicate() {
        let r = status_fixture();
        r.begin_turn("t1", 1000, None, 0);
        assert_eq!(
            r.end_turn("t1", 2000),
            StopOutcome::Idle,
            "accepted stop, nothing queued: the caller flashes"
        );
        assert_eq!(
            r.end_turn("t1", 2100),
            StopOutcome::Ignored,
            "burst duplicate: the caller stays silent"
        );
        assert_eq!(
            r.end_turn("nope", 2200),
            StopOutcome::Ignored,
            "unknown tab: nothing to flash"
        );
    }

    /// Hooks may be only partially installed: without UserPromptSubmit the
    /// first tool starts the clock, so Stop can still report a duration.
    #[test]
    fn tool_without_prompt_starts_the_clock() {
        let r = status_fixture();
        r.set_tool("t1", "Read", 500);
        r.end_turn("t1", 900);
        assert_eq!(
            r.get("t1").unwrap().status,
            Some(TabStatus::Done { duration: Some(400), at: 900 })
        );
    }

    #[test]
    fn stop_without_any_start_has_no_duration() {
        let r = status_fixture();
        r.end_turn("t1", 900);
        assert_eq!(
            r.get("t1").unwrap().status,
            Some(TabStatus::Done { duration: None, at: 900 })
        );
    }

    /// Claude Code merges hooks across settings levels, so one turn can
    /// receive two Stop events. The second must not erase the duration the
    /// first one computed.
    #[test]
    fn duplicate_stop_keeps_first_duration() {
        let r = status_fixture();
        r.begin_turn("t1", 1000, None, 0);
        r.end_turn("t1", 4200);
        r.end_turn("t1", 4300);
        assert_eq!(
            r.get("t1").unwrap().status,
            Some(TabStatus::Done { duration: Some(3200), at: 4200 })
        );
    }

    #[test]
    fn notice_is_orthogonal_to_status() {
        let r = status_fixture();
        r.set_tool("t1", "Bash", 100);
        r.set_notice("t1", Some("needs permission".into()), 200);
        let tab = r.get("t1").unwrap();
        assert_eq!(
            tab.status,
            Some(TabStatus::Tool { name: "Bash".into(), since: 100 }),
            "a notice must not clobber the turn state"
        );
        assert_eq!(
            tab.notice,
            Some(Notice { msg: Some("needs permission".into()), at: 200 })
        );

        r.clear_notice("t1");
        let tab = r.get("t1").unwrap();
        assert_eq!(tab.notice, None);
        assert!(matches!(tab.status, Some(TabStatus::Tool { .. })));
    }

    #[test]
    fn turn_events_clear_the_notice() {
        let r = status_fixture();
        for apply in [
            |r: &Registry| r.begin_turn("t1", 300, None, 0),
            |r: &Registry| r.set_tool("t1", "Bash", 300),
            // end_turn now returns the queued prompt; discard it here.
            |r: &Registry| { r.end_turn("t1", 300); },
        ] {
            r.set_notice("t1", Some("stale".into()), 200);
            apply(&r);
            assert_eq!(r.get("t1").unwrap().notice, None);
        }
    }

    #[test]
    fn set_answer_records_the_users_choice() {
        let r = status_fixture();
        r.set_answer("t1", "Option B".into(), 1200);
        let tab = r.get("t1").unwrap();
        assert_eq!(tab.answer, Some(Answer { msg: "Option B".into(), at: 1200 }));
        // A recorded answer is history, not pending state: a new turn must
        // not wipe it (the renderer dedups answers by `at`).
        r.begin_turn("t1", 1300, None, 0);
        assert_eq!(
            r.get("t1").unwrap().answer,
            Some(Answer { msg: "Option B".into(), at: 1200 })
        );
    }

    #[test]
    fn unknown_tab_status_ops_are_noops() {
        let r = status_fixture();
        r.begin_turn("nope", 1, None, 0);
        r.set_tool("nope", "Bash", 1);
        r.end_turn("nope", 1);
        r.set_notice("nope", None, 1);
        r.clear_notice("nope");
        let tab = r.get("t1").unwrap();
        assert_eq!(tab.status, None);
        assert_eq!(tab.notice, None);
    }

    #[test]
    fn fresh_tab_has_no_protocol_state() {
        let r = status_fixture();
        let tab = r.get("t1").unwrap();
        assert_eq!(tab.status, None);
        assert_eq!(tab.notice, None);
        assert_eq!(tab.turn_start, None);
    }
}
