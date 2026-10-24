//! Native application menu: the keyboard shortcuts for tabs.
//!
//! Doing this here rather than with `keydown` handlers in the renderer is what
//! makes the shortcuts behave like a desktop app:
//!
//!   * Tauri's default macOS menu binds ⌘W to "Close Window", which would quit
//!     the app instead of closing a tab. Providing our own menu replaces it.
//!   * The Edit submenu keeps ⌘C / ⌘V / ⌘A working, which xterm.js needs for
//!     copy/paste in a webview.
//!   * Accelerators fire even when the webview does not have keyboard focus.
//!
//! Menu clicks are forwarded to the renderer as a `menu-shortcut` event: the
//! renderer owns tab state, so it decides what "close-tab" actually means.

use std::sync::OnceLock;
use tauri::menu::{
    CheckMenuItem, CheckMenuItemBuilder, Menu, MenuBuilder, MenuItem, MenuItemBuilder,
    SubmenuBuilder,
};
use tauri::{AppHandle, Emitter, Runtime};

pub const QUIT: &str = "quit";
pub const NEW_TAB: &str = "new-tab";
pub const CLOSE_TAB: &str = "close-tab";
pub const NEXT_TAB: &str = "next-tab";
pub const PREV_TAB: &str = "prev-tab";
pub const SELECT_TAB: &str = "select-tab";
pub const NEXT_WAITING: &str = "next-waiting";
pub const FIND: &str = "find";
pub const ZOOM_IN: &str = "zoom-in";
pub const ZOOM_OUT: &str = "zoom-out";
pub const ZOOM_RESET: &str = "zoom-reset";
pub const INPUT_STRIP: &str = "input-strip";
pub const INPUT_STRIP_FOCUS: &str = "input-strip-focus";

/// Runtime handle to the "Pinned Input Strip" checkbox. The enabled pref
/// lives in the renderer (localStorage), so the renderer reports it — at
/// startup and on every toggle — through `set_input_strip_checked`. The
/// type-erased setter keeps `build` generic over the runtime.
static INPUT_STRIP_CHECK: OnceLock<Box<dyn Fn(bool) + Send + Sync>> = OnceLock::new();

pub fn set_input_strip_checked(checked: bool) {
    if let Some(set) = INPUT_STRIP_CHECK.get() {
        set(checked);
    }
}

/// Menu ids we forward to the renderer (`FIND` opens its terminal search
/// bar; the zoom items adjust the renderer's terminal font size; the
/// input-strip items control the pinned input strip). Everything else is
/// handled natively: `QUIT` in `lib.rs` (quit confirmation),
/// copy/paste/minimize/... by predefined menu items.
fn is_renderer_action(id: &str) -> bool {
    matches!(
        id,
        NEW_TAB
            | CLOSE_TAB
            | NEXT_TAB
            | PREV_TAB
            | NEXT_WAITING
            | FIND
            | ZOOM_IN
            | ZOOM_OUT
            | ZOOM_RESET
            | INPUT_STRIP
            | INPUT_STRIP_FOCUS
    ) || (id.starts_with(SELECT_TAB) && id[SELECT_TAB.len()..].starts_with('-'))
}

pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let mut menu = MenuBuilder::new(app);

    #[cfg(target_os = "macos")]
    {
        // A custom Quit item instead of the predefined one: the predefined
        // item sends `terminate:` straight to NSApplication, which bypasses
        // `RunEvent::ExitRequested` entirely (tao has no
        // `applicationShouldTerminate:`), leaving no chance to confirm. The
        // custom id arrives in `on_menu_event`, where lib.rs asks first.
        let quit = item(app, QUIT, "Quit clitab", "CmdOrCtrl+Q")?;
        let app_menu = SubmenuBuilder::new(app, "clitab")
            .about(None)
            .separator()
            .services()
            .hide()
            .hide_others()
            .show_all()
            .separator()
            .item(&quit)
            .build()?;
        menu = menu.item(&app_menu);
    }

    let new_tab = item(app, NEW_TAB, "New Tab", "CmdOrCtrl+T")?;
    let close_tab = item(app, CLOSE_TAB, "Close Tab", "CmdOrCtrl+W")?;
    let next_tab = item(app, NEXT_TAB, "Next Tab", "Control+Tab")?;
    let prev_tab = item(app, PREV_TAB, "Previous Tab", "Control+Shift+Tab")?;
    let next_waiting = item(app, NEXT_WAITING, "Next Waiting Tab", "CmdOrCtrl+J")?;

    // ⌘1 … ⌘9 jump straight to a tab.
    let mut indexed: Vec<MenuItem<R>> = Vec::with_capacity(9);
    for number in 1..=9usize {
        indexed.push(item(
            app,
            &format!("{SELECT_TAB}-{number}"),
            &format!("Tab {number}"),
            &format!("CmdOrCtrl+{number}"),
        )?);
    }

    let mut tabs = SubmenuBuilder::new(app, "Tabs")
        .item(&new_tab)
        .item(&close_tab)
        .separator()
        .item(&next_tab)
        .item(&prev_tab)
        .item(&next_waiting)
        .separator();
    for tab in &indexed {
        tabs = tabs.item(tab);
    }
    let tabs = tabs.build()?;
    menu = menu.item(&tabs);

    // A custom Find item (the platform has no predefined one): ⌘F is owned
    // by the renderer's terminal search bar, not by WKWebView's page search.
    let find = item(app, FIND, "Find…", "CmdOrCtrl+F")?;

    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .separator()
        .item(&find)
        .build()?;
    menu = menu.item(&edit);

    // Terminal font zoom, the standard macOS terminal gesture (iTerm2/Ghostty).
    // Accelerators are muda code names: "Equal" is the physical key that reads
    // ⌘+ unshifted, so Zoom In shows as ⌘= and matches the code, not the
    // shifted glyph (⌘⇧= does not fire). The renderer owns the font size; it
    // clamps and persists it.
    let zoom_in = item(app, ZOOM_IN, "Zoom In", "CmdOrCtrl+Equal")?;
    let zoom_out = item(app, ZOOM_OUT, "Zoom Out", "CmdOrCtrl+Minus")?;
    let zoom_reset = item(app, ZOOM_RESET, "Actual Size", "CmdOrCtrl+Digit0")?;

    // The pinned input strip gets two entries because click and accelerator
    // are indistinguishable in one menu item, and they mean different things:
    // the checkbox is the pure on/off switch (no accelerator), while ⌘⇧B is
    // the context-dependent focus cycle (enable-and-focus / focus the strip /
    // return focus to the terminal) — interpreted by the renderer, like every
    // other shortcut's meaning. Built checked: the feature defaults on, and
    // the renderer reports its persisted pref at startup.
    let input_strip = CheckMenuItemBuilder::with_id(INPUT_STRIP, "Pinned Input Strip")
        .checked(true)
        .build(app)?;
    {
        let handle: CheckMenuItem<R> = input_strip.clone();
        let _ = INPUT_STRIP_CHECK.set(Box::new(move |checked: bool| {
            let _ = handle.set_checked(checked);
        }));
    }
    let input_strip_focus =
        item(app, INPUT_STRIP_FOCUS, "Focus Input Strip", "CmdOrCtrl+Shift+B")?;

    let view = SubmenuBuilder::new(app, "View")
        .item(&zoom_in)
        .item(&zoom_out)
        .item(&zoom_reset)
        .separator()
        .item(&input_strip)
        .item(&input_strip_focus)
        .build()?;
    menu = menu.item(&view);

    #[cfg(target_os = "macos")]
    {
        let window = SubmenuBuilder::new(app, "Window")
            .minimize()
            .maximize()
            .fullscreen()
            .build()?;
        menu = menu.item(&window);
    }

    menu.build()
}

fn item<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    text: &str,
    accelerator: &str,
) -> tauri::Result<MenuItem<R>> {
    MenuItemBuilder::with_id(id, text)
        .accelerator(accelerator)
        .build(app)
}

/// Forward a menu click to the renderer.
pub fn forward(app: &AppHandle, id: &str) {
    if !is_renderer_action(id) {
        return;
    }
    let _ = app.emit("menu-shortcut", serde_json::json!({ "id": id }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_actions_are_forwarded() {
        assert!(is_renderer_action(NEW_TAB));
        assert!(is_renderer_action(CLOSE_TAB));
        assert!(is_renderer_action(NEXT_TAB));
        assert!(is_renderer_action(PREV_TAB));
        assert!(is_renderer_action(NEXT_WAITING));
        assert!(is_renderer_action(FIND));
        assert!(is_renderer_action(ZOOM_IN));
        assert!(is_renderer_action(ZOOM_OUT));
        assert!(is_renderer_action(ZOOM_RESET));
        assert!(is_renderer_action(INPUT_STRIP));
        assert!(is_renderer_action(INPUT_STRIP_FOCUS));
        assert!(is_renderer_action("select-tab-1"));
        assert!(is_renderer_action("select-tab-9"));
    }

    #[test]
    fn non_renderer_items_are_left_to_the_platform() {
        // Quit is handled in lib.rs (confirmation), never forwarded.
        assert!(!is_renderer_action(QUIT));
        assert!(!is_renderer_action("tauri::copy"));
        assert!(!is_renderer_action("tauri::minimize"));
        // Guard against a prefix that is not one of our numbered ids.
        assert!(!is_renderer_action(SELECT_TAB));
        assert!(!is_renderer_action("select-tab"));
    }
}
