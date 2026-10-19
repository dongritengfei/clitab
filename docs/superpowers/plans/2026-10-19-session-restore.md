# Session Restore Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Relaunching clitab restores the previous run's tabs (order, titles, cwds) and resumes the agent sessions — the previously-active tab automatically at startup, the others on first activation — with the resumed session's earlier user prompts shown as display-only timeline history.

**Architecture:** The prompt hook gains a session binding (`sid`/`tp`/`a`) decoded in `status.rs` and tracked per tab in the `Registry`, which now continuously writes a crash-safe tab snapshot (`restore.rs`). At startup, `TabManager::restore_snapshot` recreates the tabs and injects `<agent> --resume <sid>` into the shell of the tab that was active; other restored tabs lazy-resume via a new `resume_tab` command on first activation. After a resume, `history.rs` streams the session transcript JSONL for real user prompts, which the renderer prepends to the timeline as non-navigable `historical` rows.

**Tech Stack:** Rust (Tauri 2, serde, portable-pty, new: chrono 0.4), TypeScript/React 18 (xterm.js unchanged).

**Spec:** `docs/superpowers/specs/2026-10-19-session-restore-design.md`

## Global Constraints

- Work only in the worktree `/Users/yiyi/workspace/github/clitab/.claude/worktrees/feat+session-restore`. Never bare `git stash` (shared stack) — use WIP commits if you must set work aside.
- The ONLY new Rust dependency is `chrono = { version = "0.4", default-features = false, features = ["std"] }`. No new frontend dependencies, no frontend test runner, no linter — `npm run typecheck` is the frontend gate.
- Test commands: full suite `npm test` (= `cargo test --lib`); single test `cargo test --manifest-path src-tauri/Cargo.toml --lib <name>`; typecheck `npm run typecheck`. Baseline before Task 1: 109 tests pass. Every task ends with the full suite green.
- IPC responses serialize camelCase (serde `rename_all = "camelCase"`); Rust→renderer event payload keys stay snake_case (`tab_id`) per existing convention.
- session.rs invariants: never move the `pty-output` emit outside the stream lock; never reorder parse-after-emit (chunks are split at OSC offsets and each segment is emitted before its event is announced).
- Registry locking rule (new, Task 4): never hold two registry locks at the same time — clone-and-release (`snapshot()` does exactly this).
- Hook JSON changes must keep the wiki's `Agent-Setup.md` and `Agent-Setup.zh.md` JSON blocks byte-identical (wiki cloned at `/tmp/clitab-wiki`; commit there but do NOT push without asking the user).
- Commits follow repo history style (conventional prefixes), each ending with a `Co-Authored-By: Claude Code <noreply@anthropic.com>` trailer:

