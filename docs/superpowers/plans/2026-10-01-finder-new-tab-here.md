# Finder "New clitab Tab Here" Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Right-clicking a folder (or file, or Finder window background) in Finder offers "New clitab Tab Here" (Services submenu), opening a focused clitab tab in that directory whether or not the app is already running.

**Architecture:** NSServices (same mechanism as Ghostty/iTerm2): `src-tauri/Info.plist` declares the service; an objc2 `define_class!` provider registered via `NSApp.setServicesProvider` handles `openTab:userData:`. A `Mutex<ServiceState>` state machine resolves the cold-start race: requests arriving before the 400 ms startup grace are stashed as the initial tab's cwd; later requests create a tab immediately on the backend and emit `tab-created`, which the renderer merges with its existing dedup pattern.

**Tech Stack:** Tauri 2 / Rust, objc2 0.6 + objc2-app-kit 0.3 + objc2-foundation 0.3 (already in Cargo.lock as tao/wry transitive deps), React 18 + xterm.js frontend.

**Spec:** `docs/superpowers/specs/2026-10-01-finder-new-tab-here-design.md`

**Spec refinements (deviations, deliberate):**
1. The spec had the settled branch emit an `open-tab-at` event for the *renderer* to call `create_tab`. That loses the message when a cold-start service request arrives after the 400 ms settle but before the webview has mounted its listeners. This plan instead creates the tab **on the backend** in both branches and emits `tab-created` (payload = existing `TabResponse`) purely as a UI notification; if no renderer is listening yet, the renderer's mount-time `list_tabs` snapshot picks the tab up. Consequence: `src/types.ts` needs no change and `createTab` is not parameterized (the spec's frontend section is superseded); user-visible behavior is identical.
2. Provider keep-alive: the spec said "store in `AppState`"; the plan uses `std::mem::forget` on the singleton instead (same guarantee — outlives the process — without a cfg-gated `AppState` field).
3. `NSSendTypes` adds `public.file-url` alongside Ghostty's two types, because the handler reads that type first (`stringForType`), avoiding both the deprecated `NSFilenamesPboardType` and an unconstructible `NSArray<AnyClass>`.

## Global Constraints

- macOS only; objc2 deps are target-gated: `objc2 = "0.6"`, `objc2-app-kit = { version = "0.3", features = ["NSApplication", "NSPasteboard", "NSResponder"] }`, `objc2-foundation = { version = "0.3", features = ["NSString", "NSObject"] }` (versions must match Cargo.lock: objc2 0.6.4, app-kit/foundation 0.3.2 — do not bump).
- Menu copy is exactly `New clitab Tab Here`; `NSMessage` is exactly `openTab`; the selector implemented is `openTab:userData:`.
- Startup grace is exactly 400 ms (`ServiceState` settle delay), exposed as `services::STARTUP_GRACE_MS`.
- Services registration must happen on the main thread (Tauri `setup`), before the event loop pumps.
- Event names stay kebab-case strings (`tab-created`); the payload reuses the camelCase `TabResponse` serde struct — do not invent a new payload shape.
- Mutexes are locked via `pty::lock()` (poison-recovering), never `.lock().unwrap()`.
- READMEs (EN + zh-CN) stay in sync; CLAUDE.md's event list stays accurate.
- Services only register from a real `.app` bundle — `npm run tauri dev` cannot exercise this feature; manual verification requires `npm run package:macos`.
- Commit messages end with `Co-Authored-By: Claude Code <noreply@anthropic.com>`.
- Work happens directly on `master` (repo convention; the user's WIP already lives there).

## Review Focus

1. **Percent-encoded paths** (spaces / non-ASCII, e.g. `file:///Users/x/%E6%88%91%20dir/`) — expect the exact decoded directory as cwd. Pinned by Task 2 tests `percent_decodes_paths`, `file_urls_become_paths`.
2. **Multi-selection in Finder** (several paths / multi-line pasteboard text) — expect only the first path, exactly one tab. Pinned by Task 2 test `first_line_wins_for_multi_selection`.
3. **Cold start, message after settle but before webview mount** — expect the tab to exist regardless (backend-driven creation; renderer learns via `tab-created` or mount-time `list_tabs`). Pinned by Task 8 manual scenario ②.
4. **Pasteboard lacking a file-url entry** — expect fallback to plain-text path; if neither yields a path, return `false`, no tab, no crash. Pinned by Task 3 fallback chain + Task 8 scenario ③/④.
5. **Target directory deleted between click and creation** — expect silent fallback to home (existing `resolve_cwd`), no error dialog. Pinned by Task 2 test `missing_paths_pass_through` + existing `resolve_cwd` tests.

---

### Task 1: Commit the existing `create_tab(cwd)` foundation

The working tree already contains the feature's groundwork (uncommitted WIP by the user): `create_tab(cwd)` with `resolve_cwd` fallback + tests, renderer passing the active tab's cwd, README lines. Commit it separately so later commits stay reviewable.

**Files:**
- Modify (already modified in working tree): `README.md`, `README.zh-CN.md`, `src-tauri/src/lib.rs`, `src-tauri/src/pty/manager.rs`, `src/hooks/useTabManager.ts`

**Interfaces:**
- Consumes: nothing.
- Produces: `TabManager::create_tab(cwd: Option<String>) -> Result<TabRecord, ManagerError>` (falls back to `session::default_cwd()` when `None` or the directory is gone); the `create_tab` Tauri command takes `cwd: Option<String>`. Later tasks call both.

- [ ] **Step 1: Verify the WIP is green**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib && npm run typecheck`
Expected: all tests pass (including `requested_directory_wins_when_it_exists`, `missing_or_absent_request_falls_back_to_default`), typecheck clean.

- [ ] **Step 2: Commit exactly these five files**

```bash
git add README.md README.zh-CN.md src-tauri/src/lib.rs src-tauri/src/pty/manager.rs src/hooks/useTabManager.ts
git commit -m "$(cat <<'EOF'
feat: new tabs inherit the active tab's working directory

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: `services.rs` — pure logic + state machine (TDD)

Cross-platform pure Rust: pasteboard-text → path classification, and the `ServiceState` cold-start state machine. No objc2 yet, so everything here is unit-testable.

**Files:**
- Create: `src-tauri/src/services.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod services;` next to the existing `mod menu; mod osc; mod pty;`)

