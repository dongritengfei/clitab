//! The tab snapshot: what the sidebar looked like while the app last ran.
//! Written on every snapshot-relevant change (never only at quit — a crash
//! must leave the same state), read once at startup. Same best-effort
//! contract as `last_cwd.rs`: I/O errors are silently ignored.

use crate::status::{AgentKind, SessionBinding};
use serde::{Deserialize, Deserializer, Serialize};
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
    /// Deserialized tolerantly: a hostile or corrupt binding (the sid fails
    /// the charset guard, unknown agent kind, malformed JSON) degrades to
    /// `None` — the tab still restores as a plain shell, the snapshot does
    /// not fail wholesale.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_tolerant_session"
    )]
    pub session: Option<SessionBinding>,
    /// True when the agent CLI was still running at snapshot time; only then
    /// does restore offer a resume (spec D3).
    pub agent_running: bool,
    /// The agent binary the shell integration last saw launched in this tab
    /// (strict preexec match). Lets restore relaunch a never-prompted agent
    /// — no hook binding ever existed for it. Tolerant like `session`
    /// (unknown kind → `None`), and absent in older snapshots — same
    /// degradation, no version bump needed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_tolerant_agent"
    )]
    pub agent: Option<AgentKind>,
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

/// Tolerant deserialize for `SnapshotTab.session`: tries the real
/// `SessionBinding` deserialize (which enforces the sid charset guard), and
/// on ANY serde error — hostile sid, unknown agent, malformed payload —
/// falls back to `None`. A corrupt binding must degrade to "no session",
/// never sink the whole tab or the snapshot file. The serde error type
/// requires `IntoDeserializer` juggling; `Option::deserialize` + explicit
/// error swallowing keeps the call site honest.
fn deserialize_tolerant_session<'de, D>(
    deserializer: D,
) -> Result<Option<SessionBinding>, D::Error>
where
    D: Deserializer<'de>,
{
    // Deserialize as a generic Value first so we can swallow binding-level
    // errors while still rejecting structurally-wrong payloads (null → None
    // falls through the Option deserialize naturally).
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(None);
    };
    match serde_json::from_value::<SessionBinding>(value) {
        Ok(binding) => Ok(Some(binding)),
        Err(_) => Ok(None),
    }
}

/// Same tolerance policy as the session, one level simpler: an unknown or
/// malformed agent kind becomes `None` (restore then declines the plain
/// relaunch), never a snapshot-wide failure.
fn deserialize_tolerant_agent<'de, D>(deserializer: D) -> Result<Option<AgentKind>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(None);
    };
    Ok(serde_json::from_value::<AgentKind>(value).ok())
}

/// Best-effort write. Write-then-rename: a crash mid-write leaves the
/// *previous* snapshot intact instead of a truncated file (rename is atomic
/// within one filesystem). The tmp file carries a per-save unique suffix
/// (monotonic nanos from `SystemTime`) so two persisting threads cannot
/// interleave writes into the same `.tmp` and have one rename promote a
/// mixed file.
pub fn save(config_dir: &Path, snapshot: &Snapshot) {
    let _ = std::fs::create_dir_all(config_dir);
    let Ok(json) = serde_json::to_string(snapshot) else {
        return;
    };
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = config_dir.join(format!("{FILE}.tmp.{suffix:x}"));
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
                    agent: None,
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
                    agent: Some(AgentKind::Claude),
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

    /// A snapshot written by an older build has no `agent` field: it must
    /// still load (agent → None, no plain relaunch offered).
    #[test]
    fn older_snapshots_without_agent_still_load() {
        let dir = temp_dir();
        std::fs::write(
            dir.join("tab-snapshot.json"),
            r#"{"version":1,"active":0,"tabs":[{"cwd":"/a","title":"/a","hasProgramTitle":false,"agentRunning":true}]}"#,
        )
        .unwrap();
        let snapshot = load(&dir).expect("pre-agent snapshots must load");
        assert_eq!(snapshot.tabs[0].agent, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Same tolerance policy as `session`: an unknown agent kind degrades to
    /// None for that tab instead of sinking the whole snapshot.
    #[test]
    fn unknown_agent_kind_degrades_to_none() {
        let dir = temp_dir();
        std::fs::write(
            dir.join("tab-snapshot.json"),
            r#"{"version":1,"tabs":[{"cwd":"/a","title":"/a","hasProgramTitle":false,"agentRunning":true,"agent":"gemini"}]}"#,
        )
        .unwrap();
        let snapshot = load(&dir).expect("an unknown agent must not sink the snapshot");
        assert_eq!(snapshot.tabs[0].agent, None);
        assert_eq!(snapshot.tabs[0].cwd, "/a");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wrong_version_is_ignored() {
        let dir = temp_dir();
        std::fs::write(dir.join("tab-snapshot.json"), r#"{"version":99,"tabs":[]}"#).unwrap();
        assert!(load(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Fix 1 — snapshot-level guard: a hostile sid in a tab's binding must
    /// drop *that binding only*, not the whole tab. Other fields (cwd, title,
    /// flags) must survive intact so a tampered snapshot degrades to "plain
    /// shell tab" rather than "no restore at all".
    #[test]
    fn hostile_sid_is_dropped_but_the_tab_survives() {
        let dir = temp_dir();
        let json = r#"{
            "version":1,
            "tabs":[
                {"cwd":"/a","title":"/a","hasProgramTitle":false,"agentRunning":false,
                 "session":{"agent":"claude","sid":"a; rm -rf /"}},
                {"cwd":"/b","title":"Fix build","hasProgramTitle":true,"agentRunning":true,
                 "session":{"agent":"claude","sid":"valid-sid-1"}}
            ],
            "active":1
        }"#;
        std::fs::write(dir.join("tab-snapshot.json"), json).unwrap();
        let snapshot = load(&dir).expect("snapshot loads despite hostile sid");
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.tabs[0].cwd, "/a");
        assert_eq!(snapshot.tabs[0].title, "/a");
        assert!(
            snapshot.tabs[0].session.is_none(),
            "hostile sid dropped the binding, not the tab"
        );
        assert_eq!(
            snapshot.tabs[1].session.as_ref().map(|b| b.sid.as_str()),
            Some("valid-sid-1"),
            "sibling tab's binding round-trips"
        );
        // A newline in the sid is the same class of hostile payload.
        // Use the JSON escape `\n` (raw-string `\\n`) so the file stays
        // valid JSON — a literal newline would fail structurally, not at
        // the sid guard.
        let json_newline = json.replace("a; rm -rf /", "a\\nrm -rf /");
        std::fs::write(dir.join("tab-snapshot.json"), json_newline.as_str()).unwrap();
        let snapshot = load(&dir).expect("newline sid also degrades to None");
        assert!(snapshot.tabs[0].session.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