```bash
git commit -m "$(cat <<'EOF'
feat: <subject>

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

## Review Focus

1. **Snapshot file corrupted by a crash mid-write** → next launch treats it as "nothing to restore" and falls back to the last-cwd startup; never errors, never panics. (`save` is tmp+rename so this is defense-in-depth.) Pinned by `restore::tests::corrupt_snapshot_is_ignored` (Task 3).
2. **Transcript with malformed JSONL** (truncated tail line, non-JSON garbage, records missing `timestamp`/`message` fields) → those lines are skipped, real prompts still extracted. Pinned by `history::tests::extracts_only_real_user_prompts` (Task 2).
3. **Hostile/broken hook sends a `sid` containing shell metacharacters** → the binding is rejected at decode; it can never reach the injected resume command line. Pinned by `status::tests::session_binding_rejects_unusable_fields` (Task 1).
4. **Old hook install / missing jq** (payload without `sid`/`tp`/`a`) → everything keeps working exactly as before; tabs simply restore as plain shells. Pinned by `status::tests::legacy_payloads_have_no_binding_but_keep_working` (Task 1).
5. **Session transcript deleted between quit and relaunch** → the resume is silently cancelled, the tab stays a usable shell, a stale program title reverts to the cwd, and the renderer is not told to load history. Pinned by `registry::tests::cancel_resume_reverts_to_a_plain_shell_tab` (Task 4) plus the transcript gate in `resume_tab` (Task 6), verified end-to-end in Task 10 step 7.

---

### Task 1: Session binding decode (`status.rs`)

**Files:**
- Modify: `src-tauri/src/status.rs` (add types + `decode_session` after the existing `decode()` fn; add tests inside the existing `mod tests`)

**Interfaces:**
- Consumes: nothing new (serde/serde_json already imported in the file).
- Produces:
  - `pub enum AgentKind { Claude, Qoder }` — `Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize`, serde `rename_all = "lowercase"`; method `pub fn cmd(&self) -> &'static str` → `"claude"` / `"qoder"`.
  - `pub struct SessionBinding { pub agent: AgentKind, pub sid: String, pub transcript: Option<String> }` — `Clone, PartialEq, Eq, Debug, Serialize, Deserialize`, serde `rename_all = "camelCase"`, `transcript` has `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - `pub fn decode_session(json: &str) -> Option<SessionBinding>`.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `src-tauri/src/status.rs`:

```rust
#[test]
fn session_binding_decodes_from_the_prompt_payload() {
    let b = decode_session(
        r#"{"e":"prompt","msg":"hi","sid":"6f9a1b2c-0d3e","tp":"/Users/x/.claude/projects/-Users-x/6f9a1b2c.jsonl","a":"claude"}"#,
    )
    .unwrap();
    assert_eq!(b.agent, AgentKind::Claude);
    assert_eq!(b.agent.cmd(), "claude");
    assert_eq!(b.sid, "6f9a1b2c-0d3e");
    assert_eq!(
        b.transcript.as_deref(),
        Some("/Users/x/.claude/projects/-Users-x/6f9a1b2c.jsonl")
    );
    // The status event itself is unchanged by the extra fields.
    assert_eq!(
        decode(r#"{"e":"prompt","msg":"hi","sid":"6f9a1b2c-0d3e","a":"claude"}"#),
        Some(StatusEvent::Prompt { msg: Some("hi".into()) })
    );
}

#[test]
fn qoder_binding_decodes_with_its_own_command() {
    let b = decode_session(r#"{"e":"prompt","sid":"abc123","a":"qoder"}"#).unwrap();
    assert_eq!(b.agent, AgentKind::Qoder);
    assert_eq!(b.agent.cmd(), "qoder");
    assert_eq!(b.transcript, None, "tp is optional; history derives a path");
}

/// The sid is interpolated into a shell command line at resume time, so the
/// decoder is the injection guard: anything outside [A-Za-z0-9-] is rejected.
#[test]
fn session_binding_rejects_unusable_fields() {
    assert_eq!(decode_session(r#"{"e":"prompt","sid":"abc"}"#), None, "no agent kind");
    assert_eq!(decode_session(r#"{"e":"prompt","a":"claude"}"#), None, "no sid");
    assert_eq!(decode_session(r#"{"e":"prompt","a":"claude","sid":""}"#), None);
    assert_eq!(decode_session(r#"{"e":"prompt","a":"claude","sid":"a; rm -rf /"}"#), None);
    assert_eq!(decode_session(r#"{"e":"prompt","a":"claude","sid":"$(whoami)"}"#), None);
    assert_eq!(decode_session(r#"{"e":"prompt","a":"cursor","sid":"abc"}"#), None, "unknown agent");
    assert_eq!(decode_session("not json"), None);
}

#[test]
fn empty_transcript_path_is_normalized_to_none() {
    let b = decode_session(r#"{"e":"prompt","a":"claude","sid":"abc","tp":""}"#).unwrap();
    assert_eq!(b.transcript, None);
}

/// Payloads from old hook installs (no sid/tp/a) keep decoding as before —
/// session restore degrades to plain-shell tabs, never to an error.
#[test]
fn legacy_payloads_have_no_binding_but_keep_working() {
    assert_eq!(decode_session(r#"{"e":"prompt","msg":"hi"}"#), None);
    assert_eq!(
        decode(r#"{"e":"prompt","msg":"hi"}"#),
        Some(StatusEvent::Prompt { msg: Some("hi".into()) })
    );
}

/// The snapshot file round-trips bindings through serde.
#[test]
fn session_binding_round_trips_through_json() {
    let b = SessionBinding {
        agent: AgentKind::Qoder,
        sid: "s-1".into(),
        transcript: Some("/tp.jsonl".into()),
    };
    let json = serde_json::to_string(&b).unwrap();
    assert_eq!(serde_json::from_str::<SessionBinding>(&json).unwrap(), b);
    assert!(json.contains(r#""agent":"qoder""#), "{json}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib status::tests`
Expected: FAIL — `cannot find function decode_session`, `cannot find type SessionBinding / AgentKind`.

- [ ] **Step 3: Implement**

In `src-tauri/src/status.rs`, insert after the `decode()` function (before `is_injected_prompt`):

```rust
/// Which agent CLI a tab's session belongs to; `cmd` is the binary name the
/// resume injection types into the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Qoder,
}

impl AgentKind {
    pub fn cmd(&self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Qoder => "qoder",
        }
    }
}

/// The agent session a tab is currently talking to, learned from the prompt
/// hook's `sid`/`tp`/`a` fields. Persisted into the tab snapshot so the next
/// launch can `--resume` it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBinding {
    pub agent: AgentKind,
    pub sid: String,
    /// Hook-supplied transcript path; `None` when the hook could not provide
    /// it (history then derives the conventional projects-dir path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
}

/// Decode the session-binding side of a payload (in practice only
/// UserPromptSubmit carries it). `None` when the hook did not send a usable
/// binding — old install, missing jq, unknown agent — and callers keep the
/// previous binding then. The sid charset guard is load-bearing: the id is
/// interpolated into a shell command line at resume time.
pub fn decode_session(json: &str) -> Option<SessionBinding> {
    #[derive(Deserialize)]
    struct SessionWire {
        #[serde(default)]
        sid: Option<String>,
        #[serde(default)]
        tp: Option<String>,
        #[serde(default)]
        a: Option<String>,
    }
    let wire: SessionWire = serde_json::from_str(json).ok()?;
    let agent = match wire.a?.as_str() {
        "claude" => AgentKind::Claude,
        "qoder" => AgentKind::Qoder,
        _ => return None,
    };
    let sid = wire.sid?;
    if sid.is_empty() || !sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    Some(SessionBinding {
        agent,
        sid,
        transcript: wire.tp.filter(|tp| !tp.is_empty()),
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib status::tests`
Expected: PASS (all status tests, old and new).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/status.rs
git commit -m "$(cat <<'EOF'
feat: decode agent session bindings from hook payloads

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Transcript history parsing (`history.rs` + chrono dep)

**Files:**
- Modify: `src-tauri/Cargo.toml` (add the chrono dependency)
- Create: `src-tauri/src/history.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod history;` to the module list — alphabetical, after `mod attention;`)

**Interfaces:**
- Consumes: `crate::status::{AgentKind, SessionBinding}` (Task 1).
- Produces:
  - `pub const HISTORY_CAP: usize = 500;`
  - `pub struct HistoryPrompt { pub at: u64, pub msg: String }` — `Clone, Debug, Serialize`, serde `rename_all = "camelCase"` (crosses IPC as `{at, msg}`).
  - `pub fn transcript_for(binding: &SessionBinding, cwd: &str) -> Option<PathBuf>`
  - `pub fn user_prompts(path: &Path, cap: usize) -> Vec<HistoryPrompt>` — oldest first, at most `cap` (the newest).

- [ ] **Step 1: Add the dependency**

In `src-tauri/Cargo.toml`, after the `base64 = "0.22"` line in `[dependencies]`:

```toml
chrono = { version = "0.4", default-features = false, features = ["std"] }
```

- [ ] **Step 2: Write the failing tests**

Create `src-tauri/src/history.rs` containing ONLY the module doc and the test module (the implementation — and with it every import — comes in step 4; the file must not compile yet):

```rust
//! Historical user prompts from an agent session transcript (JSONL), parsed
//! after a resume so the timeline can show what the user submitted before
//! this app run. Streaming by design: transcripts reach 50 MB, and only the
//! tail (last `cap` prompts) is ever shown.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{AgentKind, SessionBinding};
    use chrono::DateTime;
    use std::path::PathBuf;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("clitab-history-{tag}-{}.jsonl", uuid::Uuid::new_v4()))
    }

    fn user_line(text: &str, ts: &str) -> String {
        format!(r#"{{"type":"user","timestamp":"{ts}","message":{{"role":"user","content":"{text}"}}}}"#)
    }

    #[test]
    fn extracts_only_real_user_prompts() {
        let path = temp_path("extract");
        let ts = "2026-10-19T08:30:00.000Z";
        let expected_at = DateTime::parse_from_rfc3339(ts).unwrap().timestamp_millis() as u64;
        let lines = vec![
            user_line("fix the bug", ts),
            // tool_result: content is an array, never a typed prompt
            r#"{"type":"user","timestamp":"2026-10-19T08:31:00.000Z","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#.to_string(),
            // wrappers the agent injects through the user-prompt pipeline
            user_line("<task-notification>x</task-notification>", "2026-10-19T08:32:00.000Z"),
            user_line("<local-command-stdout>done</local-command-stdout>", "2026-10-19T08:33:00.000Z"),
            user_line("<command-name>/clear</command-name>", "2026-10-19T08:33:30.000Z"),
            // assistant record
            r#"{"type":"assistant","timestamp":"2026-10-19T08:34:00.000Z","message":{"role":"assistant","content":"hi"}}"#.to_string(),
            // sidechain (subagent) prompt
            r#"{"type":"user","isSidechain":true,"timestamp":"2026-10-19T08:35:00.000Z","message":{"role":"user","content":"agent prompt"}}"#.to_string(),
            // garbage and a truncated tail line (crash mid-write)
            "not json at all".to_string(),
            r#"{"type":"user","timest"#.to_string(),
            // whitespace-only prompt
            user_line("   ", "2026-10-19T08:36:00.000Z"),
            user_line("second real prompt", "2026-10-19T08:37:00.000Z"),
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        let prompts = user_prompts(&path, HISTORY_CAP);
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0].msg, "fix the bug");
        assert_eq!(prompts[0].at, expected_at);
        assert_eq!(prompts[1].msg, "second real prompt");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn keeps_only_the_last_cap_prompts_in_order() {
        let path = temp_path("cap");
        let lines: Vec<String> = (0..5)
            .map(|i| user_line(&format!("p{i}"), "2026-10-19T08:30:00.000Z"))
            .collect();
        std::fs::write(&path, lines.join("\n")).unwrap();
        let prompts = user_prompts(&path, 2);
        assert_eq!(
            prompts.iter().map(|p| p.msg.as_str()).collect::<Vec<_>>(),
            ["p3", "p4"]
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_or_unreadable_file_is_empty() {
        assert!(user_prompts(Path::new("/clitab-no-such-transcript.jsonl"), 10).is_empty());
    }

    #[test]
    fn transcript_path_prefers_the_hook_then_derives() {
        let with_tp = SessionBinding {
            agent: AgentKind::Claude,
            sid: "s1".into(),
            transcript: Some("/custom/s1.jsonl".into()),
        };
        assert_eq!(
            transcript_for(&with_tp, "/proj").unwrap(),
            PathBuf::from("/custom/s1.jsonl")
        );

        let home = std::env::var("HOME").unwrap();
        let derived = SessionBinding { agent: AgentKind::Claude, sid: "s1".into(), transcript: None };
        assert_eq!(
            transcript_for(&derived, "/Users/x/proj").unwrap(),
            PathBuf::from(format!("{home}/.claude/projects/-Users-x-proj/s1.jsonl"))
        );
        let qoder = SessionBinding { agent: AgentKind::Qoder, sid: "s2".into(), transcript: None };
        assert_eq!(
            transcript_for(&qoder, "/Users/x/proj").unwrap(),
            PathBuf::from(format!("{home}/.qoder/projects/-Users-x-proj/s2.jsonl"))
        );
    }
}
```

Also add `mod history;` to the module list at the top of `src-tauri/src/lib.rs` (after `mod attention;`).

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib history::tests`
Expected: FAIL — `user_prompts`, `transcript_for`, `HISTORY_CAP`, `Path` not found (compile error).

- [ ] **Step 4: Implement**

In `src-tauri/src/history.rs`, insert between the module doc and `#[cfg(test)]`:

```rust
use crate::status::{AgentKind, SessionBinding};
use chrono::DateTime;
use serde::Serialize;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Matches the timeline's renderer-side cap (`MAX_TIMELINE_EVENTS` in
/// `src/lib/timeline.ts`): history and live rows share one budget.
pub const HISTORY_CAP: usize = 500;

/// One historical user prompt. `at` is epoch ms from the transcript's own
/// timestamp — the renderer displays it like a live row.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPrompt {
    pub at: u64,
    pub msg: String,
}

/// Wrappers the agents inject through the user-prompt pipeline: they ride
/// `type:"user"` records but are not the user's own input. Same verdict as
/// `status::is_injected_prompt` for the live path, extended to the other
/// transcript-only wrappers (spec D6 — v1 is strict).
const INJECTED_PREFIXES: [&str; 6] = [
    "<task-notification>",
    "<local-command-caveat>",
    "<local-command-stdout>",
    "<command-name>",
    "<command-message>",
    "<cross-session-message",
];

/// Where the transcript for a binding lives: the hook-supplied path when
/// present, else derived from the agent's projects-dir convention
/// (`~/.claude/projects/<cwd with / as ->/<sid>.jsonl`; Qoder mirrors it).
pub fn transcript_for(binding: &SessionBinding, cwd: &str) -> Option<PathBuf> {
    if let Some(tp) = &binding.transcript {
        return Some(PathBuf::from(tp));
    }
    let home = std::env::var("HOME").ok()?;
    let base = match binding.agent {
        AgentKind::Claude => ".claude",
        AgentKind::Qoder => ".qoder",
    };
    Some(
        PathBuf::from(home)
            .join(base)
            .join("projects")
            .join(cwd.replace('/', "-"))
            .join(format!("{}.jsonl", binding.sid)),
    )
}

/// The real user prompts of a transcript, oldest first, capped to the last
/// `cap`. Tolerant line-by-line: a truncated tail line, a non-JSON line or an
/// unknown record shape is skipped, never an error (Review Focus 2).
pub fn user_prompts(path: &Path, cap: usize) -> Vec<HistoryPrompt> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let mut out: VecDeque<HistoryPrompt> = VecDeque::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        let Some(prompt) = prompt_of(&line) else { continue };
        if out.len() == cap {
            out.pop_front();
        }
        out.push_back(prompt);
    }
    out.into()
}

fn prompt_of(line: &str) -> Option<HistoryPrompt> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("type")?.as_str()? != "user" {
        return None;
    }
    if v.get("isSidechain").and_then(|s| s.as_bool()).unwrap_or(false) {
        return None;
    }
    let message = v.get("message")?;
    if message.get("role")?.as_str()? != "user" {
        return None;
    }
    // Strict v1 rule (spec D6): only plain-string content. Arrays are tool
    // results, skill injections or attachments — never a typed prompt.
    let text = message.get("content")?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    if INJECTED_PREFIXES.iter().any(|prefix| text.starts_with(prefix)) {
        return None;
    }
    let at = DateTime::parse_from_rfc3339(v.get("timestamp")?.as_str()?)
        .ok()?
        .timestamp_millis();
    if at < 0 {
        return None;
    }
    Some(HistoryPrompt { at: at as u64, msg: text.to_string() })
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib history::tests`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/history.rs src-tauri/src/lib.rs
git commit -m "$(cat <<'EOF'
feat: parse historical user prompts from agent transcripts

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Tab snapshot persistence (`restore.rs`)

**Files:**
- Create: `src-tauri/src/restore.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod restore;` — alphabetical, after `mod pty;`)

**Interfaces:**
- Consumes: `crate::status::SessionBinding` (Task 1).
- Produces:
  - `pub struct SnapshotTab { pub cwd: String, pub title: String, pub has_program_title: bool, pub session: Option<SessionBinding>, pub agent_running: bool }` — `Clone, Debug, PartialEq, Serialize, Deserialize`, serde camelCase, `session` with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - `pub struct Snapshot { pub version: u32, pub active: Option<usize>, pub tabs: Vec<SnapshotTab> }` — same derives/attrs (`active` also `#[serde(default, skip_serializing_if = "Option::is_none")]`); constructor `pub fn current(tabs: Vec<SnapshotTab>, active: Option<usize>) -> Self` stamps `version: 1`.
  - `pub fn save(config_dir: &Path, snapshot: &Snapshot)` — best-effort, tmp+rename.
  - `pub fn load(config_dir: &Path) -> Option<Snapshot>` — `None` on missing/corrupt/wrong-version.
  - File name constant `tab-snapshot.json` (private `const FILE`).

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/restore.rs` with the module doc and test module only:

```rust
//! The tab snapshot: what the sidebar looked like while the app last ran.
//! Written on every snapshot-relevant change (never only at quit — a crash
//! must leave the same state), read once at startup. Same best-effort
//! contract as `last_cwd.rs`: I/O errors are silently ignored.

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
```

Also add `mod restore;` to the module list in `src-tauri/src/lib.rs` (after `mod pty;`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib restore::tests`
Expected: FAIL — `Snapshot`, `SnapshotTab`, `save`, `load` not found (compile error).

- [ ] **Step 3: Implement**

In `src-tauri/src/restore.rs`, insert between the module doc and `#[cfg(test)]`:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib restore::tests`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/restore.rs src-tauri/src/lib.rs
git commit -m "$(cat <<'EOF'
feat: persist a crash-safe tab snapshot

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Registry — session state, active tab, snapshot persistence

**Files:**
- Modify: `src-tauri/src/pty/registry.rs` (imports, `TabRecord`, `Registry`, `insert`/`remove`/`reorder`/`set_title`/`set_cwd`/`clear_program_title`, new mutators + `snapshot`/`persist`, new tests appended to the existing `mod tests`)
- Modify: `src-tauri/src/lib.rs` (test module ONLY: the two `TabRecord` literals in `tab_response_serializes_protocol_state` gain the four new fields — without this the crate stops compiling)

**Interfaces:**
- Consumes: `crate::status::SessionBinding` (Task 1), `crate::restore::{Snapshot, SnapshotTab}` (Task 3).
- Produces (Tasks 5–6 call these):
  - `TabRecord` new pub fields: `session: Option<SessionBinding>`, `agent_running: bool`, `resume_pending: bool`, `restored_session: bool`.
  - `Registry::with_persist_dir(dir: PathBuf) -> Self`
  - `pub fn insert_record(&self, record: TabRecord) -> TabRecord`
  - `pub fn set_active(&self, id: &str)` / `pub fn active_id(&self) -> Option<String>`
  - `pub fn set_session(&self, id: &str, binding: SessionBinding)`
  - `pub fn set_agent_running(&self, id: &str, running: bool)`
  - `pub fn begin_resume(&self, id: &str)` / `pub fn cancel_resume(&self, id: &str)`
  - `pub fn snapshot(&self) -> Snapshot`
  - Behavior changes: every snapshot-relevant mutator persists on *change*; `remove` advances `active` to the closed tab's neighbor (same rule as the renderer's `abandonTab`).

⚠️ **Deadlock rule for this task:** `persist()` re-acquires the `tabs` and `active` locks (through `snapshot()`). Every mutator must drop its guard(s) *before* calling it — scoped blocks everywhere, and an explicit `drop(tabs)` in `reorder` (whose guard lives to the end of the function today). Never hold two registry locks at once.

- [ ] **Step 1: Write the failing tests**

Append to the existing `mod tests` in `src-tauri/src/pty/registry.rs` (after the last test, before the module's closing brace; `SessionBinding`, `PathBuf` and `VecDeque` arrive via `use super::*` once step 3 lands):

```rust
    use crate::status::AgentKind;

    fn binding(sid: &str) -> SessionBinding {
        SessionBinding { agent: AgentKind::Claude, sid: sid.into(), transcript: None }
    }

    fn persist_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("clitab-registry-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A tab as the restore path builds it: snapshot said an agent session
    /// was alive, resume not yet issued.
    fn restored_record(id: &str) -> TabRecord {
        TabRecord {
            id: id.into(),
            title: "Fix build".into(),
            cwd: "/proj".into(),
            has_program_title: true,
            waiting: false,
            status: None,
            notice: None,
            answer: None,
            turn_start: None,
            prompt_queue: VecDeque::new(),
            last_stop_ms: None,
            session: Some(binding("s-1")),
            agent_running: true,
            resume_pending: true,
            restored_session: true,
        }
    }

    #[test]
    fn session_and_agent_running_track_the_hook_signals() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        assert_eq!(registry.get("t1").unwrap().session, None);
        registry.set_session("t1", binding("s-1"));
        registry.set_agent_running("t1", true);
        let tab = registry.get("t1").unwrap();
        assert_eq!(tab.session.as_ref().unwrap().sid, "s-1");
        assert!(tab.agent_running);
        // shell integration's claude-done (PromptReady): the agent exited
        registry.set_agent_running("t1", false);
        assert!(!registry.get("t1").unwrap().agent_running);
        // the binding survives the exit — history still knows which session
        // the tab belongs to
        assert_eq!(registry.get("t1").unwrap().session.unwrap().sid, "s-1");
    }

    #[test]
    fn resume_flags_transition() {
        let registry = Registry::new();
        registry.insert_record(restored_record("t1"));
        let tab = registry.get("t1").unwrap();
        assert!(tab.resume_pending);
        assert!(tab.restored_session);
        assert!(tab.agent_running, "snapshot said the agent was alive");

        registry.begin_resume("t1");
        let tab = registry.get("t1").unwrap();
        assert!(!tab.resume_pending, "resume issued");
        assert!(tab.agent_running, "--resume fires no hook; the flag must be set here");
        assert!(tab.restored_session, "history still belongs to the restored session");
    }

    /// Review Focus 5 (deleted transcript): cancel keeps the tab a usable
    /// plain shell — title reverts to the cwd, but the session binding stays
    /// so the transcript's history can still be served.
    #[test]
    fn cancel_resume_reverts_to_a_plain_shell_tab() {
        let registry = Registry::new();
        registry.insert_record(restored_record("t1"));
        registry.cancel_resume("t1");
        let tab = registry.get("t1").unwrap();
        assert!(!tab.resume_pending);
        assert!(!tab.restored_session);
        assert!(!tab.has_program_title);
        assert_eq!(tab.title, "/proj");
        assert_eq!(tab.session.unwrap().sid, "s-1", "binding is kept for history");
    }

    #[test]
    fn active_defaults_to_the_closed_tabs_neighbor() {
        let registry = abc();
        registry.set_active("b");
        assert_eq!(registry.active_id().unwrap(), "b");
        registry.remove("b");
        assert_eq!(registry.active_id().unwrap(), "c", "next tab slides into place");
        registry.remove("c");
        assert_eq!(registry.active_id().unwrap(), "a", "clamped to the new last");
        registry.remove("a");
        assert_eq!(registry.active_id(), None, "no tabs, no active tab");
    }

    #[test]
    fn removing_a_background_tab_keeps_the_active_one() {
        let registry = abc();
        registry.set_active("a");
        registry.remove("b");
        assert_eq!(registry.active_id().unwrap(), "a");
    }

    #[test]
    fn snapshot_captures_tabs_order_session_and_active_index() {
        let dir = persist_dir();
        let registry = Registry::with_persist_dir(dir.clone());
        registry.insert("a".into(), "/a".into());
        registry.insert("b".into(), "/b".into());
        registry.set_session("b", binding("s-1"));
        registry.set_agent_running("b", true);
        registry.set_title("b", "Fix build", true);
        registry.reorder("a", 1);
        registry.set_active("a");

        let snapshot = registry.snapshot();
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.tabs[0].cwd, "/b");
        assert_eq!(snapshot.tabs[0].title, "Fix build");
        assert!(snapshot.tabs[0].has_program_title);
        assert_eq!(snapshot.tabs[0].session.as_ref().unwrap().sid, "s-1");
        assert!(snapshot.tabs[0].agent_running);
        assert_eq!(snapshot.tabs[1].cwd, "/a");
        assert_eq!(snapshot.active, Some(1), "a is second after the reorder");
        // every change above persisted: the file matches the in-memory state
        assert_eq!(crate::restore::load(&dir).unwrap(), snapshot);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Turn-protocol state is deliberately NOT part of the snapshot: a
    /// restored tab starts protocol-fresh (see `SnapshotTab`'s doc).
    #[test]
    fn turn_protocol_mutators_do_not_persist() {
        let dir = persist_dir();
        let registry = Registry::with_persist_dir(dir.clone());
        registry.insert("a".into(), "/a".into());
        registry.begin_turn("a", 1000, Some("hi".into()), 99_999);
        registry.set_notice("a", Some("permission?".into()), 1100);
        registry.set_waiting("a");
        let on_disk = crate::restore::load(&dir).unwrap();
        assert_eq!(on_disk.tabs.len(), 1);
        assert!(!on_disk.tabs[0].agent_running);
        // the in-memory turn state exists but never reached the file
        assert!(registry.get("a").unwrap().status.is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib registry::tests`
Expected: FAIL — compile errors: `restored_record` builds `TabRecord` with unknown fields `session`/`agent_running`/`resume_pending`/`restored_session`; `insert_record`, `set_session`, `set_agent_running`, `begin_resume`, `cancel_resume`, `set_active`, `active_id`, `snapshot`, `with_persist_dir` not found.

- [ ] **Step 3: Implement**

**3a. Imports** — replace the import block at the top of `registry.rs`:

```rust
use super::lock;
use crate::restore::{Snapshot, SnapshotTab};
use crate::status::{is_injected_prompt, Answer, Notice, SessionBinding, TabStatus};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
```

**3b. `TabRecord`** — add four fields after `last_stop_ms` (keep every existing field and doc comment verbatim):

```rust
    /// Epoch ms of the last accepted `end_turn`; anchors the duplicate-stop
    /// burst guard.
    pub last_stop_ms: Option<u64>,
    /// The agent session this tab last talked to. Every prompt hook carrying
    /// a binding refreshes it; only the *latest* session per tab is recorded
    /// (spec D7).
    pub session: Option<SessionBinding>,
    /// True while the agent CLI is believed to be running: any hook event
    /// sets it, shell-integration `claude-done` (PromptReady) clears it
    /// (spec D3). Snapshotted — it decides whether restore offers a resume.
    pub agent_running: bool,
    /// Restored tab whose resume has not been issued yet. Set at restore for
    /// resumable tabs; cleared by `begin_resume` / `cancel_resume`.
    pub resume_pending: bool,
    /// This tab exists because of a restore AND its resume path is live:
    /// drives the renderer's lazy history load. Stays true through
    /// `begin_resume` (the startup auto-resume clears only `resume_pending`,
    /// and the renderer still owes the history load); cleared by
    /// `cancel_resume`.
    pub restored_session: bool,
```

**3c. `Registry` struct** — replace:

```rust
#[derive(Debug, Default)]
pub struct Registry {
    tabs: Mutex<Vec<TabRecord>>,
}
```

with:

```rust
#[derive(Debug, Default)]
pub struct Registry {
    tabs: Mutex<Vec<TabRecord>>,
    /// The tab the user is looking at; `None` until the first activation.
    /// Snapshotted so the next launch focuses the same tab.
    active: Mutex<Option<String>>,
    /// Directory holding `tab-snapshot.json` (the app config dir). `None` in
    /// unit tests and when the config dir is unavailable — persistence then
    /// silently no-ops, exactly like `last_cwd.rs`.
    persist_dir: Option<PathBuf>,
}
```

**3d. Constructor** — after `new()`, add:

```rust
    /// A registry that persists the tab snapshot to `dir` on every
    /// snapshot-relevant change (spec Q6: continuously, not only at quit —
    /// a crash must leave the same state a graceful quit would).
    pub fn with_persist_dir(dir: PathBuf) -> Self {
        Self { persist_dir: Some(dir), ..Self::default() }
    }
```

**3e. `insert`** — replace the whole method (four new default fields + persist):

```rust
    /// Register a fresh tab whose title starts out as its working directory.
    pub fn insert(&self, id: String, cwd: String) -> TabRecord {
        let record = TabRecord {
            id: id.clone(),
            title: cwd.clone(),
            cwd,
            has_program_title: false,
            waiting: false,
            status: None,
            notice: None,
            answer: None,
            turn_start: None,
            prompt_queue: VecDeque::new(),
            last_stop_ms: None,
            session: None,
            agent_running: false,
            resume_pending: false,
            restored_session: false,
        };
        lock(&self.tabs).push(record.clone());
        self.persist();
        record
    }

    /// Register a pre-built record (restore path: title and flags come from
    /// the snapshot, not from the cwd defaults).
    pub fn insert_record(&self, record: TabRecord) -> TabRecord {
        lock(&self.tabs).push(record.clone());
        self.persist();
        record
    }
```

**3f. `remove`** — replace the whole method:

```rust
    pub fn remove(&self, id: &str) -> bool {
        // Successor rule matches the renderer's `abandonTab`: the neighbor at
        // the closed index takes over (next tab slides into place; at the end
        // of the list, the new last one). Computed in its own scope — the
        // tabs lock is released before `active` is touched (lock rule).
        let successor = {
            let mut tabs = lock(&self.tabs);
            let Some(index) = tabs.iter().position(|t| t.id == id) else {
                return false;
            };
            tabs.remove(index);
            tabs.get(index.min(tabs.len().saturating_sub(1))).map(|t| t.id.clone())
        };
        {
            let mut active = lock(&self.active);
            if active.as_deref() == Some(id) {
                *active = successor;
            }
        }
        self.persist();
        true
    }
```

**3g. `reorder`** — replace the whole method (note the explicit `drop` — `persist()` re-locks `tabs`):

```rust
    /// Move a tab to `to_index` (remove-then-insert, index clamped to the
    /// list); false when the tab is unknown. The renderer performs the same
    /// array move locally while dragging, so both sides converge on one order
    /// and a webview reload restores it via `list`.
    pub fn reorder(&self, id: &str, to_index: usize) -> bool {
        let mut tabs = lock(&self.tabs);
        let Some(from) = tabs.iter().position(|t| t.id == id) else {
            return false;
        };
        let record = tabs.remove(from);
        let target = to_index.min(tabs.len());
        tabs.insert(target, record);
        drop(tabs); // persist() re-locks tabs — release first (lock rule)
        self.persist();
        true
    }
```

**3h. `set_title` / `set_cwd` / `clear_program_title`** — replace all three (change gates: OSC 0/2 fire on every ink repaint and OSC 7 on every prompt; only real changes may hit the disk):

```rust
    /// A program (shell prompt, TUI, `echo -e`) set the terminal title.
    /// Change-gated: a repaint storm must not become a write storm.
    pub fn set_title(&self, id: &str, title: &str, is_program_title: bool) {
        let changed = {
            let mut tabs = lock(&self.tabs);
            match tabs.iter_mut().find(|t| t.id == id) {
                Some(tab)
                    if tab.title != title || tab.has_program_title != is_program_title =>
                {
                    tab.title = title.to_string();
                    tab.has_program_title = is_program_title;
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.persist();
        }
    }

    /// OSC 7 reported a new working directory. Returns true only when the
    /// value actually changed: the hook fires on every prompt, and only a
    /// real change is worth persisting (see the CwdChanged handler).
    pub fn set_cwd(&self, id: &str, cwd: &str) -> bool {
        let changed = {
            let mut tabs = lock(&self.tabs);
            match tabs.iter_mut().find(|t| t.id == id) {
                Some(tab) if tab.cwd != cwd => {
                    tab.cwd = cwd.to_string();
                    if !tab.has_program_title {
                        tab.title = cwd.to_string();
                    }
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.persist();
        }
        changed
    }

    /// Forget a program title and fall back to the working directory.
    /// Change-gated: fires on every turn end.
    pub fn clear_program_title(&self, id: &str) {
        let changed = {
            let mut tabs = lock(&self.tabs);
            match tabs.iter_mut().find(|t| t.id == id) {
                Some(tab) if tab.has_program_title => {
                    tab.has_program_title = false;
                    tab.title = tab.cwd.clone();
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.persist();
        }
    }
```

**3i. Session/restore mutators** — insert directly after `clear_program_title`:

```rust
    /// The user is looking at this tab (the renderer reports every switch).
    /// Change-gated: tab clicks are frequent, disk writes must not be.
    pub fn set_active(&self, id: &str) {
        let changed = {
            let mut active = lock(&self.active);
            if active.as_deref() == Some(id) {
                false
            } else {
                *active = Some(id.to_string());
                true
            }
        };
        if changed {
            self.persist();
        }
    }

    pub fn active_id(&self) -> Option<String> {
        lock(&self.active).clone()
    }

    /// A prompt hook carried a session binding: record it. Change-gated —
    /// the same binding re-arrives on every prompt of a session, and only
    /// the first (or a genuinely new session) is worth a write.
    pub fn set_session(&self, id: &str, binding: SessionBinding) {
        let changed = {
            let mut tabs = lock(&self.tabs);
            match tabs.iter_mut().find(|t| t.id == id) {
                Some(tab) if tab.session.as_ref() != Some(&binding) => {
                    tab.session = Some(binding);
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.persist();
        }
    }

    /// Agent liveness (spec D3): any hook event ⇒ `true`; shell-integration
    /// `claude-done` (PromptReady) ⇒ `false`. Change-gated: hook events are
    /// dense, transitions are rare.
    pub fn set_agent_running(&self, id: &str, running: bool) {
        let changed = {
            let mut tabs = lock(&self.tabs);
            match tabs.iter_mut().find(|t| t.id == id) {
                Some(tab) if tab.agent_running != running => {
                    tab.agent_running = running;
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.persist();
        }
    }

    /// The `<agent> --resume <sid>` line was injected into the tab's shell.
    /// `claude --resume` fires NO hook at startup, so `agent_running` must be
    /// set HERE: quitting right after a restore must still snapshot the tab
    /// as alive, or the next launch would not offer the resume again and the
    /// restore chain would silently break (spec D3). `restored_session`
    /// stays true — the renderer still owes this tab its history load.
    pub fn begin_resume(&self, id: &str) {
        {
            let mut tabs = lock(&self.tabs);
            if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
                tab.resume_pending = false;
                tab.agent_running = true;
            }
        }
        self.persist();
    }

    /// Resume is impossible (transcript deleted, binding lost): the tab stays
    /// a plain shell (spec D4). A program title kept from the snapshot is
    /// reverted — the agent it belonged to is not running. The binding is
    /// deliberately KEPT: `session_history` can still serve the transcript
    /// if it reappears, and a later prompt hook revives the tab normally.
    pub fn cancel_resume(&self, id: &str) {
        {
            let mut tabs = lock(&self.tabs);
            if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
                tab.resume_pending = false;
                tab.restored_session = false;
                if tab.has_program_title {
                    tab.has_program_title = false;
                    tab.title = tab.cwd.clone();
                }
            }
        }
        self.persist();
    }
```

**3j. Snapshot + persist** — insert after `list()` (still inside `impl Registry`):

```rust
    /// Everything the next launch needs to recreate this moment, in sidebar
    /// order. Clone-and-release per the lock rule: `tabs` and `active` are
    /// locked one after the other, never at the same time — `persist()` is
    /// called from mutators that have just released their own guards.
    pub fn snapshot(&self) -> Snapshot {
        let tabs = lock(&self.tabs).clone();
        let active = lock(&self.active).clone();
        let active_index = active
            .as_deref()
            .and_then(|id| tabs.iter().position(|t| t.id == id));
        Snapshot::current(
            tabs.into_iter()
                .map(|t| SnapshotTab {
                    cwd: t.cwd,
                    title: t.title,
                    has_program_title: t.has_program_title,
                    session: t.session,
                    agent_running: t.agent_running,
                })
                .collect(),
            active_index,
        )
    }

    // Persistence map (spec Q6). Snapshot-relevant mutators write the file:
    //   insert, insert_record, remove, reorder, set_title, set_cwd,
    //   clear_program_title, set_active, set_session, set_agent_running,
    //   begin_resume, cancel_resume
    // Turn-protocol mutators deliberately do NOT (turn state is not part of
    // the snapshot; a restored tab starts protocol-fresh):
    //   set_waiting, clear_waiting, begin_turn, begin_auto_turn, set_tool,
    //   end_turn, reconcile_idle_turn, set_notice, set_answer, clear_notice

    /// Write `tab-snapshot.json`. MUST NOT be called while any registry lock
    /// is held (it re-locks via `snapshot()`). Best-effort like everything
    /// else in this file's persistence story.
    fn persist(&self) {
        let Some(dir) = &self.persist_dir else {
            return;
        };
        crate::restore::save(dir, &self.snapshot());
    }
```

**3k. `lib.rs` test literals** — in `src-tauri/src/lib.rs`'s test module, each `TabRecord` literal (there are two, in `tab_response_serializes_protocol_state`) gains four fields after `last_stop_ms: None,`:

```rust
            session: None,
            agent_running: false,
            resume_pending: false,
            restored_session: false,
```

Verify no other literal exists: `grep -rn "TabRecord {" src-tauri/src` — expected hits are `registry.rs` (`insert`, already updated in 3e) and the two in `lib.rs` tests.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib registry::tests`
Expected: PASS (all pre-existing registry tests + the 7 new ones).

- [ ] **Step 5: Run the full suite**

Run: `npm test`
Expected: PASS — 130 tests (baseline 109 + 6 status + 4 history + 4 restore + 7 registry).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/pty/registry.rs src-tauri/src/lib.rs
git commit -m "$(cat <<'EOF'
feat: track session state, active tab and snapshot persistence in the registry

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: PTY loop wiring (`session.rs`)

**Files:**
- Modify: `src-tauri/src/pty/session.rs` — ONLY the `OscEvent::PromptReady` and `OscEvent::Clitab` arms of `handle_osc` (around lines 352–370)

**Interfaces:**
- Consumes: `registry.set_agent_running` (Task 4), `status::decode_session` (Task 1).
- Produces: nothing new — this task makes the tracked state *live*. Later tasks read what it writes.

No new unit tests: `handle_osc` needs an `AppHandle` and a PTY, which this codebase does not mock (the pre-existing arms are untested for the same reason). The behavior is pinned from both ends — the registry transitions by Task 4's tests, the decode by Task 1's — and end-to-end by Task 10. Do NOT touch the emit-order invariants around `handle_osc` (Global Constraints).

- [ ] **Step 1: Replace the `PromptReady` arm**

```rust
            OscEvent::PromptReady => {
                // The assistant's turn ended and the prompt came back.
                program_active.store(false, Ordering::Relaxed);
                registry.clear_program_title(tab_id);
                // The shell-integration turn-end flag fires when the agent CLI
                // hands the terminal back to the shell prompt: the agent has
                // exited (spec D3). The session binding deliberately survives —
                // history still refers to it, and the next prompt hook revives
                // the flag.
                registry.set_agent_running(tab_id, false);
                *lock(last_activity) = Instant::now();
                // After clear_program_title: the notification then carries the
                // same title the tab bar shows (the cwd it reverted to).
                crate::attention::enter_waiting(app, registry, tab_id);
                let _ = app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
                let _ = app.emit("prompt-ready", serde_json::json!({ "tab_id": tab_id }));
            }
```

(The only change vs. today is the `set_agent_running(tab_id, false)` call plus its comment; everything else stays byte-identical.)

- [ ] **Step 2: Replace the `Clitab` arm**

```rust
            OscEvent::Clitab(json) => {
                // Unknown kinds and malformed payloads decode to None and are
                // dropped: a hook emitting something newer than this build
                // must be a no-op, never an error.
                // The session binding rides the prompt payload (the only hook
                // whose stdin carries session_id/transcript_path). Old hook
                // installs omit sid/tp/a and decode_session returns None —
                // the previous binding is kept, restore degrades silently
                // (Review Focus 4).
                if let Some(binding) = status::decode_session(&json) {
                    registry.set_session(tab_id, binding);
                }
                if let Some(event) = status::decode(&json) {
                    // Any understood hook event proves the agent CLI is alive
                    // right now (spec D3).
                    registry.set_agent_running(tab_id, true);
                    Self::handle_status(tab_id, app, registry, flashed, idle_gap_ms, event);
                }
            }
```

- [ ] **Step 3: Run the full suite**

Run: `npm test`
Expected: PASS (130 tests — unchanged count; this task adds behavior, not tests).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/pty/session.rs
git commit -m "$(cat <<'EOF'
feat: wire session bindings and agent liveness into the PTY loop

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Restore + resume machinery (`manager.rs`, `lib.rs`, `services.rs`)

**Files:**
- Modify: `src-tauri/src/pty/manager.rs` (`new`, `create_tab`, four new methods)
- Modify: `src-tauri/src/lib.rs` (module list, `TabResponse::from_record` replacing `From`, `create_tab`/`list_tabs`, three new commands, `invoke_handler`, startup thread, tests)
- Modify: `src-tauri/src/services.rs` (one line in `open_tab`)

**Interfaces:**
- Consumes: `Registry::{with_persist_dir, set_active, active_id, get, insert_record, cancel_resume, begin_resume}` (Task 4), `restore::{load, SnapshotTab}` (Task 3), `history::{transcript_for, user_prompts, HISTORY_CAP, HistoryPrompt}` (Task 2), `AgentKind::cmd()` via `SessionBinding` (Task 1), `PtySession::write` (existing).
- Produces:
  - `TabManager::set_active(&self, tab_id: &str)` / `active_id(&self) -> Option<String>` / `record(&self, tab_id: &str) -> Option<TabRecord>` / `restore_snapshot(&self) -> Option<String>` / `resume_tab(&self, tab_id: &str) -> Result<(), ManagerError>`
  - `TabResponse::from_record(tab: TabRecord, is_active: bool) -> Self`; new response fields `is_active`, `pending_resume`, `restored_session` (camelCase on the wire: `isActive`, `pendingResume`, `restoredSession` — Task 7 mirrors them in `types.ts`).
  - Commands: `set_active_tab(tab_id)`, `resume_tab(tab_id)`, `session_history(tab_id) -> Vec<HistoryPrompt>`.

The restore/resume orchestration itself has no unit tests (it needs `AppHandle` + real PTYs, which this codebase does not mock — same situation as `create_tab` today); Task 10 is its verification. The IPC contract it exposes IS unit-tested (`lib.rs` tests below).

- [ ] **Step 1: Update the `lib.rs` tests first (failing state)**

In `src-tauri/src/lib.rs`'s `mod tests`:

1. In `tab_response_serializes_protocol_state`, replace both `TabResponse::from(record)` / `TabResponse::from(idle)` with `TabResponse::from_record(record, true)` / `TabResponse::from_record(idle, true)`, and after `assert!(json.get("turnStart").is_none());` add:

```rust
        assert_eq!(json["isActive"], serde_json::json!(true));
        assert_eq!(json["pendingResume"], serde_json::json!(false));
        assert_eq!(json["restoredSession"], serde_json::json!(false));
```

2. Append a new test:

```rust
    /// The restore flags must reach the renderer inside `tab-created` /
    /// `list_tabs` payloads — lazy resume and history loading key off them.
    /// `session` and `agent_running` stay backend-internal.
    #[test]
    fn restore_flags_travel_in_the_response() {
        let record = TabRecord {
            id: "t3".into(),
            title: "Fix build".into(),
            cwd: "/proj".into(),
            has_program_title: true,
            waiting: false,
            status: None,
            notice: None,
            answer: None,
            turn_start: None,
            prompt_queue: Default::default(),
            last_stop_ms: None,
            session: None,
            agent_running: false,
            resume_pending: true,
            restored_session: true,
        };
        let json = serde_json::to_value(TabResponse::from_record(record, false)).unwrap();
        assert_eq!(json["isActive"], serde_json::json!(false));
        assert_eq!(json["pendingResume"], serde_json::json!(true));
        assert_eq!(json["restoredSession"], serde_json::json!(true));
        assert!(json.get("session").is_none());
        assert!(json.get("agentRunning").is_none());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `npm test`
Expected: FAIL (compile error) — `no function or associated item named from_record found for struct TabResponse`.

- [ ] **Step 3: Implement `lib.rs`**

**3a. Module list** — replace:

```rust
mod attention;
mod history;
mod last_cwd;
mod menu;
mod osc;
mod pty;
mod restore;
mod services;
mod status;
```

and add to the `use` block below it:

```rust
use history::HistoryPrompt;
```

**3b. `TabResponse`** — replace the struct AND the `impl From<TabRecord> for TabResponse` block with:

```rust
/// Tab metadata handed to the renderer. `camelCase` so it lines up with the
/// `Tab` type in `src/types.ts` without a mapping step.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabResponse {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub has_claude_title: bool,
    /// Hook-protocol turn state; null until the tab's session speaks it.
    pub status: Option<TabStatus>,
    /// Notification awaiting the user; null once acknowledged.
    pub notice: Option<Notice>,
    pub waiting: bool,
    /// The tab the user was looking at when this snapshot was taken.
    pub is_active: bool,
    /// Restored tab whose `--resume` has not been issued yet: the renderer
    /// triggers it on first activation.
    pub pending_resume: bool,
    /// Restored tab with a live resume path: the renderer loads the session
    /// transcript's history the first time it focuses the tab.
    pub restored_session: bool,
}

impl TabResponse {
    /// `is_active` is a property of the response, not of the record (the
    /// registry tracks the active id separately), so callers pass it in.
    pub fn from_record(tab: TabRecord, is_active: bool) -> Self {
        Self {
            id: tab.id,
            title: tab.title,
            cwd: tab.cwd,
            has_claude_title: tab.has_program_title,
            status: tab.status,
            notice: tab.notice,
            // `turn_start`, `prompt_queue`, `last_stop_ms`, `answer`,
            // `session` and `agent_running` are deliberately not exposed:
            // backend-internal, and timeline-only (arrives via `tab-status`).
            waiting: tab.waiting,
            is_active,
            pending_resume: tab.resume_pending,
            restored_session: tab.restored_session,
        }
    }
}
```

**3c. `create_tab` command** — a user-created tab is always the focused one:

```rust
#[tauri::command]
fn create_tab(state: State<'_, AppState>, cwd: Option<String>) -> Result<TabResponse, String> {
    state
        .tab_manager
        .create_tab(cwd)
        .map(|record| TabResponse::from_record(record, true))
        .map_err(|e| e.to_string())
}
```

**3d. `list_tabs` command**:

```rust
#[tauri::command]
fn list_tabs(state: State<'_, AppState>) -> Result<Vec<TabResponse>, String> {
    let active = state.tab_manager.active_id();
    Ok(state
        .tab_manager
        .list_tabs()
        .into_iter()
        .map(|tab| {
            let is_active = active.as_deref() == Some(tab.id.as_str());
            TabResponse::from_record(tab, is_active)
        })
        .collect())
}
```

**3e. New commands** — insert after `ack_tab_notice`:

```rust
/// The renderer switched tabs. Never errors on an unknown tab: a stale
/// report is harmless (same contract as `ack_tab_notice`).
#[tauri::command]
fn set_active_tab(state: State<'_, AppState>, tab_id: String) -> Result<(), String> {
    state.tab_manager.set_active(&tab_id);
    Ok(())
}

/// First activation of a restored tab: issue its pending `--resume`. Degrades
/// silently to a plain shell when the transcript is gone (see
/// `TabManager::resume_tab`); only a vanished tab is an error, and it carries
/// the `TAB_GONE` marker so the renderer can ignore it.
#[tauri::command]
fn resume_tab(state: State<'_, AppState>, tab_id: String) -> Result<(), String> {
    state.tab_manager.resume_tab(&tab_id).map_err(ipc_error)
}

/// The restored session's historical user prompts, oldest first, capped at
/// `HISTORY_CAP`. Empty (not an error) when the tab has no binding, the
/// transcript is gone, or hooks were never installed.
#[tauri::command]
fn session_history(
    state: State<'_, AppState>,
    tab_id: String,
) -> Result<Vec<HistoryPrompt>, String> {
    let record = state
        .tab_manager
        .record(&tab_id)
        .ok_or_else(|| format!("{TAB_GONE}{tab_id}"))?;
    let Some(binding) = record.session else {
        return Ok(Vec::new());
    };
    let Some(path) = history::transcript_for(&binding, &record.cwd) else {
        return Ok(Vec::new());
    };
    Ok(history::user_prompts(&path, history::HISTORY_CAP))
}
```

**3f. `invoke_handler`** — replace the list:

```rust
        .invoke_handler(tauri::generate_handler![
            create_tab,
            close_tab,
            list_tabs,
            reorder_tab,
            pty_input,
            resize_pty,
            attach_stream,
            detach_tab,
            has_active_process,
            ack_tab_notice,
            set_active_tab,
            resume_tab,
            session_history
        ])
```

**3g. Startup thread** — replace the whole `let handle = app.handle().clone(); std::thread::spawn(move || { ... });` block inside `setup` (the explanatory comment above it — "The first tab is created shortly *after* startup…" — stays as-is):

```rust
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(
                    services::STARTUP_GRACE_MS,
                ));
                let pending = pty::lock(&service_state).settle();
                // Borrow through the state guard: `handle.state::<AppState>()`
                // hands out a reference that must outlive `manager`.
                let state = handle.state::<AppState>();
                let manager = &state.tab_manager;
                // A tab already exists only if the user beat the grace
                // (e.g. ⌘T while the webview was loading); don't stack.
                if !manager.list_tabs().is_empty() {
                    return;
                }
                // Restore wins over both other startup paths (spec Q3): the
                // previous run's sidebar comes back as-is, active tab's agent
                // session already resuming. A Finder request during the grace
                // becomes an EXTRA focused tab on top of the restore (spec
                // D5) — it never replaces a restored tab.
                if manager.restore_snapshot().is_some() {
                    if let Some(path) = pending {
                        services::open_tab(
                            &handle,
                            Some(path.to_string_lossy().into_owned()),
                            true,
                        );
                    }
                    return;
                }
                // No snapshot (first launch, or corrupt/old file): today's
                // behavior. The Finder service request wins; a normal launch
                // restores the last-used working directory (no file, or a
                // directory that has since vanished → `resolve_cwd` applies
                // the default fallback).
                let cwd = pending
                    .map(|p| p.to_string_lossy().into_owned())
                    .or_else(|| last_cwd::dir(&handle).and_then(|dir| last_cwd::load(&dir)));
                services::open_tab(&handle, cwd, false);
            });
```

- [ ] **Step 4: Implement `manager.rs`**

**4a. `new()`** — replace:

```rust
    pub fn new(app: AppHandle) -> Self {
        // The registry persists the tab snapshot next to `last-cwd` (same
        // config dir). Without one, snapshots silently skip and restore
        // degrades to today's last-cwd startup.
        let registry = match crate::last_cwd::dir(&app) {
            Some(dir) => Registry::with_persist_dir(dir),
            None => Registry::new(),
        };
        Self {
            sessions: Mutex::new(HashMap::new()),
            registry: Arc::new(registry),
            app,
        }
    }
```

**4b. `create_tab`** — after `lock(&self.sessions).insert(tab_id.clone(), session);` insert:

```rust
        // A new tab is by definition the one the user is about to look at.
        self.registry.set_active(&tab_id);
```

**4c. New methods** — insert after `reorder_tab` (before the closing `}` of `impl TabManager`):

```rust
    /// The renderer switched tabs: remember which one the user is looking at
    /// (the snapshot's active index). Unknown tabs are ignored — a stale
    /// report is harmless, same contract as `ack_notice`.
    pub fn set_active(&self, tab_id: &str) {
        if self.registry.get(tab_id).is_some() {
            self.registry.set_active(tab_id);
        }
    }

    pub fn active_id(&self) -> Option<String> {
        self.registry.active_id()
    }

    pub fn record(&self, tab_id: &str) -> Option<TabRecord> {
        self.registry.get(tab_id)
    }

    /// Recreate the previous run's sidebar (spec Q3/Q6): same tabs, same
    /// order, same titles. The previously-active tab's agent session resumes
    /// right away (spec Q4 hybrid: active auto, the rest lazy on first
    /// activation). Returns the id of the tab to focus, or `None` when there
    /// is nothing to restore (no snapshot / empty / every spawn failed) —
    /// the caller then falls back to the last-cwd startup.
    pub fn restore_snapshot(&self) -> Option<String> {
        let dir = crate::last_cwd::dir(&self.app)?;
        let snapshot = crate::restore::load(&dir)?;
        if snapshot.tabs.is_empty() {
            return None;
        }

        let mut created: Vec<TabRecord> = Vec::with_capacity(snapshot.tabs.len());
        for tab in snapshot.tabs {
            match self.restore_tab(tab) {
                Ok(record) => created.push(record),
                // One unspawnable tab must not sink the whole restore.
                Err(e) => eprintln!("clitab: could not restore a tab: {e}"),
            }
        }
        if created.is_empty() {
            return None;
        }

        // If some spawns failed, indices shifted — clamping is the best
        // available guess at "the tab they were looking at".
        let active_index = snapshot.active.unwrap_or(0).min(created.len() - 1);
        let active_id = created[active_index].id.clone();
        self.registry.set_active(&active_id);
        // Resume BEFORE announcing: the `tab-created` payload must carry the
        // post-resume flags (`pendingResume` already false), so the renderer
        // does not try to resume the active tab a second time.
        if let Err(e) = self.resume_tab(&active_id) {
            eprintln!("clitab: could not resume the active tab's session: {e}");
        }

        {
            use tauri::Emitter;
            for (index, record) in created.iter().enumerate() {
                // Re-read each record: the resume and any early shell
                // integration output are already reflected in the payload.
                let current = self.registry.get(&record.id).unwrap_or_else(|| record.clone());
                let _ = self.app.emit(
                    "tab-created",
                    crate::TabResponse::from_record(current, index == active_index),
                );
            }
        }
        Some(active_id)
    }

    /// One restored tab: a fresh UUID and PTY, metadata from the snapshot.
    /// A resumable tab keeps its program title until the resume proves
    /// impossible (`cancel_resume` reverts it); a tab whose agent had already
    /// exited comes back as the plain shell it is, with its cwd title.
    fn restore_tab(
        &self,
        snapshot: crate::restore::SnapshotTab,
    ) -> Result<TabRecord, ManagerError> {
        let tab_id = uuid::Uuid::new_v4().to_string();
        let cwd = resolve_cwd(Some(snapshot.cwd)).to_string_lossy().to_string();
        let resumable = snapshot.agent_running && snapshot.session.is_some();
        let keep_program_title = snapshot.has_program_title && resumable;
        let title = if keep_program_title { snapshot.title } else { cwd.clone() };

        let record = TabRecord {
            id: tab_id.clone(),
            title,
            cwd: cwd.clone(),
            has_program_title: keep_program_title,
            waiting: false,
            status: None,
            notice: None,
            answer: None,
            turn_start: None,
            prompt_queue: Default::default(),
            last_stop_ms: None,
            session: snapshot.session,
            // Restore does not claim the agent is running — it is not, yet.
            // The resume injection (`begin_resume`) sets the flag.
            agent_running: false,
            resume_pending: resumable,
            restored_session: resumable,
        };
        let record = self.registry.insert_record(record);

        let session = match PtySession::new(
            tab_id.clone(),
            cwd,
            self.app.clone(),
            Arc::clone(&self.registry),
        ) {
            Ok(session) => Arc::new(session),
            Err(e) => {
                self.registry.remove(&tab_id);
                return Err(e.into());
            }
        };
        lock(&self.sessions).insert(tab_id.clone(), session);
        // Deliberately NO last_cwd save here: restored tabs re-open the
        // previous run's directories, they are not a fresh user choice —
        // the last-cwd file already reflects the last real one.
        Ok(record)
    }

    /// Issue the tab's pending resume: inject `<agent> --resume <sid>` into
    /// its shell as ordinary input (spec D1 — keeps OSC 7 cwd reporting,
    /// title revert and the preexec machinery working; no `--fork-session`,
    /// so the transcript keeps growing under the same id). The sid's charset
    /// was validated at decode time (`decode_session`), so interpolating it
    /// into the command line is safe. Degrades silently to a plain shell
    /// (spec D4) when no resume is pending, the binding is gone, or the
    /// transcript file no longer exists; only a vanished TAB is NotFound.
    pub fn resume_tab(&self, tab_id: &str) -> Result<(), ManagerError> {
        let record = self
            .registry
            .get(tab_id)
            .ok_or_else(|| ManagerError::NotFound(tab_id.to_string()))?;
        if !record.resume_pending {
            return Ok(());
        }
        let Some(binding) = record.session.clone() else {
            self.registry.cancel_resume(tab_id);
            return Ok(());
        };
        let has_transcript = crate::history::transcript_for(&binding, &record.cwd)
            .map(|path| path.is_file())
            .unwrap_or(false);
        if !has_transcript {
            // Deleted between quit and relaunch: plain shell, title reverts,
            // no error the user could act on (Review Focus 5).
            self.registry.cancel_resume(tab_id);
            return Ok(());
        }
        let line = format!("{} --resume {}\n", binding.agent.cmd(), binding.sid);
        self.session(tab_id)?.write(line.as_bytes())?;
        // `--resume` fires no hook: the registry must learn "agent alive"
        // from here, or an immediate quit would break the restore chain.
        self.registry.begin_resume(tab_id);
        Ok(())
    }
```

- [ ] **Step 5: Implement `services.rs`**

In `open_tab`, replace:

```rust
            let _ = app.emit("tab-created", crate::TabResponse::from(record));
```

with:

```rust
            // Backend-created tabs (startup fallback, Finder service) are
            // always the focused one.
            let _ = app.emit("tab-created", crate::TabResponse::from_record(record, true));
```

- [ ] **Step 6: Run the full suite**

Run: `npm test`
Expected: PASS — 131 tests (130 + `restore_flags_travel_in_the_response`).

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/pty/manager.rs src-tauri/src/lib.rs src-tauri/src/services.rs
git commit -m "$(cat <<'EOF'
feat: restore tabs and resume agent sessions at startup

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Renderer display layer — historical timeline rows

**Files:**
- Modify: `src/types.ts` (`Tab`, `TabResponse`, new `HistoryPrompt`)
- Modify: `src/lib/timeline.ts` (`TimelineEvent.historical`, new `historyEvents`)
- Modify: `src/components/TimelinePanel.tsx` (rendering of historical rows)
- Modify: `src/App.css` (hover guard + `.historical` style)

**Interfaces:**
- Consumes: `TabResponse`'s new camelCase fields `isActive` / `pendingResume` / `restoredSession` and the `session_history` response shape `{at, msg}` (Task 6).
- Produces (Task 8 uses these):
  - `Tab` / `TabResponse` gain `isActive: boolean; pendingResume: boolean; restoredSession: boolean;` (`toTab` spreads the response, so `Tab` gets them for free).
  - `export interface HistoryPrompt { at: number; msg: string }` in `types.ts`.
  - `TimelineEvent.historical?: boolean`.
  - `export function historyEvents(prompts: HistoryPrompt[]): TimelineEvent[]` in `lib/timeline.ts` — negative ids (`-(index+1)`), kind `'turn-start'`, `historical: true`.

No frontend test runner exists (Global Constraints): `npm run typecheck` is this task's gate. Negative ids are load-bearing: markers, stale keys and navigation all key on positive tracker ids, so a negative id can never collide with a live row or resolve to a marker.

- [ ] **Step 1: `src/types.ts`**

In `interface Tab`, after `waiting: boolean;` add:

```ts
  /** Backend-owned: this tab was the looked-at one when the payload was
   *  built (restore focuses it; backend-created tabs always set it). */
  isActive: boolean;
  /** Restored tab whose `--resume` has not been issued yet: the renderer
   *  triggers it on first activation. */
  pendingResume: boolean;
  /** Restored tab with a live resume path: its session transcript's history
   *  loads on first focus. */
  restoredSession: boolean;
```

In `interface TabResponse`, after `waiting: boolean;` add the same three fields with the same doc comments.

After `interface TabAnswer`, add:

```ts
/** A historical user prompt from a resumed session's transcript. Mirrors
 *  `HistoryPrompt` in `src-tauri/src/history.rs`. */
export interface HistoryPrompt {
  /** Epoch ms from the transcript's own record timestamp. */
  at: number;
  msg: string;
}
```

- [ ] **Step 2: `src/lib/timeline.ts`**

Replace line 1:

```ts
import type { HistoryPrompt, TabAnswer, TabNotice, TabStatus } from '../types';
```

In `interface TimelineEvent`, after the `startedAt` field, add:

```ts
  /** Loaded from a resumed session's transcript after a restore: predates
   *  this app run and is display-only — there is no terminal line to jump
   *  to. Historical rows carry negative `id`s (see `historyEvents`). */
  historical?: boolean;
```

Append at the end of the file:

```ts
/**
 * Transcript prompts as display-only timeline rows. Ids are negative so they
 * can never collide with the tracker's positive ids in the same per-tab array
 * (markers, stale keys and click-navigation all key on positive ids). Arrival
 * order puts history first; `displayOrder` sorts by timestamp anyway.
 */
export function historyEvents(prompts: HistoryPrompt[]): TimelineEvent[] {
  return prompts.map((prompt, index) => ({
    id: -(index + 1),
    kind: 'turn-start',
    at: prompt.at,
    msg: prompt.msg,
    historical: true,
  }));
}
```

- [ ] **Step 3: `src/components/TimelinePanel.tsx`**

Replace the whole `ordered.map((ev) => { ... })` block (the only JSX change; `labelOf`, the props and the empty state stay untouched):

```tsx
          ordered.map((ev) => {
            const stale = isStale(ev.id);
            // A history row predates this app run: the terminal scrollback it
            // belonged to is gone, so it displays but never navigates.
            const historical = ev.historical === true;
            return (
              <button
                key={ev.id}
                className={`timeline-event kind-${ev.kind}${stale ? ' stale' : ''}${
                  historical ? ' historical' : ''
                }`}
                disabled={stale || historical}
                onClick={() => onNavigate(ev.id)}
                title={
                  historical
                    ? "From the resumed session's transcript — predates this app run"
                    : stale
                      ? 'This point has scrolled out of the terminal history'
                      : 'Jump to this point in the terminal'
                }
              >
                <span className="timeline-dot" aria-hidden="true" />
                <span className="timeline-time">{formatTime(ev.startedAt ?? ev.at)}</span>
                {historical ? (
                  <span
                    className="timeline-tag"
                    title="Loaded from the session transcript after a restore"
                  >
                    History
                  </span>
                ) : ev.queued ? (
                  <span
                    className="timeline-tag"
                    title="Submitted while the previous turn was still running; it executes when that turn finishes"
                  >
                    Queued
                  </span>
                ) : null}
                {/* The label shows the message in full; the title still
                    exposes the raw hook text. The button's own title keeps
                    the navigation hint on the dot/time. */}
                <span className="timeline-label" title={ev.msg ?? undefined}>
                  {labelOf(ev)}
                </span>
              </button>
            );
          })
```

- [ ] **Step 4: `src/App.css`**

Replace the hover rule:

```css
.timeline-event:hover:not(.stale):not(.historical) {
  background: #1e1e2e;
}
```

and insert after the `.timeline-event.stale` block:

```css
/* Restored-session history: visible for reading, but the terminal line it
   belonged to died with the previous run — nothing to jump to. */
.timeline-event.historical {
  opacity: 0.55;
  cursor: default;
}
```

- [ ] **Step 5: Verify**

Run: `npm run typecheck`
Expected: PASS, no errors. (If any other file constructs a `Tab`/`TabResponse` literal, typecheck names it — none were found at plan time: every construction spreads a response.)

Run: `npm test`
Expected: PASS (131 — Rust untouched; sanity check only).

- [ ] **Step 6: Commit**

```bash
git add src/types.ts src/lib/timeline.ts src/components/TimelinePanel.tsx src/App.css
git commit -m "$(cat <<'EOF'
feat: display-only history rows in the timeline

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: Renderer wiring — lazy resume + history loading (`useTabManager.ts`)

**Files:**
- Modify: `src/hooks/useTabManager.ts` (imports, two new refs/callbacks, `abandonTab`, `switchTab`, the mount effect's `list_tabs` block, the `tab-created` listener, the effect's dep list)

**Interfaces:**
- Consumes: `historyEvents` / `MAX_TIMELINE_EVENTS` (Task 7), `HistoryPrompt` + `Tab.pendingResume` / `Tab.restoredSession` / `Tab.isActive` (Task 7), commands `set_active_tab` / `resume_tab` / `session_history` (Task 6).
- Produces: nothing consumed by later tasks — this completes the feature's code path.

Gate: `npm run typecheck` + `npm test` (no frontend test runner). Behavior is verified end-to-end in Task 10.

- [ ] **Step 1: Imports**

Add `type HistoryPrompt,` to the `../types` import (after `type FocusTabPayload,`):

```ts
import {
  TAB_GONE,
  type AttachStreamResponse,
  type FocusTabPayload,
  type HistoryPrompt,
  type MenuShortcutPayload,
  type PromptReadyPayload,
  type PtyOutputPayload,
  type Tab,
  type TabCwdPayload,
  type TabExitPayload,
  type TabFlashPayload,
  type TabResponse,
  type TabStatusPayload,
  type TabTitlePayload,
  type TabWaitingPayload,
} from '../types';
```

Replace the `../lib/timeline` import:

```ts
import {
  historyEvents,
  MAX_TIMELINE_EVENTS,
  stampQueuedStarts,
  startQueuedTurn,
  TimelineTracker,
  type TimelineEvent,
} from '../lib/timeline';
```

- [ ] **Step 2: The once-per-run guard ref**

After `const markers = useRef(new Map<string, Map<number, IMarker>>());` add:

```ts
  // Restored tabs whose lazy resume/history load has already been triggered:
  // first focus fires it exactly once per app run (and guards the double
  // invoke of dev-mode StrictMode).
  const resumedTabs = useRef(new Set<string>());
```

- [ ] **Step 3: The two new callbacks**

Insert between the `outputReady` memo and `abandonTab`:

```ts
  // Prepend the resumed session's transcript prompts as display-only history
  // rows. They share the live timeline's array and cap; historical rows have
  // negative ids and no markers, so cap-trimming them needs no marker
  // cleanup, and live rows (always newer) are never the ones dropped here.
  const loadSessionHistory = useCallback(async (tabId: string) => {
    let prompts: HistoryPrompt[];
    try {
      prompts = await invoke<HistoryPrompt[]>('session_history', { tabId });
    } catch (err) {
      // TAB_GONE (the tab closed mid-load) or any backend failure: history
      // is best-effort and must never surface an error banner.
      console.debug('session_history failed:', messageOf(err));
      return;
    }
    if (prompts.length === 0) return;
    const historical = historyEvents(prompts);
    setTimelines((prev) => {
      // Idempotence guard: a StrictMode double-invoke or a list_tabs /
      // tab-created race must not prepend the same history twice.
      if (prev[tabId]?.some((e) => e.historical)) return prev;
      const merged = [...historical, ...(prev[tabId] ?? [])];
      if (merged.length <= MAX_TIMELINE_EVENTS) return { ...prev, [tabId]: merged };
      return { ...prev, [tabId]: merged.slice(-MAX_TIMELINE_EVENTS) };
    });
  }, []);

  // First focus of a restored tab (spec Q4's lazy path): issue its pending
  // resume, then load the transcript history. The startup-restored active
  // tab was already resumed by the backend — its payload arrives with
  // pendingResume false and restoredSession true, so only the history load
  // remains. Non-restored tabs are a no-op.
  const maybeResume = useCallback(
    (tab: Tab) => {
      if (!tab.pendingResume && !tab.restoredSession) return;
      if (resumedTabs.current.has(tab.id)) return;
      resumedTabs.current.add(tab.id);
      if (tab.pendingResume) {
        invoke('resume_tab', { tabId: tab.id })
          .then(() => loadSessionHistory(tab.id))
          .catch((err) => console.debug('resume_tab failed:', messageOf(err)));
      } else {
        void loadSessionHistory(tab.id);
      }
    },
    [loadSessionHistory]
  );
```

- [ ] **Step 4: `abandonTab`**

After `markers.current.delete(tabId);` add:

```ts
    resumedTabs.current.delete(tabId);
```

(A closed restored tab that somehow returns — it cannot, ids are UUIDs — must not stay blocked; this simply keeps the set from growing forever.)

- [ ] **Step 5: `switchTab`**

Replace the whole callback:

```ts
  const switchTab = useCallback((tabId: string) => {
    setActiveTabId(tabId);
    // Seeing the tab acknowledges its notification: clear it here and in the
    // registry, so a webview reload cannot resurrect an already-seen notice.
    if (tabsRef.current.some((tab) => tab.id === tabId && tab.notice)) {
      void invoke('ack_tab_notice', { tabId }).catch(() => {
        /* the tab may already be gone */
      });
    }
    setTabs((prev) =>
      prev.map((tab) => (tab.id === tabId ? { ...tab, flashing: false, notice: null } : tab))
    );
    // Report the new active tab to the backend — the snapshot's active index
    // decides which tab auto-resumes at the next launch.
    void invoke('set_active_tab', { tabId }).catch(() => {
      /* the tab may already be gone */
    });
    // First activation of a restored tab: lazy resume + history load.
    const switched = tabsRef.current.find((tab) => tab.id === tabId);
    if (switched) maybeResume(switched);
  }, [maybeResume]);
```

- [ ] **Step 6: The mount effect's `list_tabs` block**

Replace:

```ts
        setActiveTabId((current) => current ?? snapshot[0]?.id ?? null);
```

with:

```ts
        // Restore: the backend marks the previously-active tab isActive (the
        // first tab is not necessarily it after a reorder). This also covers
        // a webview reload mid-run: renderer state (timelines, resumedTabs)
        // died with the old document, so the history legitimately reloads.
        const preferred = snapshot.find((tab) => tab.isActive) ?? snapshot[0];
        setActiveTabId((current) => current ?? preferred?.id ?? null);
        if (preferred) maybeResume(preferred);
```

- [ ] **Step 7: The `tab-created` listener**

Replace:

```ts
        setActiveTabId(created.id);
```

with:

```ts
        // Only the tab the backend marked active takes focus: startup restore
        // announces EVERY restored tab through this event, and exactly one of
        // them was the user's (services::open_tab's tabs — startup fallback,
        // Finder service — always arrive isActive, keeping today's behavior).
        if (payload.isActive) {
          setActiveTabId(created.id);
          maybeResume(created);
        }
```

- [ ] **Step 8: The mount effect's dep list**

Replace:

```ts
  }, [abandonTab, outputReady]);
```

with:

```ts
  }, [abandonTab, maybeResume, outputReady]);
```

(`maybeResume` is stable — its dep `loadSessionHistory` has none — so the effect still subscribes exactly once.)

- [ ] **Step 9: Verify**

Run: `npm run typecheck`
Expected: PASS.

Run: `npm test`
Expected: PASS (131).

- [ ] **Step 10: Commit**

```bash
git add src/hooks/useTabManager.ts
git commit -m "$(cat <<'EOF'
feat: lazy resume and history loading in the renderer

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: Docs — AGENT_HOOKS.md, READMEs, CLAUDE.md, wiki

**Files:**
- Modify: `AGENT_HOOKS.md`, `README.md`, `README.zh-CN.md`, `CLAUDE.md`
- Modify (separate repo, cloned at `/tmp/clitab-wiki` — commit there, **do NOT push**): `Agent-Setup.md`, `Agent-Setup.zh.md`, `Everyday-Use.md`, `Everyday-Use.zh.md`

**Interfaces:**
- Consumes: the finished behavior of Tasks 1–8 (docs describe shipped code, nothing else).
- Produces: nothing code-facing.

No tests — verification is the byte-identical grep in step 6 plus a read-through. Docs-only commit at the end.

- [ ] **Step 1: `AGENT_HOOKS.md`**

**1a.** In the "How it works" Events bullet, replace the opening `Events:` sentence pair:

```
- Events: `{"e":"prompt","msg":"…"}` turn start, with the submitted prompt for
  the timeline (msg omitted without jq) · `{"e":"tool","tool":"Bash"}` running
```

with:

```
- Events: `{"e":"prompt","msg":"…","sid":"…","tp":"…","a":"claude"}` turn
  start, with the submitted prompt for the timeline (msg omitted without jq)
  and the session binding session restore runs on (`sid` = session id,
  `tp` = transcript path, `a` = agent kind, `"claude"` or `"qoder"`; all three
  are omitted without jq or with an old hook install — restore then degrades
  to plain-shell tabs) · `{"e":"tool","tool":"Bash"}` running
```

(the rest of the bullet — `a tool` through `no hook fires at that moment).` — stays verbatim).

**1b.** Replace the jq bullet:

```
- [jq](https://jqlang.github.io/jq/) powers tool names, notification text,
  submitted-prompt text and recorded answers. Without jq everything degrades
  gracefully — turn start/stop and flashing still work, only the detail text
  is missing.
```

with:

```
- [jq](https://jqlang.github.io/jq/) powers tool names, notification text,
  submitted-prompt text, recorded answers and the session binding
  (`sid`/`tp`/`a`) that session restore needs. Without jq everything degrades
  gracefully — turn start/stop and flashing still work, only the detail text
  is missing and restored tabs come back as plain shells.
```

**1c.** Insert a new section immediately before `## Qoder CLI notes`:

```markdown
## Session restore

clitab snapshots its tab list (`tab-snapshot.json` in the app config dir,
written on every snapshot-relevant change — a crash loses nothing) and
recreates it at the next launch: same tabs, order, titles and working
directories. A tab whose agent CLI was still running when the snapshot was
taken comes back with its session resumed — `<agent> --resume <sid>` is
injected into the tab's ordinary shell, which keeps shell integration (cwd
reporting, title revert) working and appends to the same transcript (no
`--fork-session`).

- The binding (`sid`/`tp`/`a`) rides the UserPromptSubmit payload and
  refreshes on every prompt; only the *latest* session per tab is recorded.
  Agent liveness comes from existing signals: any hook event ⇒ running,
  shell-integration `claude-done` (OSC 9) ⇒ exited. `--resume` itself fires
  no hook, so clitab marks a resumed tab alive on injection.
- Only the previously-active tab resumes at startup; the others resume on
  first activation.
- After a resume, the timeline prepends the session's historical user prompts,
  parsed from the transcript JSONL (strict rule: plain-string
  `message.content`, non-sidechain `type:"user"` records, injected wrappers
  like `<task-notification>` excluded). These **History** rows are
  display-only — the scrollback they belonged to is gone, so there is nothing
  to jump to.
- Degradation is silent at every step: hooks not (re)installed or missing jq
  ⇒ no binding ⇒ tabs restore as plain shells; deleted transcript ⇒ the
  resume is cancelled and a kept program title reverts to the cwd. The `sid`
  is charset-checked (`[A-Za-z0-9-]`) at decode time because it is
  interpolated into a shell command line.
```

- [ ] **Step 2: `README.md`**

Insert directly after the **Session timeline** bullet (before `- **Open from Finder**`):

```markdown
- **Session restore** — relaunch to the tabs you left behind: same order,
  same names, and the agent session you were in resumes where it stopped
  (the active tab automatically, the others on first click). Restored
  sessions show their earlier prompts as display-only **History** rows in
  the timeline.
```

- [ ] **Step 3: `README.zh-CN.md`**

Insert directly after the **会话时间线** bullet (before `- **从 Finder 打开**`):

```markdown
- **会话恢复** —— 重新打开 clitab 即回到上次离开时的标签:顺序、名称保持
  一致,离开时仍在运行的 agent 会话自动 `--resume` 接续(活动标签在启动时
  恢复,其余标签在首次点击时恢复)。恢复的会话会在时间线中以只读的
  **History** 行展示此前的用户 prompt。
```

- [ ] **Step 4: `CLAUDE.md`** (architecture sync — append/insert, never rewrite existing sentences)

**4a.** `lib.rs` bullet — after its final sentence "`src/types.ts` mirrors the constant — keep them in sync." append:

```
 The startup thread is restore-first: after the service grace it replays `tab-snapshot.json` via `TabManager::restore_snapshot` (a Finder request during the grace becomes an extra focused tab on top of the restore) and only falls back to the last-cwd flow when there is no snapshot. The renderer's restore path adds three commands — `set_active_tab` (the snapshot's active index), `resume_tab` (lazy `--resume` injection) and `session_history` (transcript prompts) — and `TabResponse` carries `is_active` / `pending_resume` / `restored_session`.
```

**4b.** `pty/manager.rs` bullet — after its final sentence "…(a persistent hourglass over an idle Claude)." append:

```
 The registry also carries the session-restore state — each tab's `SessionBinding`, `agent_running` (any hook event sets it, `claude-done` clears it), the resume flags and the active-tab id — and persists `restore.rs` snapshots on every snapshot-relevant change (turn-protocol mutators deliberately don't); its lock rule is clone-and-release, never two registry locks at once. On the manager side, `restore_snapshot` recreates the previous run's sidebar at startup — resuming the previously-active tab's session right away — and `resume_tab` injects `<agent> --resume <sid>` on a restored tab's first activation, degrading silently to a plain shell when the transcript is gone.
```

**4c.** `pty/session.rs` bullet — after its final sentence "…don't reorder emit before parse." append:

```
 `handle_osc` also keeps the restore state live: hook events refresh the tab's session binding and set `agent_running`; `PromptReady` (`claude-done`) clears the flag.
```

**4d.** `status.rs` bullet — after its final sentence "Invariant: `stop` never touches the title state — the `claude-done` title-revert path still owns it." append:

```
 `decode_session` additionally decodes the prompt payload's session binding (`sid`/`tp`/`a` → `SessionBinding`); the sid charset is validated there because it is interpolated into the resume shell command.
```

**4e.** Insert two new module bullets directly after the `last_cwd.rs` bullet (which ends "…a stale directory falls back via `resolve_cwd`."):

```markdown
- `restore.rs` — the tab snapshot (`app_config_dir/tab-snapshot.json`): version-gated, tmp+rename-atomic, best-effort like `last_cwd.rs`. Records cwd/title/program-title/agent-liveness, each tab's latest `SessionBinding` and the active-tab index; turn state is deliberately absent — a restored tab starts protocol-fresh.
- `history.rs` — streams a session transcript JSONL (the hook-supplied `tp` wins, else derived as `~/.claude|.qoder/projects/<cwd with / as ->/<sid>.jsonl`) and extracts the real user prompts (plain-string content, non-sidechain, injected wrappers excluded), capped to the timeline's 500-event budget. Malformed lines are skipped, never an error.
```

**4f.** Data flow "Events (Rust → renderer)" bullet — after its final sentence "…is removed from the session map." append:

```
 `TabResponse` (`create_tab` / `list_tabs` / `tab-created`) carries `isActive` / `pendingResume` / `restoredSession` for the restore path.
```

**4g.** Frontend `hooks/useTabManager.ts` bullet — append after "…the only place that talks to Rust.":

```
 It also drives session restore: reports the active tab (`set_active_tab`), issues a restored tab's lazy `--resume` on first activation and prepends the transcript's historical prompts as display-only rows.
```

**4h.** Frontend `lib/timeline.ts` bullet — after its final sentence "…because the FIFO stamping of queued prompts depends on it." append:

```
 `historyEvents` turns a restored session's transcript prompts into display-only rows (negative ids, no markers, never navigable).
```

- [ ] **Step 5: Wiki `Agent-Setup.md` + `Agent-Setup.zh.md`** (`/tmp/clitab-wiki`)

⚠️ The OLD command string below occurs **twice per file** — the Claude and Qoder UserPromptSubmit blocks are byte-identical today. Never replace-all: scope each replacement to its section (5a → the Claude Code block, 5b → the Qoder CLI block), or the Qoder hook would end up saying `a:"claude"`.

**5a.** In BOTH files, replace the UserPromptSubmit `command` string in the **Claude Code** JSON block — old (identical in both files):

```
            "command": "t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); j=$(jq -c '{e:\"prompt\",msg:.prompt}' 2>/dev/null || printf '{\"e\":\"prompt\"}'); [ -n \"$t\" ] && [ \"$t\" != '??' ] && printf '\\033]7777;%s\\033\\\\' \"$j\" > /dev/$t 2>/dev/null; true"
```

new:

```
            "command": "t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); j=$(jq -c '{e:\"prompt\",msg:.prompt,sid:.session_id,tp:.transcript_path,a:\"claude\"}' 2>/dev/null || printf '{\"e\":\"prompt\"}'); [ -n \"$t\" ] && [ \"$t\" != '??' ] && printf '\\033]7777;%s\\033\\\\' \"$j\" > /dev/$t 2>/dev/null; true"
```

**5b.** In BOTH files, replace the UserPromptSubmit `command` string in the **Qoder CLI** JSON block — same old string as 5a, new string identical to 5a's except the jq object ends `a:\"qoder\"}`:

```
            "command": "t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); j=$(jq -c '{e:\"prompt\",msg:.prompt,sid:.session_id,tp:.transcript_path,a:\"qoder\"}' 2>/dev/null || printf '{\"e\":\"prompt\"}'); [ -n \"$t\" ] && [ \"$t\" != '??' ] && printf '\\033]7777;%s\\033\\\\' \"$j\" > /dev/$t 2>/dev/null; true"
```

(The `|| printf '{\"e\":\"prompt\"}'` no-jq fallback stays unchanged on purpose: without jq there is no sid, and restore degrades — that is the documented contract.)

**5c.** Upgrade hazard — an old and a new UserPromptSubmit hook firing together would double-report every prompt (phantom queue entries). In BOTH install prompts of `Agent-Setup.md` (Claude Code section and Qoder section), insert after the bullet "- Merge the command strings verbatim — do not alter any escaping, quoting or whitespace;":

```
- If an older clitab UserPromptSubmit hook is already present (its jq builds {e:"prompt",msg:.prompt} without sid), replace that command with the new one instead of keeping both — two prompt hooks would double-report every turn;
```

In BOTH install prompts of `Agent-Setup.zh.md`, insert after the bullet "- 命令字符串必须逐字合并,不要改动任何转义、引号或空格;":

```
- 如果已存在旧版 clitab 的 UserPromptSubmit hook(其 jq 只生成 {e:"prompt",msg:.prompt},不含 sid),请用下面的新命令替换它,不要两条并存 —— 两个 prompt hook 会让每个回合被重复上报;
```

**5d.** In `Agent-Setup.md`'s "## After installing (both agents)" section, append a bullet after the jq bullet:

```
- **Upgrading from an older clitab?** Session restore needs the extended
  UserPromptSubmit payload (`sid`/`tp`/`a`): re-run the install prompt above —
  it replaces the old hook command. Until then everything else keeps working;
  restored tabs just come back as plain shells.
```

In `Agent-Setup.zh.md`'s "## 安装之后(两个 agent 通用)" section, append after the jq bullet:

```
- **从旧版 clitab 升级?** 会话恢复依赖扩展后的 UserPromptSubmit 载荷
  (`sid`/`tp`/`a`):重新执行上面的安装提示词即可 —— 它会替换旧的 hook
  命令。在此之前其他功能照常工作,只是恢复的标签会回到普通 shell。
```

- [ ] **Step 6: Wiki `Everyday-Use.md` + `Everyday-Use.zh.md`** — append at the end of each file

`Everyday-Use.md`:

```markdown

## Session Restore

Relaunching clitab brings back the tabs you left behind — same order, same
names, same directories:

- A tab whose agent (Claude Code / Qoder CLI) was **still running** when you
  quit comes back with its session resumed: the previously-active tab resumes
  automatically at startup, the others resume the first time you click them.
  The resumed session continues in the same transcript — nothing is forked or
  lost.
- Its earlier prompts appear in the [session
  timeline](https://github.com/dongritengfei/clitab/wiki/Agent-Features#session-timeline)
  as dimmed **History** rows. They are display-only: the terminal scrollback
  they belonged to died with the previous run, so there is nothing to jump to.
- A tab whose agent had already exited comes back as a plain shell in its old
  directory.
- Restore needs the [agent
  hooks](https://github.com/dongritengfei/clitab/wiki/Agent-Setup) installed —
  the extended UserPromptSubmit payload carries the session id. Without them,
  or when the session transcript has been deleted, tabs still restore, just as
  plain shells.
- The terminal scrollback itself is not restored; what you see is the agent's
  own resume repaint.
```

`Everyday-Use.zh.md`:

```markdown

## 会话恢复

重新打开 clitab 会带回你离开时的标签 —— 顺序、名称、目录保持一致:

- 退出时 agent(Claude Code / Qoder CLI)**仍在运行**的标签,恢复后自动
  接续会话:上次活动的标签在启动时立即恢复,其余标签在首次点击时恢复。
  接续的会话写回同一份 transcript —— 不分叉、不丢失。
- 会话此前的用户 prompt 会以暗色的 **History** 行出现在[会话时间线](https://github.com/dongritengfei/clitab/wiki/Agent-Features.zh#会话时间线)
  中。它们仅供查看:所属的终端回滚缓冲已随上次运行消失,无法跳转。
- agent 已退出的标签恢复为该目录下的普通 shell。
- 恢复依赖已安装的 [agent hooks](https://github.com/dongritengfei/clitab/wiki/Agent-Setup.zh)
  —— 扩展后的 UserPromptSubmit 载荷携带会话 id。未安装 hooks、或 transcript
  已被删除时,标签仍会恢复,只是回到普通 shell。
- 终端回滚缓冲本身不会恢复;你看到的是 agent 自己 resume 后的重绘。
```

- [ ] **Step 7: Verify the byte-identical rule**

Run: `grep -h 'msg:.prompt' /tmp/clitab-wiki/Agent-Setup.md /tmp/clitab-wiki/Agent-Setup.zh.md | grep 'jq -c' | sort | uniq -c`
Expected: exactly two distinct command lines, each with count **2** (the `a:\"claude\"` variant once per file, the `a:\"qoder\"` variant once per file). The second `grep 'jq -c'` filters out the 5c upgrade-note bullets, which quote the *old* payload and would skew the counts.

Also eyeball: `README.md` and `README.zh-CN.md` each gained exactly one bullet in the same position.

- [ ] **Step 8: Commit (repo docs, then wiki — wiki commit stays local, do NOT push)**

```bash
git add AGENT_HOOKS.md README.md README.zh-CN.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs: document session restore and the extended hook payload

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"

git -C /tmp/clitab-wiki add Agent-Setup.md Agent-Setup.zh.md Everyday-Use.md Everyday-Use.zh.md
git -C /tmp/clitab-wiki commit -m "$(cat <<'EOF'
docs: session restore setup and everyday-use notes

Co-Authored-By: Claude Code <noreply@anthropic.com>
EOF
)"
```

---

### Task 10: End-to-end verification (manual, with the user)

**Files:** none (verification only; fixes go through systematic-debugging and land as their own commits).

This task needs the real app, real agent CLIs and the user's own hook config — it cannot be automated. **ASK THE USER before touching `~/.claude/settings.json`** (step 2); everything else runs in the worktree.

- [ ] **Step 1: All gates green**

Run: `npm test` (expect 131) · `npm run typecheck` (expect clean) · `npm run build` (expect success).

- [ ] **Step 2: Hook config (user decision)**

Ask the user: the full restore loop needs the extended UserPromptSubmit hook (`sid`/`tp`/`a`). Offer to update their `~/.claude/settings.json` command to the new wiki string (Step 5a of Task 9), or have them re-run the wiki install prompt themselves. If they decline, verify the degraded path instead (tabs restore as plain shells — no resume, no history, no errors).

- [ ] **Step 3: Live run**

Run: `npm run dev:log` (tees to `/tmp/clitab-dev.log` for bug reports). Then walk the checklist; each item is pass/fail:

1. **Setup:** two tabs, both running `claude`; submit at least two prompts in each; leave tab 2 active.
2. **Snapshot:** `find ~/Library/Application\ Support -maxdepth 3 -name tab-snapshot.json` — one file; its JSON shows two tabs in order, both with `session.sid` and `agentRunning: true`, and `active: 1`.
3. **Mixed liveness:** exit claude in tab 1 (type `/exit`); the snapshot flips that tab to `agentRunning: false`. Quit the app (⌘Q → confirm).
4. **Relaunch:** same two tabs, same order, same titles, same cwds. Tab 2 (was active) has `claude --resume <sid>` already injected and the conversation repaints. Tab 1 is a plain shell at its cwd with the cwd title (its agent had exited — no resume offered).
5. **History rows:** tab 2's timeline shows the previous run's prompts as dimmed rows with a **History** badge, correct text and timestamps; they are not clickable (no hover highlight, disabled). New live prompts add normal navigable rows below them.
6. **Lazy resume:** repeat 1–3 but leave BOTH agents running at quit. Relaunch, then click the inactive restored tab: `claude --resume` is injected at that moment (not before), and its history loads.
7. **Deleted transcript (Review Focus 5):** with a pending-resume tab from step 6's setup, quit, `mv` its transcript JSONL aside, relaunch, click the tab → no resume, no error dialog, title reverts from the program title to the cwd, tab is a usable shell.
8. **Finder during grace (optional, spec D5):** quit, then Finder → right-click a folder → Services → "New clitab Tab Here". Cold start lands on the restored tabs PLUS one extra focused tab for that folder.

- [ ] **Step 4: Wrap up**

Re-run `npm test` + `npm run typecheck` after any fixes. Then report results to the user — merging the branch is their call (finishing-a-development-branch).

<!-- PLAN-END -->
