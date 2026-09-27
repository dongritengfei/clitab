//! The authoritative list of tabs and their metadata.
//!
//! The PTY reader threads need to keep titles/cwd in sync, so this lives
//! behind its own lock instead of being buried inside `TabManager`: a session
//! only ever touches the registry, never the session map. That keeps lock
//! ordering trivial (session map and registry are never held at the same time)
//! and means `list_tabs` survives a webview reload with the right titles.

use super::lock;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct TabRecord {
    pub id: String,
    pub title: String,
    pub cwd: String,
    /// True when the title was set by a program (e.g. Claude Code) rather than
    /// derived from the working directory.
    pub has_program_title: bool,
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
}
