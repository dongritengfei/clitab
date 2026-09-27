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

use tauri::menu::{Menu, MenuBuilder, MenuItem, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Runtime};

pub const NEW_TAB: &str = "new-tab";
pub const CLOSE_TAB: &str = "close-tab";
pub const NEXT_TAB: &str = "next-tab";
pub const PREV_TAB: &str = "prev-tab";
pub const SELECT_TAB: &str = "select-tab";

/// Menu ids we forward to the renderer. Everything else (quit, copy, paste,
/// minimize, ...) is handled natively by predefined menu items.
fn is_tab_action(id: &str) -> bool {
    matches!(id, NEW_TAB | CLOSE_TAB | NEXT_TAB | PREV_TAB)
        || (id.starts_with(SELECT_TAB) && id[SELECT_TAB.len()..].starts_with('-'))
}

pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let mut menu = MenuBuilder::new(app);

    #[cfg(target_os = "macos")]
    {
        let app_menu = SubmenuBuilder::new(app, "clitab")
            .about(None)
            .separator()
            .services()
            .hide()
            .hide_others()
            .show_all()
            .separator()
            .quit()
            .build()?;
        menu = menu.item(&app_menu);
    }

    let new_tab = item(app, NEW_TAB, "New Tab", "CmdOrCtrl+T")?;
    let close_tab = item(app, CLOSE_TAB, "Close Tab", "CmdOrCtrl+W")?;
    let next_tab = item(app, NEXT_TAB, "Next Tab", "Control+Tab")?;
    let prev_tab = item(app, PREV_TAB, "Previous Tab", "Control+Shift+Tab")?;

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
        .separator();
    for tab in &indexed {
        tabs = tabs.item(tab);
    }
    let tabs = tabs.build()?;
    menu = menu.item(&tabs);

    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    menu = menu.item(&edit);

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
    if !is_tab_action(id) {
        return;
    }
    let _ = app.emit("menu-shortcut", serde_json::json!({ "id": id }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_actions_are_forwarded() {
        assert!(is_tab_action(NEW_TAB));
        assert!(is_tab_action(CLOSE_TAB));
        assert!(is_tab_action(NEXT_TAB));
        assert!(is_tab_action(PREV_TAB));
        assert!(is_tab_action("select-tab-1"));
        assert!(is_tab_action("select-tab-9"));
    }

    #[test]
    fn predefined_items_are_left_to_the_platform() {
        assert!(!is_tab_action("tauri::quit"));
        assert!(!is_tab_action("tauri::copy"));
        assert!(!is_tab_action("tauri::minimize"));
        // Guard against a prefix that is not one of our numbered ids.
        assert!(!is_tab_action(SELECT_TAB));
        assert!(!is_tab_action("select-tab"));
    }
}