**Interfaces:**
- Consumes: nothing (pure).
- Produces (used by Tasks 3–4):
  - `pub const STARTUP_GRACE_MS: u64 = 400;`
  - `pub struct ServiceState { .. }` with `Default`, `pub fn offer(&mut self, path: PathBuf) -> Option<PathBuf>` (returns the path back when settled = "open now"; stashes as pending when not) and `pub fn settle(&mut self) -> Option<PathBuf>` (marks settled, returns pending, one-shot).
  - `pub fn percent_decode(input: &str) -> String`
  - `pub fn target_from_raw(raw: &str) -> Option<PathBuf>` (first non-empty line; `file://` prefix → percent-decoded path; otherwise the literal string)
  - `pub fn classify_path(path: &Path) -> Option<PathBuf>` (directory → itself; anything else → parent, letting `resolve_cwd` handle stale paths)

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/services.rs` containing only:

```rust
//! Opening tabs from outside the renderer: the Finder "New clitab Tab Here"
//! service (NSServices, macOS only) and the delayed startup that lets a
//! cold launch land directly in the requested folder.

use std::path::{Path, PathBuf};

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
```

Also add `mod services;` to `src-tauri/src/lib.rs` right after the existing `mod pty;` line.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib services`
Expected: FAIL — `cannot find function percent_decode`, `cannot find type ServiceState`, etc. (compilation errors count as the failing state here).

- [ ] **Step 3: Write the implementation**

Insert above the `#[cfg(test)]` block in `src-tauri/src/services.rs`:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: PASS — all 9 new `services::tests` plus the pre-existing suite.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/services.rs src-tauri/src/lib.rs
git commit -m "$(cat <<'EOF'
feat: path classification and cold-start state machine for the Finder service

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: objc2 provider class + backend tab opening

**Files:**
- Modify: `src-tauri/Cargo.toml` (target-gated dependencies)
- Modify: `src-tauri/src/services.rs` (append `open_tab` helper and the cfg-gated `provider` module)

**Interfaces:**
- Consumes: Task 2's `ServiceState::offer`, `target_from_raw`, `classify_path`; `AppState.tab_manager.create_tab(Option<String>) -> Result<TabRecord, ManagerError>`; `TabResponse: From<TabRecord>` + `Serialize`; `pty::lock`.
- Produces (used by Task 4):
  - `pub fn open_tab(app: &AppHandle, cwd: Option<String>, focus: bool)` — creates the tab on the backend, emits `tab-created` with the `TabResponse` payload, optionally unminimizes/shows/focuses the `main` window, and on failure keeps the existing eprintln + dialog behavior.
  - `#[cfg(target_os = "macos")] pub fn register(app: &AppHandle, state: Arc<Mutex<ServiceState>>) -> Retained<ServicesProvider>` — main-thread only; sets `NSApp.servicesProvider` and returns the provider so the caller can keep it alive.

