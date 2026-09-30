//! The authoritative list of tabs and their metadata.
//!
//! The PTY reader threads need to keep titles/cwd in sync, so this lives
//! behind its own lock instead of being buried inside `TabManager`: a session
//! only ever touches the registry, never the session map. That keeps lock
//! ordering trivial (session map and registry are never held at the same time)
//! and means `list_tabs` survives a webview reload with the right titles.

use super::lock;
use crate::status::{Notice, TabStatus};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct TabRecord {
    pub id: String,
    pub title: String,
    pub cwd: String,
    /// True when the title was set by a program (e.g. Claude Code) rather than
    /// derived from the working directory.
    pub has_program_title: bool,
    /// Turn state reported via the OSC 7777 hook protocol; `None` until the
    /// tab's session first speaks it (plain shell tabs never do).
    pub status: Option<TabStatus>,
    /// A Notification-hook message awaiting the user; orthogonal to `status`.
    pub notice: Option<Notice>,
    /// Epoch ms of the current turn's start, consumed by `end_turn`.
    pub turn_start: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Registry {
    tabs: Mutex<Vec<TabRecord>>,
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
            status: None,
            notice: None,
            turn_start: None,
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

    /// UserPromptSubmit: a turn began. Any stale notice is by definition
    /// answered — the user just typed.
    pub fn begin_turn(&self, id: &str, now_ms: u64) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            tab.status = Some(TabStatus::Thinking { since: now_ms });
            tab.turn_start = Some(now_ms);
            tab.notice = None;
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
    /// observed; the start mark is consumed either way. A duplicate Stop
    /// (hooks fire once per settings level) finds no start mark and an
    /// already-`Done` status, and must not clobber the first duration.
    pub fn end_turn(&self, id: &str, now_ms: u64) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            if tab.turn_start.is_none() && matches!(tab.status, Some(TabStatus::Done { .. })) {
                return;
            }
            let duration = tab.turn_start.map(|start| now_ms.saturating_sub(start));
            tab.status = Some(TabStatus::Done {
                duration,
                at: now_ms,
            });
            tab.turn_start = None;
            tab.notice = None;
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

    use crate::status::{Notice, TabStatus};

    fn status_fixture() -> Registry {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        registry
    }

    #[test]
    fn turn_lifecycle_thinking_tool_done() {
        let r = status_fixture();
        r.begin_turn("t1", 1000);
        let tab = r.get("t1").unwrap();
        assert_eq!(tab.status, Some(TabStatus::Thinking { since: 1000 }));
        assert_eq!(tab.turn_start, Some(1000));

        r.set_tool("t1", "Bash", 1500);
        let tab = r.get("t1").unwrap();
        assert_eq!(
            tab.status,
            Some(TabStatus::Tool { name: "Bash".into(), since: 1500 })
        );
        assert_eq!(tab.turn_start, Some(1000), "tool must not restart the clock");

        r.end_turn("t1", 4200);
        let tab = r.get("t1").unwrap();
        assert_eq!(
            tab.status,
            Some(TabStatus::Done { duration: Some(3200), at: 4200 })
        );
        assert_eq!(tab.turn_start, None, "end_turn consumes the start mark");
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
        r.begin_turn("t1", 1000);
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
            |r: &Registry| r.begin_turn("t1", 300),
            |r: &Registry| r.set_tool("t1", "Bash", 300),
            |r: &Registry| r.end_turn("t1", 300),
        ] {
            r.set_notice("t1", Some("stale".into()), 200);
            apply(&r);
            assert_eq!(r.get("t1").unwrap().notice, None);
        }
    }

    #[test]
    fn unknown_tab_status_ops_are_noops() {
        let r = status_fixture();
        r.begin_turn("nope", 1);
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
