mod osc;
mod pty;

use pty::manager::TabManager;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{Listener, Manager, State};

pub struct AppState {
    pub tab_manager: Arc<TabManager>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TabInfoResponse {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub flashing: bool,
}

#[tauri::command]
fn create_tab(state: State<AppState>) -> Result<TabInfoResponse, String> {
    let tab = state.tab_manager.create_tab().map_err(|e| e.to_string())?;
    Ok(TabInfoResponse {
        id: tab.id,
        title: tab.title,
        cwd: tab.cwd,
        flashing: tab.flashing,
    })
}

#[tauri::command]
fn close_tab(state: State<AppState>, tab_id: String) -> Result<bool, String> {
    state.tab_manager.close_tab(&tab_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn switch_tab(state: State<AppState>, tab_id: String) -> Result<(), String> {
    state.tab_manager.set_flashing(&tab_id, false).map_err(|e| e.to_string())
}

#[tauri::command]
fn pty_input(state: State<AppState>, tab_id: String, data: Vec<u8>) -> Result<(), String> {
    state.tab_manager.write_input(&tab_id, &data).map_err(|e| e.to_string())
}

#[tauri::command]
fn resize_pty(state: State<AppState>, tab_id: String, rows: u16, cols: u16) -> Result<(), String> {
    state.tab_manager.resize_pty(&tab_id, rows, cols).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_tabs(state: State<AppState>) -> Result<Vec<TabInfoResponse>, String> {
    let tabs = state.tab_manager.list_tabs();
    Ok(tabs
        .into_iter()
        .map(|t| TabInfoResponse {
            id: t.id,
            title: t.title,
            cwd: t.cwd,
            flashing: t.flashing,
        })
        .collect())
}

#[tauri::command]
fn has_active_process(state: State<AppState>, tab_id: String) -> Result<bool, String> {
    Ok(state.tab_manager.has_active_process(&tab_id))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let tab_manager = Arc::new(TabManager::new(app.handle().clone()));

            app.manage(AppState { tab_manager: tab_manager.clone() });

            // Listen for PTY output to track activity
            let tm = tab_manager.clone();
            app.listen("pty-output", move |event| {
                if let Ok(payload) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                    if let Some(tab_id) = payload["tab_id"].as_str() {
                        tm.mark_output(tab_id);
                    }
                }
            });

            // Create initial tab
            let _ = tab_manager.create_tab();

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            create_tab,
            close_tab,
            switch_tab,
            pty_input,
            resize_pty,
            list_tabs,
            has_active_process
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
