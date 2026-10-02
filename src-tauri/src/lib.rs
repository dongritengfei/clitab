mod attention;
mod menu;
mod osc;
mod pty;
mod services;
mod status;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use pty::manager::{ManagerError, TabManager};
use pty::registry::TabRecord;
use serde::{Deserialize, Serialize};
use status::{Notice, TabStatus};
use std::sync::{Arc, Mutex};
use tauri::{Listener, Manager, State};

/// Errors cross the IPC boundary as strings, so "the tab is gone" — which the
/// renderer must tolerate silently — gets an explicit marker instead of being
/// guessed from the message wording.
pub const TAB_GONE: &str = "clitab:tab-gone:";

fn ipc_error(error: ManagerError) -> String {
    match error {
        ManagerError::NotFound(id) => format!("{TAB_GONE}{id}"),
        other => other.to_string(),
    }
}

pub struct AppState {
    pub tab_manager: Arc<TabManager>,
}

/// Tab metadata handed to the renderer. `camelCase` so it lines up with the
/// `Tab` type in `src/types.ts` without a mapping step.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabResponse {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub has_claude_title: bool,
    /// Hook-protocol turn state; null until the tab's session speaks it.
    pub status: Option<TabStatus>,
    /// Notification awaiting the user; null once acknowledged.
    pub notice: Option<Notice>,
    pub waiting: bool,
}

impl From<TabRecord> for TabResponse {
    fn from(tab: TabRecord) -> Self {
        Self {
            id: tab.id,
            title: tab.title,
            cwd: tab.cwd,
            has_claude_title: tab.has_program_title,
            status: tab.status,
            notice: tab.notice,
            // `turn_start` is deliberately not exposed: backend-internal.
            waiting: tab.waiting,
        }
    }
}

/// Replay bytes (base64) handed to a terminal view on attach, plus the stream
/// position the replay ends at so live chunks can be deduplicated against it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachStreamResponse {
    pub data: String,
    pub replay_end: u64,
}

