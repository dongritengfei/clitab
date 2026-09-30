//! Opening tabs from outside the renderer: the Finder "New clitab Tab Here"
//! service (NSServices, macOS only) and the delayed startup that lets a
//! cold launch land directly in the requested folder.

use std::path::{Path, PathBuf};

/// How long startup waits for a Finder service request before creating the
/// default tab. The request can only be delivered once the event loop runs,
/// so this wait must not block `setup` itself (see the startup thread in
/// `lib.rs`).
pub const STARTUP_GRACE_MS: u64 = 400;

/// Cold-start handshake between the service handler and the startup thread.
/// Both sides hold the mutex, so a request is either stashed (before settle)
/// or answered immediately (after) — it can never be lost in between.
#[derive(Default)]
pub struct ServiceState {
    /// Path requested during the startup grace; the latest one wins.
    pending: Option<PathBuf>,
    /// True once the startup thread has decided what the first tab is.
    settled: bool,
}

impl ServiceState {
    /// Hand a requested path to the state machine. Returns `Some(path)` when
    /// the app is already up (caller must open the tab now), `None` when the
    /// path was stashed for the startup thread.
    pub fn offer(&mut self, path: PathBuf) -> Option<PathBuf> {
        if self.settled {
            Some(path)
        } else {
            self.pending = Some(path);
            None
        }
    }

    /// Startup grace elapsed: mark settled and take the stashed path (if any).
    pub fn settle(&mut self) -> Option<PathBuf> {
        self.settled = true;
        self.pending.take()
    }
}

