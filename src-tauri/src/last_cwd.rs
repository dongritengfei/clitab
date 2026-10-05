//! The last working directory, remembered across app restarts. A normal
//! launch reopens it as the first tab's cwd; a Finder "New clitab Tab Here"
//! request always wins over it (see the startup thread in `lib.rs`).
//! Best-effort by design: an unwritable config dir must never break a
//! terminal, so every I/O error is silently ignored.

use std::path::{Path, PathBuf};

/// The config dir holding the `last-cwd` file, or `None` when Tauri cannot
/// resolve one (callers then skip persistence).
pub fn dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    use tauri::Manager;
    app.path().app_config_dir().ok()
}

const FILE: &str = "last-cwd";

/// Remember `cwd` as the directory the next normal launch should start in.
pub fn save(config_dir: &Path, cwd: &str) {
    let _ = std::fs::create_dir_all(config_dir);
    let _ = std::fs::write(config_dir.join(FILE), format!("{cwd}\n"));
}

/// The directory remembered by the last [`save`], if any. Stale paths are
/// fine to return: `resolve_cwd` in the manager validates and falls back to
/// the default, same as for any other requested cwd.
pub fn load(config_dir: &Path) -> Option<String> {
    // The trim matters: `save` writes a POSIX text line, and a trailing
    // newline would fail `resolve_cwd`'s is_dir check.
    let cwd = std::fs::read_to_string(config_dir.join(FILE)).ok()?;
    let cwd = cwd.trim();
    (!cwd.is_empty()).then(|| cwd.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets its own config dir; the uuid keeps parallel tests apart.
    fn temp_config_dir() -> PathBuf {
        std::env::temp_dir().join(format!("clitab-last-cwd-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn saved_directory_round_trips() {
        let dir = temp_config_dir();
        save(&dir, "/tmp/a project");
        assert_eq!(load(&dir).as_deref(), Some("/tmp/a project"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_without_a_save_is_none() {
        // First launch ever: no file yet, so startup keeps its old default.
        let dir = temp_config_dir();
        assert_eq!(load(&dir), None);
    }

    #[test]
    fn the_last_save_wins() {
        let dir = temp_config_dir();
        save(&dir, "/tmp/one");
        save(&dir, "/tmp/two");
        assert_eq!(load(&dir).as_deref(), Some("/tmp/two"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
