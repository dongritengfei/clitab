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
//!     false→true transition. It is posted through `UNUserNotificationCenter`
//!     (the deprecated `NSUserNotificationCenter` no longer delivers on
//!     modern macOS); posting is async and the request identifier is the tab
//!     id, so a re-ring replaces the tab's existing banner instead of
//!     stacking a second. Clicks come back through the delegate installed by
//!     [`init_notifications`], focusing the window and routing to the tab
//!     via `focus-tab`.
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
    post_notification(tab_id, &title);
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

/// Post the "waiting for input" banner. Runs on whichever reader thread
/// rang: `addNotificationRequest` is async and nothing blocks on the user's
/// answer. The identifier is the tab id, so Notification Center keeps one
/// thread per tab and the delegate reads it back as the click-routing key.
#[cfg(target_os = "macos")]
fn post_notification(tab_id: &str, title: &str) {
    use block2::RcBlock;
    use objc2::AnyThread;
    use objc2_foundation::{NSError, NSString};
    use objc2_user_notifications::{
        UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
    };

    let content = UNMutableNotificationContent::init(UNMutableNotificationContent::alloc());
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str("Waiting for input"));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(tab_id),
        &content,
        None, // no trigger: deliver immediately
    );
    let done: RcBlock<dyn Fn(*mut NSError)> = RcBlock::new(|error: *mut NSError| {
        if !error.is_null() {
            // The previous sender silently stopped delivering and nobody
            // noticed; leave a breadcrumb in stderr.
            eprintln!("clitab: notification post failed");
        }
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .addNotificationRequest_withCompletionHandler(&request, Some(&done));
}

#[cfg(not(target_os = "macos"))]
fn post_notification(_tab_id: &str, _title: &str) {}

/// Install the click delegate and ask for banner permission. Called from
/// `lib.rs` setup: the permission prompt should surface at launch, in the
/// foreground — never mid-session while the app is in the background.
///
/// `setDelegate` is a weak property, so the caller must keep the returned
/// singleton alive for the process lifetime (leaked in setup, the same deal
/// as the services provider).
#[cfg(target_os = "macos")]
pub(crate) fn init_notifications(
    app: &AppHandle,
) -> objc2::rc::Retained<notification_delegate::NotificationDelegate> {
    use block2::RcBlock;
    use objc2::runtime::{Bool, ProtocolObject};
    use objc2_foundation::NSError;
    use objc2_user_notifications::{UNAuthorizationOptions, UNUserNotificationCenter};

    let delegate = notification_delegate::NotificationDelegate::new(app.clone());
    let center = UNUserNotificationCenter::currentNotificationCenter();
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    let done: RcBlock<dyn Fn(Bool, *mut NSError)> =
        RcBlock::new(|_granted: Bool, _error: *mut NSError| {
            // A denial is visible and reversible in System Settings; banners
            // simply never appear and the rest of the queue (flash, badge,
            // ⌘J) keeps working.
        });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &done,
    );
    delegate
}

/// The `UNUserNotificationCenter` delegate: an ObjC class created at runtime
/// with objc2, mirroring the services provider's pattern.
#[cfg(target_os = "macos")]
mod notification_delegate {
    use super::MAIN_WINDOW;
    use block2::DynBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol};
    use objc2::{define_class, msg_send, AnyThread, DefinedClass};
    use objc2_user_notifications::{
        UNNotificationResponse, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    use tauri::{AppHandle, Emitter, Manager};

    pub(crate) struct DelegateIvars {
        app: AppHandle,
    }

    define_class!(
        // SAFETY: `NSObject` has no subclassing requirements, the ivars are
        // `Send + Sync`, and the class does not implement `Drop`. Left
        // any-thread like the services provider: UN delivers delegate calls
        // on its own queue, and the click routing hops to the main thread
        // explicitly.
        #[unsafe(super(NSObject))]
        #[name = "ClitabNotificationDelegate"]
        #[ivars = DelegateIvars]
        pub(crate) struct NotificationDelegate;

        // Required superclass conformance of the delegate protocol.
        unsafe impl NSObjectProtocol for NotificationDelegate {}

        unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
            /// The user clicked the banner (dismissing one delivers no
            /// response). The completion handler must be called or UN
            /// considers the interaction unprocessed.
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive_response(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                completion_handler: &DynBlock<dyn Fn()>,
            ) {
                let tab_id = response
                    .notification()
                    .request()
                    .identifier()
                    .to_string();
                let ivars = self.ivars();
                let app = ivars.app.clone();
                // Window work belongs on the main thread; UN calls this
                // delegate on its own queue.
                let _ = ivars.app.run_on_main_thread(move || {
                    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                        let _ = window.set_focus();
                    }
                    // The tab may be gone by now; the renderer guards on its
                    // mirror.
                    let _ = app
                        .emit("focus-tab", serde_json::json!({ "tab_id": tab_id.as_str() }));
                });
                completion_handler.call(());
            }
        }
    );

    impl NotificationDelegate {
        pub(crate) fn new(app: AppHandle) -> Retained<Self> {
            let this = Self::alloc().set_ivars(DelegateIvars { app });
            // SAFETY: plain `NSObject` initialization of a fresh allocation.
            unsafe { msg_send![super(this), init] }
        }
    }
}

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