/// Percent-decode a URL path component byte-wise, so multi-byte UTF-8
/// (e.g. Chinese directory names) reassembles correctly. Malformed escapes
/// stay literal rather than erroring — a weird path should still open a tab.
pub fn percent_decode(input: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Normalize whatever string the pasteboard carried into a filesystem path:
/// the first non-empty line (multi-selection puts several), `file://` URLs
/// percent-decoded, anything else taken literally.
pub fn target_from_raw(raw: &str) -> Option<PathBuf> {
    let first = raw.lines().map(str::trim).find(|line| !line.is_empty())?;
    let path = match first.strip_prefix("file://") {
        Some(rest) => percent_decode(rest.split(['?', '#']).next().unwrap_or(rest)),
        None => first.to_string(),
    };
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// What a right-click target means for a new tab: a directory opens as-is;
/// anything else (a file, or a path that no longer exists) defers to its
/// parent — `resolve_cwd` in the manager applies the home fallback for stale
/// parents, so this function must not fail on those.
pub fn classify_path(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        Some(path.to_path_buf())
    } else {
        path.parent().map(PathBuf::from)
    }
}

/// Create a tab from outside the renderer (startup thread or Finder service)
/// and announce it. The renderer merges `tab-created` with its `list_tabs`
/// snapshot using the same id-dedup as its own `createTab`, so the order of
/// "event" vs. "webview mounted" does not matter. `focus` additionally brings
/// the window to the front — wanted for the service path, not for startup
/// (the window is already coming up).
pub fn open_tab(app: &tauri::AppHandle, cwd: Option<String>, focus: bool) {
    use tauri::{Emitter, Manager};
    use tauri_plugin_dialog::DialogExt;

    let manager = &app.state::<crate::AppState>().tab_manager;
    match manager.create_tab(cwd) {
        Ok(record) => {
            let _ = app.emit("tab-created", crate::TabResponse::from(record));
            if focus {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.unminimize();
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        }
        Err(e) => {
            // Same contract as the old synchronous startup path: without a
            // PTY the window is useless, so say so out loud.
            eprintln!("clitab: failed to open a terminal tab: {e}");
            app.dialog()
                .message(format!("Could not start a terminal session:\n{e}"))
                .title("clitab")
                .show(|_| {});
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decodes_paths() {
        assert_eq!(percent_decode("/Users/x/a%20b"), "/Users/x/a b");
        assert_eq!(percent_decode("/Users/x/%E6%88%91"), "/Users/x/我");
        assert_eq!(percent_decode("100%"), "100%"); // dangling % stays literal
        assert_eq!(percent_decode("%zz"), "%zz"); // malformed escape stays literal
    }

    #[test]
    fn file_urls_become_paths() {
        assert_eq!(
            target_from_raw("file:///tmp/a%20dir/").unwrap(),
            PathBuf::from("/tmp/a dir/")
        );
        assert_eq!(target_from_raw("/tmp/plain").unwrap(), PathBuf::from("/tmp/plain"));
    }

    #[test]
    fn first_line_wins_for_multi_selection() {
        assert_eq!(
            target_from_raw("file:///tmp/one\nfile:///tmp/two").unwrap(),
            PathBuf::from("/tmp/one")
        );
    }

    #[test]
    fn empty_input_is_rejected() {
        assert_eq!(target_from_raw(""), None);
        assert_eq!(target_from_raw("\n  \n"), None);
    }

    #[test]
    fn directories_pass_through_files_become_parent() {
        let dir = std::env::temp_dir();
        assert_eq!(classify_path(&dir).unwrap(), dir);

        let file = dir.join("clitab-classify-test");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(classify_path(&file).unwrap(), dir);
        std::fs::remove_file(&file).unwrap();
    }

    #[test]
    fn missing_paths_pass_through() {
        // Neither dir nor file: hand back the parent so `resolve_cwd` can
        // apply its home fallback; classification must not fail here.
        assert_eq!(
            classify_path(Path::new("/clitab-no-such/file")).unwrap(),
            PathBuf::from("/clitab-no-such")
        );
    }

    #[test]
    fn offer_before_settle_stashes_and_settle_returns_it() {
        let mut state = ServiceState::default();
        assert_eq!(state.offer(PathBuf::from("/a")), None);
        assert_eq!(state.settle().unwrap(), PathBuf::from("/a"));
    }

    #[test]
    fn offer_after_settle_returns_immediately() {
        let mut state = ServiceState::default();
        state.settle();
        assert_eq!(state.offer(PathBuf::from("/b")).unwrap(), PathBuf::from("/b"));
    }

    #[test]
    fn last_pending_wins_and_settle_is_one_shot() {
        let mut state = ServiceState::default();
        state.offer(PathBuf::from("/a"));
        state.offer(PathBuf::from("/b"));
        assert_eq!(state.settle().unwrap(), PathBuf::from("/b"));
        assert_eq!(state.settle(), None);
    }
}

/// The NSServices provider: an ObjC class created at runtime with objc2.
/// Registered from `lib.rs` setup; `Info.plist` declares the matching
/// `openTab` service so Finder shows "New clitab Tab Here".
#[cfg(target_os = "macos")]
mod provider {
    use super::{classify_path, open_tab, target_from_raw, ServiceState};
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject};
    use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker};
    use objc2_app_kit::{
        NSApplication, NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypeString,
    };
    use objc2_foundation::NSString;
    use std::sync::{Arc, Mutex};
    use tauri::AppHandle;

    pub(crate) struct ProviderIvars {
        app: AppHandle,
        state: Arc<Mutex<ServiceState>>,
    }

    define_class!(
        // SAFETY: `NSObject` has no subclassing requirements, the ivars are
        // `Send + Sync`, and the class does not implement `Drop`. Left
        // any-thread (not `MainThreadOnly`) so the type stays `Send + Sync`
        // for the caller that keeps it alive.
        #[unsafe(super(NSObject))]
        #[name = "ClitabServices"]
        #[ivars = ProviderIvars]
        pub(crate) struct ServicesProvider;

        impl ServicesProvider {
            /// Finder → Services → "New clitab Tab Here". Returning `NO`
            /// means "not handled" (no usable path on the pasteboard).
            #[unsafe(method(openTab:userData:))]
            fn open_tab_service(&self, pboard: &NSPasteboard, _user_data: Option<&NSString>) -> Bool {
                let Some(path) = read_pasteboard_path(pboard)
                    .as_deref()
                    .and_then(target_from_raw)
                    .and_then(|p| classify_path(&p))
                else {
                    return Bool::NO;
                };
                let ivars = self.ivars();
                // Settled → open now; still within the startup grace → stash
                // for the startup thread, which turns it into the first tab.
                if let Some(path) = crate::pty::lock(&ivars.state).offer(path) {
                    open_tab(&ivars.app, Some(path.to_string_lossy().into_owned()), true);
                }
                Bool::YES
            }
        }
    );

    impl ServicesProvider {
        fn new(app: AppHandle, state: Arc<Mutex<ServiceState>>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(ProviderIvars { app, state });
            // SAFETY: plain `NSObject` initialization of a fresh allocation.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// First usable path on the pasteboard: the file URL (what Finder writes
    /// for file selections) or, failing that, the plain-text path string
    /// (both types are declared in `Info.plist`'s `NSSendTypes`).
    fn read_pasteboard_path(pboard: &NSPasteboard) -> Option<String> {
        // SAFETY: reading two `extern static` pasteboard-type constants.
        let (file_url, string) = unsafe { (&*NSPasteboardTypeFileURL, &*NSPasteboardTypeString) };
        pboard
            .stringForType(file_url)
            .or_else(|| pboard.stringForType(string))
            .map(|s| s.to_string())
    }

    /// Make `NSApp` route service requests to a fresh provider. Must run on
    /// the main thread before the event loop starts pumping (Tauri `setup`).
    /// The caller must keep the returned provider alive: `servicesProvider`
    /// is an unretained reference.
    pub(crate) fn register(
        app: &AppHandle,
        state: Arc<Mutex<ServiceState>>,
    ) -> Retained<ServicesProvider> {
        let mtm =
            MainThreadMarker::new().expect("services registration must happen on the main thread");
        let provider = ServicesProvider::new(app.clone(), state);
        // SAFETY: main thread (marker above); the provider is handed back to
        // the caller, so it outlives this assignment.
        unsafe { NSApplication::sharedApplication(mtm).setServicesProvider(Some(&provider)) };
        provider
    }
}

#[cfg(target_os = "macos")]
pub(crate) use provider::register;