- [ ] **Step 1: Add the dependencies**

Append to `src-tauri/Cargo.toml` (after the existing `[dependencies]` block):

```toml
[target.'cfg(target_os = "macos")'.dependencies]
objc2 = "0.6"
objc2-app-kit = { version = "0.3", features = ["NSApplication", "NSPasteboard", "NSResponder"] }
objc2-foundation = { version = "0.3", features = ["NSString", "NSObject"] }
```

- [ ] **Step 2: Add the `open_tab` helper to `services.rs`**

Insert after `classify_path` (before the `#[cfg(test)]` block):

```rust
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
```

- [ ] **Step 3: Add the cfg-gated objc2 provider module**

Append to the end of `src-tauri/src/services.rs` (after the tests module):

```rust
/// The NSServices provider: an ObjC class created at runtime with objc2.
/// Registered from `lib.rs` setup; `Info.plist` declares the matching
/// `openTab` service so Finder shows "New clitab Tab Here".
#[cfg(target_os = "macos")]
mod provider {
    use super::{classify_path, open_tab, target_from_raw, ServiceState};
    use objc2::rc::Retained;
    use objc2::runtime::NSObject;
    use objc2::{define_class, msg_send, ClassType, DefinedClass, MainThreadMarker};
    use objc2_app_kit::{
        NSApplication, NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypeString,
    };
    use objc2_foundation::NSString;
    use std::sync::{Arc, Mutex};
    use tauri::AppHandle;

    struct ProviderIvars {
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
        struct ServicesProvider;

        impl ServicesProvider {
            /// Finder → Services → "New clitab Tab Here". Returning `false`
            /// means "not handled" (no usable path on the pasteboard).
            #[unsafe(method(openTab:userData:))]
            fn open_tab_service(&self, pboard: &NSPasteboard, _user_data: Option<&NSString>) -> bool {
                let Some(path) = read_pasteboard_path(pboard)
                    .as_deref()
                    .and_then(target_from_raw)
                    .and_then(|p| classify_path(&p))
                else {
                    return false;
                };
                let ivars = self.ivars();
                // Settled → open now; still within the startup grace → stash
                // for the startup thread, which turns it into the first tab.
                if let Some(path) = crate::pty::lock(&ivars.state).offer(path) {
                    open_tab(&ivars.app, Some(path.to_string_lossy().into_owned()), true);
                }
                true
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
    pub fn register(app: &AppHandle, state: Arc<Mutex<ServiceState>>) -> Retained<ServicesProvider> {
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
pub use provider::register;
```

- [ ] **Step 4: Verify it compiles and tests still pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: PASS. Compilation exercises the objc2 macros; the runtime behavior of the provider is only observable from a packaged app (Task 8). If `define_class!` rejects a signature, do not change the selector — the ObjC-side contract (`openTab:userData:` → BOOL) is fixed by the spec.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/services.rs
git commit -m "$(cat <<'EOF'
feat: NSServices provider class and backend tab opening for the Finder service

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: `lib.rs` — register the provider, delay the initial tab

**Files:**
- Modify: `src-tauri/src/lib.rs` (imports; `setup` body around the current `create_tab(None)` call)

**Interfaces:**
- Consumes: `services::{register, open_tab, ServiceState, STARTUP_GRACE_MS}`; `pty::lock`; `TabManager::list_tabs() -> Vec<TabRecord>`.
- Produces: startup behavior every other task assumes — exactly one initial tab, created ~400 ms after setup, with the Finder-requested directory as cwd when one arrived during the grace.

- [ ] **Step 1: Replace the synchronous initial-tab creation**

In `lib.rs` `setup`, replace this block:

```rust
            if let Err(e) = tab_manager.create_tab(None) {
                // Without a PTY the window is useless, so say so out loud
                // instead of showing an empty shell.
                eprintln!("clitab: failed to start the initial terminal: {e}");
                app.dialog()
                    .message(format!("Could not start a terminal session:\n{e}"))
                    .title("clitab")
                    .show(|_| {});
            }
```

