//! Side effects of the "waiting for input" triage queue: the Dock badge,
//! macOS notifications, and the events the renderer mirrors.
//!
//! The queue state itself lives in the [`Registry`](crate::pty::registry::Registry)
//! (`waiting` on each tab record); this module is the only place that reacts
//! to transitions:
//!
//!   * `tab-waiting` keeps the renderer's mirror in sync;
//!   * the badge is the count of waiting tabs, recomputed on every change so
//!     it stays correct across a webview reload;
//!   * a notification fires only while the main window is unfocused, one per
//!     false→true transition. Its thread blocks in `send_notification` until
//!     the user clicks or dismisses; a click focuses the window and routes
//!     back to the tab via `focus-tab`.
//!
//! Locking: only the registry lock is ever taken here — never the session
//! map — preserving the "two locks, never held together" invariant.

use crate::pty::registry::Registry;
use tauri::{AppHandle, Emitter, Manager};

/// The window label in `tauri.conf.json`; must match `app.windows[].label`.
const MAIN_WINDOW: &str = "main";

/// Badge mapping: `None` hides the badge on macOS; a count of 0 must not
/// render as "0".
pub fn badge_for(count: usize) -> Option<i64> {
    (count > 0).then_some(count as i64)
}

pub fn update_badge(app: &AppHandle, registry: &Registry) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.set_badge_count(badge_for(registry.waiting_count()));
    }
}

/// A tab rang for attention (BEL, `claude-done`, stalled turn): enter the
/// triage queue. Called at every site that emits `tab-flash`, after any
/// registry title updates so a notification carries the title the tab bar
/// currently shows.
pub fn enter_waiting(app: &AppHandle, registry: &Registry, tab_id: &str) {
    if !registry.set_waiting(tab_id) {
        return; // already queued: repeated signals notify once per entry
    }
    let _ = app.emit(
        "tab-waiting",
        serde_json::json!({ "tab_id": tab_id, "waiting": true }),
    );
    update_badge(app, registry);

    // Watching the app, the flash and the badge are noise enough.
    let focused = app
        .get_webview_window(MAIN_WINDOW)
        .map(|w| w.is_focused().unwrap_or(false))
        .unwrap_or(false);
    if focused {
        return;
    }
    let title = registry
        .get(tab_id)
        .map(|tab| tab.title)
        .unwrap_or_else(|| "clitab".to_string());
    spawn_notification(app.clone(), tab_id.to_string(), title);
}

/// The user typed into this tab: it answered, leave the queue.
pub fn respond(app: &AppHandle, registry: &Registry, tab_id: &str) {
    if !registry.clear_waiting(tab_id) {
        return; // was not waiting: a silent no-op
    }
    let _ = app.emit(
        "tab-waiting",
        serde_json::json!({ "tab_id": tab_id, "waiting": false }),
    );
    update_badge(app, registry);
}

/// One thread per notification: `send_notification` blocks (condvar inside
/// `mac-notification-sys`) until the user clicks or dismisses it, and the
/// response is what routes the click back to `tab_id`.
#[cfg(target_os = "macos")]
fn spawn_notification(app: AppHandle, tab_id: String, title: String) {
    std::thread::spawn(move || {
        use mac_notification_sys::{Notification, NotificationResponse};
        let response = Notification::new()
            .title(&title)
            .message("Waiting for input")
            .wait_for_click(true)
            .send();
        if matches!(response, Ok(NotificationResponse::Click)) {
            if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                let _ = window.set_focus();
            }
            // The tab may be gone by now; the renderer guards on its mirror.
            let _ = app.emit("focus-tab", serde_json::json!({ "tab_id": tab_id }));
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn spawn_notification(_app: AppHandle, _tab_id: String, _title: String) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_hides_the_badge() {
        assert_eq!(badge_for(0), None);
        assert_eq!(badge_for(1), Some(1));
        assert_eq!(badge_for(3), Some(3));
    }
}