/// `cwd` is the directory the new tab should start in (the renderer passes the
/// active tab's working directory); `None` or a stale path falls back to home.
#[tauri::command]
fn create_tab(state: State<'_, AppState>, cwd: Option<String>) -> Result<TabResponse, String> {
    state
        .tab_manager
        .create_tab(cwd)
        .map(TabResponse::from)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn close_tab(state: State<'_, AppState>, tab_id: String) -> Result<(), String> {
    state.tab_manager.close_tab(&tab_id).map_err(ipc_error)
}

#[tauri::command]
fn list_tabs(state: State<'_, AppState>) -> Result<Vec<TabResponse>, String> {
    Ok(state
        .tab_manager
        .list_tabs()
        .into_iter()
        .map(TabResponse::from)
        .collect())
}

/// Keystrokes / paste, base64 encoded (a JSON number array per paste would be
/// far more expensive on both sides).
#[tauri::command]
fn pty_input(state: State<'_, AppState>, tab_id: String, data: String) -> Result<(), String> {
    let bytes = BASE64
        .decode(&data)
        .map_err(|e| format!("invalid base64 input: {e}"))?;
    state
        .tab_manager
        .write_input(&tab_id, &bytes)
        .map_err(ipc_error)
}

#[tauri::command]
fn resize_pty(
    state: State<'_, AppState>,
    tab_id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    state
        .tab_manager
        .resize_pty(&tab_id, rows, cols)
        .map_err(ipc_error)
}

/// Recent output of a tab (base64) plus the stream position that replay ends
/// at, and the signal to stream it live from now on. A terminal view calls
/// this when it mounts, which is what lets a webview reload rebuild its screen
/// instead of showing an empty terminal. The renderer compares `replay_end`
/// against each live chunk's `seq` so a re-attach does not write bytes twice.
#[tauri::command]
fn attach_stream(
    state: State<'_, AppState>,
    tab_id: String,
) -> Result<AttachStreamResponse, String> {
    let (data, replay_end) = state.tab_manager.attach_stream(&tab_id).map_err(ipc_error)?;
    Ok(AttachStreamResponse { data, replay_end })
}

/// The renderer unmounted this tab's view: stop emitting into a dead handler.
#[tauri::command]
fn detach_tab(state: State<'_, AppState>, tab_id: String) -> Result<(), String> {
    state.tab_manager.detach(&tab_id);
    Ok(())
}

#[tauri::command]
fn has_active_process(state: State<'_, AppState>, tab_id: String) -> Result<bool, String> {
    Ok(state.tab_manager.has_active_process(&tab_id))
}

/// The user switched to this tab, so its notification has been seen. Never
/// errors on an unknown tab: a stale ack is harmless.
#[tauri::command]
fn ack_tab_notice(state: State<'_, AppState>, tab_id: String) -> Result<(), String> {
    state.tab_manager.ack_notice(&tab_id);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .menu(menu::build)
        .on_menu_event(|app, event| menu::forward(app, event.id().as_ref()))
        .setup(|app| {
            let tab_manager = Arc::new(TabManager::new(app.handle().clone()));
            app.manage(AppState {
                tab_manager: tab_manager.clone(),
            });

            // A shell that exits on its own (`exit`, a crash, ...) must not
            // leave a dead tab behind.
            let manager = tab_manager.clone();
            app.listen("tab-exit", move |event| {
                if let Ok(payload) = serde_json::from_str::<TabExitPayload>(event.payload()) {
                    manager.remove_session(&payload.tab_id);
                }
            });

            let service_state = Arc::new(Mutex::new(services::ServiceState::default()));
            #[cfg(target_os = "macos")]
            {
                // NSApp keeps the provider by unretained reference; leaking
                // the singleton is the cheapest way to outlive this scope.
                std::mem::forget(services::register(app.handle(), service_state.clone()));
                // UNUserNotificationCenter keeps its delegate by weak
                // reference: leak the singleton the same way.
                std::mem::forget(attention::init_notifications(app.handle()));
            }

            // The first tab is created shortly *after* startup: an app
            // launched from Finder only receives the service request once the
            // event loop runs, so waiting briefly lets that request become
            // the first tab instead of stacking on an unwanted home tab.
            // The wait must live on a thread — sleeping in `setup` would
            // block the very event loop that delivers the request.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(
                    services::STARTUP_GRACE_MS,
                ));
                let pending = pty::lock(&service_state).settle();
                // A tab already exists only if the user beat the grace
                // (e.g. ⌘T while the webview was loading); don't stack.
                if !handle.state::<AppState>().tab_manager.list_tabs().is_empty() {
                    return;
                }
                services::open_tab(
                    &handle,
                    pending.map(|p| p.to_string_lossy().into_owned()),
                    false,
                );
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            create_tab,
            close_tab,
            list_tabs,
            pty_input,
            resize_pty,
            attach_stream,
            detach_tab,
            has_active_process,
            ack_tab_notice
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Typed mirror of the `tab-exit` payload, so the listener does not have to fish
/// through an untyped `serde_json::Value`.
#[derive(Debug, Deserialize)]
struct TabExitPayload {
    tab_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `src/types.ts` mirrors `TAB_GONE` as a literal: the renderer uses it to
    /// tell "the tab vanished mid-call" apart from a failure it must surface.
    #[test]
    fn missing_tabs_report_the_shared_marker() {
        let gone = ipc_error(ManagerError::NotFound("abc-123".into()));
        assert!(gone.starts_with(TAB_GONE), "{gone}");
        assert!(gone.contains("abc-123"), "the id should survive: {gone}");

        let real = ipc_error(ManagerError::Session(pty::session::SessionError::Pty(
            "boom".into(),
        )));
        assert!(
            !real.starts_with(TAB_GONE),
            "other failures must not match: {real}"
        );
        assert!(real.contains("boom"), "{real}");
    }

    use crate::status::{Notice, TabStatus};

    /// Pins the IPC contract for `src/types.ts`: camelCase, `kind`-tagged
    /// status, nulls (not absent keys) for missing protocol state, and the
    /// backend-internal `turn_start` never leaks.
    #[test]
    fn tab_response_serializes_protocol_state() {
        let record = TabRecord {
            id: "t1".into(),
            title: "Fix build".into(),
            cwd: "/tmp".into(),
            has_program_title: true,
            waiting: false,
            status: Some(TabStatus::Tool {
                name: "Bash".into(),
                since: 1700000000000,
            }),
            notice: None,
            turn_start: Some(1700000000000),
        };
        let json = serde_json::to_value(TabResponse::from(record)).unwrap();
        assert_eq!(
            json["status"],
            serde_json::json!({"kind": "tool", "name": "Bash", "since": 1700000000000u64})
        );
        assert_eq!(json["notice"], serde_json::Value::Null);
        assert_eq!(json["waiting"], serde_json::json!(false));
        assert!(json.get("turnStart").is_none());

        let idle = TabRecord {
            id: "t2".into(),
            title: "/tmp".into(),
            cwd: "/tmp".into(),
            has_program_title: false,
            waiting: false,
            status: None,
            notice: Some(Notice {
                msg: Some("needs permission".into()),
                at: 5,
            }),
            turn_start: None,
        };
        let json = serde_json::to_value(TabResponse::from(idle)).unwrap();
        assert_eq!(json["status"], serde_json::Value::Null);
        assert_eq!(
            json["notice"],
            serde_json::json!({"msg": "needs permission", "at": 5})
        );
    }
}
