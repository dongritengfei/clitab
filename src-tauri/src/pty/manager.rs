use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use super::session::{PtySession, SessionError};

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("Session error: {0}")]
    Session(#[from] SessionError),
    #[error("Tab not found: {0}")]
    NotFound(String),
    #[error("Lock error: {0}")]
    Lock(String),
}

pub struct TabInfo {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub flashing: bool,
}

struct TabState {
    had_output: bool,  // Whether PTY produced output since last user input
}

pub struct TabManager {
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
    tabs: Arc<Mutex<Vec<TabInfo>>>,
    states: Arc<Mutex<HashMap<String, TabState>>>,
    app: AppHandle,
}

impl TabManager {
    pub fn new(app: AppHandle) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            tabs: Arc::new(Mutex::new(Vec::new())),
            states: Arc::new(Mutex::new(HashMap::new())),
            app,
        }
    }

    pub fn create_tab(&self) -> Result<TabInfo, ManagerError> {
        let tab_id = uuid::Uuid::new_v4().to_string();

        let session = PtySession::new(tab_id.clone(), self.app.clone())?;
        let cwd = session.cwd.clone();

        let tab_info = TabInfo {
            id: tab_id.clone(),
            title: cwd.clone(),
            cwd,
            flashing: false,
        };

        let mut sessions = self.sessions.lock().map_err(|e| ManagerError::Lock(e.to_string()))?;
        sessions.insert(tab_id.clone(), session);

        let mut states = self.states.lock().map_err(|e| ManagerError::Lock(e.to_string()))?;
        states.insert(tab_id.clone(), TabState { had_output: false });

        let mut tabs = self.tabs.lock().map_err(|e| ManagerError::Lock(e.to_string()))?;
        tabs.push(TabInfo {
            id: tab_info.id.clone(),
            title: tab_info.title.clone(),
            cwd: tab_info.cwd.clone(),
            flashing: false,
        });

        Ok(tab_info)
    }

    pub fn close_tab(&self, tab_id: &str) -> Result<bool, ManagerError> {
        let mut sessions = self.sessions.lock().map_err(|e| ManagerError::Lock(e.to_string()))?;

        if let Some(session) = sessions.remove(tab_id) {
            session.kill();

            let mut tabs = self.tabs.lock().map_err(|e| ManagerError::Lock(e.to_string()))?;
            tabs.retain(|t| t.id != tab_id);

            let mut states = self.states.lock().map_err(|e| ManagerError::Lock(e.to_string()))?;
            states.remove(tab_id);

            Ok(true)
        } else {
            Err(ManagerError::NotFound(tab_id.to_string()))
        }
    }

    pub fn get_tab_info(&self, tab_id: &str) -> Result<TabInfo, ManagerError> {
        let tabs = self.tabs.lock().map_err(|e| ManagerError::Lock(e.to_string()))?;
        tabs.iter()
            .find(|t| t.id == tab_id)
            .map(|t| TabInfo {
                id: t.id.clone(),
                title: t.title.clone(),
                cwd: t.cwd.clone(),
                flashing: t.flashing,
            })
            .ok_or_else(|| ManagerError::NotFound(tab_id.to_string()))
    }

    pub fn update_title(&self, tab_id: &str, title: String) -> Result<(), ManagerError> {
        let mut tabs = self.tabs.lock().map_err(|e| ManagerError::Lock(e.to_string()))?;
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.title = title;
            Ok(())
        } else {
            Err(ManagerError::NotFound(tab_id.to_string()))
        }
    }

    pub fn set_flashing(&self, tab_id: &str, flashing: bool) -> Result<(), ManagerError> {
        let mut tabs = self.tabs.lock().map_err(|e| ManagerError::Lock(e.to_string()))?;
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.flashing = flashing;
            Ok(())
        } else {
            Err(ManagerError::NotFound(tab_id.to_string()))
        }
    }

    pub fn write_input(&self, tab_id: &str, data: &[u8]) -> Result<(), ManagerError> {
        let sessions = self.sessions.lock().map_err(|e| ManagerError::Lock(e.to_string()))?;
        let session = sessions
            .get(tab_id)
            .ok_or_else(|| ManagerError::NotFound(tab_id.to_string()))?;
        session.write(data)?;

        // If user pressed Enter and we had PTY output, Claude finished its turn
        if data.contains(&b'\n') || data.contains(&b'\r') {
            let mut states = self.states.lock().map_err(|e| ManagerError::Lock(e.to_string()))?;
            if let Some(state) = states.get_mut(tab_id) {
                if state.had_output {
                    state.had_output = false;
                    let _ = self.app.emit("tab-flash", serde_json::json!({
                        "tab_id": tab_id
                    }));
                }
            }
        }

        Ok(())
    }

    pub fn mark_output(&self, tab_id: &str) {
        let mut states = self.states.lock().ok();
        if let Some(states) = states.as_mut() {
            if let Some(state) = states.get_mut(tab_id) {
                state.had_output = true;
            }
        }
    }

    pub fn resize_pty(&self, tab_id: &str, rows: u16, cols: u16) -> Result<(), ManagerError> {
        let sessions = self.sessions.lock().map_err(|e| ManagerError::Lock(e.to_string()))?;
        let session = sessions
            .get(tab_id)
            .ok_or_else(|| ManagerError::NotFound(tab_id.to_string()))?;
        session.resize(rows, cols)?;
        Ok(())
    }

    pub fn has_active_process(&self, tab_id: &str) -> bool {
        let sessions = self.sessions.lock().ok();
        sessions.map(|s| s.contains_key(tab_id)).unwrap_or(false)
    }

    pub fn list_tabs(&self) -> Vec<TabInfo> {
        let tabs = self.tabs.lock().ok();
        tabs.map(|t| {
            t.iter()
                .map(|tab| TabInfo {
                    id: tab.id.clone(),
                    title: tab.title.clone(),
                    cwd: tab.cwd.clone(),
                    flashing: tab.flashing,
                })
                .collect()
        })
        .unwrap_or_default()
    }
}
