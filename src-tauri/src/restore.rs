//! The tab snapshot: what the sidebar looked like while the app last ran.
//! Written on every snapshot-relevant change (never only at quit — a crash
//! must leave the same state), read once at startup. Same best-effort
//! contract as `last_cwd.rs`: I/O errors are silently ignored.

use crate::status::SessionBinding;
use serde::{Deserialize, Serialize};
use std::path::Path;

const FILE: &str = "tab-snapshot.json";
/// Bumped when the shape changes incompatibly; older/newer files are ignored
/// (a downgrade must not misread fields, an upgrade falls back to last-cwd).
const VERSION: u32 = 1;

/// One restored-to-be tab. Turn state (`status`, notices, answers) is
/// deliberately absent: a restored tab starts protocol-fresh — the resumed
/// agent repaints its own state, and the timeline history comes from the
/// transcript instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotTab {
    pub cwd: String,
    pub title: String,
    pub has_program_title: bool,
    /// The agent session this tab last talked to (hooks installed + used).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionBinding>,
    /// True when the agent CLI was still running at snapshot time; only then
    /// does restore offer a resume (spec D3).
    pub agent_running: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub version: u32,
    /// Index into `tabs` of the tab that was active; `None` → first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<usize>,
    pub tabs: Vec<SnapshotTab>,
}

impl Snapshot {
    pub fn current(tabs: Vec<SnapshotTab>, active: Option<usize>) -> Self {
        Self { version: VERSION, active, tabs }
    }
}

/// Best-effort write. Write-then-rename: a crash mid-write leaves the
/// *previous* snapshot intact instead of a truncated file (rename is atomic
/// within one filesystem).
pub fn save(config_dir: &Path, snapshot: &Snapshot) {
    let _ = std::fs::create_dir_all(config_dir);
    let Ok(json) = serde_json::to_string(snapshot) else {
        return;
    };
    let tmp = config_dir.join(format!("{FILE}.tmp"));
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, config_dir.join(FILE));
    }
}

/// The snapshot from the last run. Corrupt files, wrong versions and I/O
/// failures all mean "nothing to restore" — startup then falls back to the
/// last-cwd flow.
pub fn load(config_dir: &Path) -> Option<Snapshot> {
    let json = std::fs::read_to_string(config_dir.join(FILE)).ok()?;
    let snapshot: Snapshot = serde_json::from_str(&json).ok()?;
    (snapshot.version == VERSION).then_some(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{AgentKind, SessionBinding};
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clitab-restore-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixture() -> Snapshot {
        Snapshot::current(
            vec![
                SnapshotTab {
                    cwd: "/a".into(),
                    title: "/a".into(),
                    has_program_title: false,
                    session: None,
                    agent_running: false,
                },
                SnapshotTab {
                    cwd: "/b".into(),
                    title: "Fix build".into(),
                    has_program_title: true,
                    session: Some(SessionBinding {
                        agent: AgentKind::Claude,
                        sid: "s-1".into(),
                        transcript: Some("/b/s-1.jsonl".into()),
                    }),
                    agent_running: true,
                },
            ],
            Some(1),
        )
    }

    #[test]
    fn snapshots_round_trip_through_disk() {
        let dir = temp_dir();
        save(&dir, &fixture());
        assert_eq!(load(&dir).unwrap(), fixture());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_is_nothing_to_restore() {
        let dir = temp_dir();
        assert!(load(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A crash mid-write leaves a truncated file: load must report "nothing
    /// to restore", never an error (startup falls back to the last-cwd path).
    #[test]
    fn corrupt_snapshot_is_ignored() {
        let dir = temp_dir();
        save(&dir, &fixture());
        std::fs::write(dir.join("tab-snapshot.json"), r#"{"version":1,"tabs":[{"cwd":"/a""#)
            .unwrap();
        assert!(load(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wrong_version_is_ignored() {
        let dir = temp_dir();
        std::fs::write(dir.join("tab-snapshot.json"), r#"{"version":99,"tabs":[]}"#).unwrap();
        assert!(load(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
