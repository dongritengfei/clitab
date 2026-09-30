mod menu;
mod osc;
mod pty;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use pty::manager::{ManagerError, TabManager};
use pty::registry::TabRecord;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{Listener, Manager, State};
use tauri_plugin_dialog::DialogExt;

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
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabResponse {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub has_claude_title: bool,
}

impl From<TabRecord> for TabResponse {
    fn from(tab: TabRecord) -> Self {
        Self {
            id: tab.id,
            title: tab.title,
            cwd: tab.cwd,
            has_claude_title: tab.has_program_title,
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

            if let Err(e) = tab_manager.create_tab(None) {
                // Without a PTY the window is useless, so say so out loud
                // instead of showing an empty shell.
                eprintln!("clitab: failed to start the initial terminal: {e}");
                app.dialog()
                    .message(format!("Could not start a terminal session:\n{e}"))
                    .title("clitab")
                    .show(|_| {});
            }

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
            has_active_process
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
}
