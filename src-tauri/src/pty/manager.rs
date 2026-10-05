use super::lock;
use super::registry::{Registry, TabRecord};
use super::session::{self, PtySession};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::AppHandle;

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
        Self {
            sessions: Mutex::new(HashMap::new()),
            registry: Arc::new(Registry::new()),
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
        // Typing into the tab is the answer: it leaves the triage queue.
        crate::attention::respond(&self.app, &self.registry, tab_id);
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