with:

```rust
            let service_state = Arc::new(Mutex::new(services::ServiceState::default()));
            #[cfg(target_os = "macos")]
            {
                // NSApp keeps the provider by unretained reference; leaking
                // the singleton is the cheapest way to outlive this scope.
                std::mem::forget(services::register(app.handle(), service_state.clone()));
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
```

Add `Mutex` to the existing `use std::sync::Arc;` import (→ `use std::sync::{Arc, Mutex};`). `Manager` is already imported; `DialogExt` stays imported (still used? — check: after this change `lib.rs` no longer calls `app.dialog()`; remove the `use tauri_plugin_dialog::DialogExt;` import **only if** the compiler flags it unused — the dialog call moved into `services::open_tab`, which imports it locally).

- [ ] **Step 2: Verify**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib && npm run typecheck`
Expected: PASS / clean. (Full app behavior is verified manually in Task 8; a quick `npm run tauri dev` sanity check is allowed: the window should show a shell after ~0.4 s instead of instantly.)

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "$(cat <<'EOF'
feat: register the Finder services provider and delay the initial tab

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Frontend — merge backend-created tabs

**Files:**
- Modify: `src/hooks/useTabManager.ts` (add one listener inside the existing `listeners.push(...)` block)

**Interfaces:**
- Consumes: `tab-created` event, payload = existing `TabResponse` (camelCase, includes `hasClaudeTitle`); `toTab(response)` helper already in the file.
- Produces: renderer behavior Task 8 verifies — a backend-created tab appears and becomes active whether the event arrives before or after mount (the `list_tabs` snapshot + id-dedup covers the before-mount case).

No `types.ts` change is needed: the payload reuses the exported `TabResponse` interface.

- [ ] **Step 1: Add the listener**

In `useTabManager.ts`, inside the `listeners.push(` call, after the `menu-shortcut` listener entry, add:

```ts
      // A tab the backend created itself: the delayed startup tab, or the
      // Finder "New clitab Tab Here" service. Same id-dedup as createTab —
      // the mount-time list_tabs snapshot may already contain it.
      listen<TabResponse>('tab-created', ({ payload }) => {
        const created = toTab(payload);
        setTabs((prev) =>
          prev.some((tab) => tab.id === created.id)
            ? prev.map((tab) => (tab.id === created.id ? created : tab))
            : [...prev, created]
        );
        setActiveTabId(created.id);
      })
```

- [ ] **Step 2: Verify**

Run: `npm run typecheck`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add src/hooks/useTabManager.ts
git commit -m "$(cat <<'EOF'
feat: renderer merges backend-created tabs (tab-created event)

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: `Info.plist` + packaged-build verification

**Files:**
- Create: `src-tauri/Info.plist`

**Interfaces:**
- Consumes: nothing.
- Produces: the bundle-level service declaration LaunchServices needs; without it the provider registered in Task 4 never receives requests.

- [ ] **Step 1: Create `src-tauri/Info.plist`**

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>NSServices</key>
  <array>
    <dict>
      <key>NSMessage</key>
      <string>openTab</string>
      <key>NSMenuItem</key>
      <dict>
        <key>default</key>
        <string>New clitab Tab Here</string>
      </dict>
      <key>NSRequiredContext</key>
      <dict>
        <key>NSTextContent</key>
        <string>FilePath</string>
      </dict>
      <key>NSSendTypes</key>
      <array>
        <string>NSFilenamesPboardType</string>
        <string>public.file-url</string>
        <string>public.plain-text</string>
      </array>
    </dict>
  </array>
</dict>
</plist>
```

(Ghostty's two `NSSendTypes` plus `public.file-url`, the type the handler reads first.)

- [ ] **Step 2: Build the bundle**

Run: `npm run package:macos`
Expected: succeeds (ad-hoc signed .app + DMG under `src-tauri/target/release/bundle/`).

- [ ] **Step 3: Verify the plist merged**

Run: `/usr/libexec/PlistBuddy -c 'Print :NSServices' src-tauri/target/release/bundle/macos/clitab.app/Contents/Info.plist`
Expected: the array from Step 1 (proves Tauri merged `src-tauri/Info.plist` and nothing clobbered it). If the key is missing, stop — check the tauri-cli version's merge behavior before proceeding.

- [ ] **Step 4: Register with LaunchServices**

Run: `/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f src-tauri/target/release/bundle/macos/clitab.app`
Expected: silent success. (Installing to /Applications and launching once has the same effect; Task 8 assumes the app under test is this build or a copy in /Applications.)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/Info.plist
git commit -m "$(cat <<'EOF'
feat: declare the NSServices entry for Finder "New clitab Tab Here"

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Docs — READMEs + CLAUDE.md

**Files:**
- Modify: `README.md` (Features list)
- Modify: `README.zh-CN.md` (same bullet, in sync)
- Modify: `CLAUDE.md` (events list + backend file map)

**Interfaces:**
- Consumes: the final behavior from Tasks 1–6.
- Produces: accurate docs (project rule: both READMEs stay in sync; CLAUDE.md's event list must match reality).

- [ ] **Step 1: README.md**

Add a bullet to the Features list (after the "Real PTY tabs" bullet):

```markdown
- **Open from Finder** — right-click a folder in Finder and choose
  Services → "New clitab Tab Here" to open a tab in that directory. Works on
  files (opens the containing folder) and on the Finder window background
  (opens the window's folder), whether or not clitab is running.
```

- [ ] **Step 2: README.zh-CN.md**

Matching bullet at the same position:

```markdown
- **从 Finder 打开** — 在 Finder 中右键文件夹,选择 服务 → “New clitab Tab Here”,
  即可在该目录打开标签。对文件(打开其所在目录)和 Finder 窗口空白处
  (打开窗口目录)同样有效,clitab 是否已在运行均可。
```

- [ ] **Step 3: CLAUDE.md**

In the Architecture → Rust backend list, after the `menu.rs` entry, add:

```markdown
- `services.rs` — the Finder "New clitab Tab Here" NSServices integration: pasteboard-text → path classification, the `ServiceState` cold-start handshake (requests during the 400 ms startup grace become the initial tab's cwd), and the objc2-defined `ClitabServices` provider (macOS-gated). Backend-created tabs are announced to the renderer as a `tab-created` event carrying the same `TabResponse` shape as `create_tab`.
```

In the "Events (Rust → renderer)" bullet under Data flow / invariants, add `tab-created` to the list.

- [ ] **Step 4: Commit**

```bash
git add README.md README.zh-CN.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs: document the Finder "New clitab Tab Here" integration

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: Manual Finder verification (requires a human at the GUI)

**Files:** none (verification only).

**Interfaces:**
- Consumes: the packaged app from Task 6 (install to /Applications first: `cp -R src-tauri/target/release/bundle/macos/clitab.app /Applications/` or via the DMG, then launch once and quit so LaunchServices knows it).
- Produces: confirmation of the spec's success criteria.

- [ ] **Step 1: Running-app scenario**

Launch clitab, leave one tab open. In Finder, right-click a folder (e.g. `~/workspace`) → Services (服务) → **New clitab Tab Here**.
Expected: clitab comes to the front; a new focused tab whose title is that directory; existing tabs untouched.

- [ ] **Step 2: Cold-start scenario**

Quit clitab completely. Right-click a different folder → Services → New clitab Tab Here.
Expected: clitab launches with **exactly one** tab, in that directory (no extra home tab).

- [ ] **Step 3: File target**

With clitab running, right-click a *file* (e.g. `README.md`) → Services → New clitab Tab Here.
Expected: new tab in the file's containing directory.

- [ ] **Step 4: Window background**

Open a Finder window in any folder, right-click the empty background → Services → New clitab Tab Here.
Expected: new tab in that window's folder.

- [ ] **Step 5: Non-ASCII directory**

Create `/tmp/clitab 测试 dir`, right-click → New clitab Tab Here.
Expected: tab cwd is exactly that path (title shows it correctly — exercises percent-decoding end to end).

- [ ] **Step 6: If the menu item is missing**

Check System Settings → Keyboard → Keyboard Shortcuts… → Services (服务) — "New clitab Tab Here" should be listed and enabled; re-run Task 6 Step 4 `lsregister -f` against the installed copy and log out/in if it still does not appear. If the item appears but nothing happens, run the app from Terminal (`/Applications/clitab.app/Contents/MacOS/clitab`) and watch stderr while invoking the service — the handler's failure modes are: no path extracted (returns false silently; pasteboard types differ from expectation — report back, do not improvise a fix) vs. tab creation error (dialog + eprintln).

- [ ] **Step 7: Record results**

All six scenarios pass → feature complete. Any failure → capture the scenario number and stderr; that is a debugging task, not a plan step.
