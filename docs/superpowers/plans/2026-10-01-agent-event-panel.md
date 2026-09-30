# Agent Event Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a right-side panel listing Claude Code conversation events (user prompt, agent question, agent done, user choice) with text + timestamps; clicking an entry scrolls the terminal to the position where the event happened.

**Architecture:** Claude Code hooks run a tiny POSIX-sh script that writes the hook's stdin JSON (truncated, base64-encoded) into the PTY stream as `OSC 9;clitab-agent;<b64>`. The Rust OSC parser decodes it *together with the sequence's absolute byte-stream offset*, stores the event per tab in the Registry, and emits an `agent-event` Tauri event. The renderer writes output chunks tagged with `seq`; when a write reaches an event's `seq` it registers an xterm marker at that exact buffer line. Clicking a panel row scrolls to the marker.

**Tech Stack:** Tauri 2 / Rust (portable-pty; existing `uuid`, `base64`, `serde_json` crates — **no new crates**), React 18 + xterm.js (`registerMarker` / `scrollLines` are existing public API — **no new npm deps**).

**Spec:** `docs/superpowers/specs/2026-10-01-agent-event-panel-design.md`

**Execution note (user requirement):** implementation runs in a git worktree — create it via superpowers:using-git-worktrees before Task 1.

## Global Constraints

- No new Rust crates, no new npm dependencies.
- `MAX_OSC_LEN` (4096) in `osc.rs` stays unchanged; the hook script caps stdin at 2800 bytes so the base64 payload (~3.8 KB) fits.
- Event text truncated to 140 chars (char-safe, CJK); per-tab event list capped at 200, oldest evicted first.
- Tauri event payload keys stay snake_case (`tab_id`), matching existing events. `AgentEvent` fields (`id`/`kind`/`text`/`time`/`seq`) are single words — identical in any casing convention.
- The re-attach dedup invariant is untouchable: `pty-output` chunks are emitted **while holding the stream lock**, each carrying its absolute `seq`; only the renderer-side `OutputHandler` signature gains the `seq` it already had internally.
- OSC handling in `read_loop` happens before the stream lock is taken (as today).
- Checks: Rust = `cargo test --manifest-path src-tauri/Cargo.toml --lib`; frontend = `npm run typecheck` (no test runner exists; do not add one).
- Every commit message ends with the attribution line:
  `Co-Authored-By: Claude Code <noreply@anthropic.com>`
- Existing `claude-done` → `prompt-ready` / flash / title-revert code path stays as-is; decoded events additionally drive the same effects (spec: "Preserving existing attention/title behavior").

## Review Focus

The five spec-implied failure modes no single task's tests fully exercise, most likely first. Each is pinned to the owning task below.

1. **A hook type whose output never reaches the PTY** (neither `/dev/tty` nor stdout) → that event kind silently missing while others work. Pinned by Task 1 (probe before any product code; if the probe fails, STOP and report — the design's core assumption is broken).
2. **Truncated JSON cut mid multi-byte UTF-8 char** (the 2800-byte cap lands inside a CJK prompt) → decode must not panic, salvage still yields the event kind. Pinned by Task 3 test `truncated_mid_multibyte_is_salvaged`.
3. **Forged / garbage `clitab-agent` OSC** from any program `cat`-ing binary noise → dropped without panic or state corruption. Pinned by Task 2 test `agent_event_requires_prefix` + Task 3 test `garbage_is_dropped`.
4. **`~/.claude/settings.json` in an unexpected shape** (hooks not an object, an event entry not an array, foreign hooks present) → merge normalizes clitab's entries and preserves user data byte-for-byte otherwise; malformed JSON is refused, never overwritten. Pinned by Task 6 tests `merge_preserves_foreign_hooks`, `merge_replaces_stale_clitab_entries`, `merge_normalizes_broken_shapes`.
5. **Jump target no longer reachable** (marker trimmed out of the 5000-line scrollback, or event seq older than the replay ring after a webview reload) → silent no-op, terminal keeps working. Pinned by Task 8's `isDisposed`/missing-marker guard + Task 11 manual acceptance item.

---

### Task 1: Probe — verify the hook → PTY path and payload shapes

Throwaway verification. **No product code.** The whole design rests on "a Claude Code hook process can write bytes that land in the PTY output stream"; verify it (and learn the real JSON shapes) before building anything.

**Files:**
- Create (throwaway, outside the repo): `/tmp/clitab-probe/.claude/settings.json`
- Output artifacts: `/tmp/clitab-probe/session.txt`, `/tmp/clitab-probe/*.json`

**Interfaces:**
- Consumes: nothing.
- Produces: verified facts — (a) which write path (`/dev/tty` and/or stdout) reaches the PTY per hook type; (b) exact stdin JSON shapes for `UserPromptSubmit`, `Notification`, `Stop`, `PostToolUse`+`AskUserQuestion` (especially where the user's selection lives in `tool_response`). Task 3's decoder is written against these shapes.

- [ ] **Step 1: Write the probe hook config**

```bash
mkdir -p /tmp/clitab-probe/.claude
cat > /tmp/clitab-probe/.claude/settings.json <<'EOF'
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command",
      "command": "cat > /tmp/clitab-probe/prompt.json; printf '\\033]9;clitab-probe;prompt\\033\\\\' > /dev/tty" }] }],
    "Notification": [{ "matcher": ".*", "hooks": [{ "type": "command",
      "command": "cat > /tmp/clitab-probe/notification.json; printf '\\033]9;clitab-probe;notification\\033\\\\' > /dev/tty" }] }],
    "Stop": [{ "hooks": [{ "type": "command",
      "command": "cat > /tmp/clitab-probe/stop.json; printf '\\033]9;clitab-probe;stop\\033\\\\' > /dev/tty" }] }],
    "PostToolUse": [{ "matcher": "AskUserQuestion", "hooks": [{ "type": "command",
      "command": "cat > /tmp/clitab-probe/choice.json; printf '\\033]9;clitab-probe;choice\\033\\\\' > /dev/tty" }] }]
  }
}
EOF
```

Each hook dumps its stdin JSON to a file (that is what we want to learn) and writes a distinguishable OSC marker to `/dev/tty`.

- [ ] **Step 2: Run Claude Code under `script` so the PTY stream is recorded**

```bash
cd /tmp/clitab-probe && script -q /tmp/clitab-probe/session.txt claude
```

Inside the session: (1) send any short prompt and let the turn finish (covers UserPromptSubmit + Stop + likely Notification); (2) send `请用 AskUserQuestion 工具问我一个单选题` and answer the choice (covers PostToolUse); (3) exit Claude, then `exit` the script session.

- [ ] **Step 3: Check the markers reached the PTY stream**

```bash
grep -c 'clitab-probe;' /tmp/clitab-probe/session.txt
grep -o 'clitab-probe;[a-z]*' /tmp/clitab-probe/session.txt | sort | uniq -c
```

Expected: count ≥ 1, and ideally all four kinds (`prompt`, `notification`, `stop`, `choice`) present. If `/dev/tty` markers are missing, retry with the redirect removed (bare `printf ...`, i.e. stdout) and re-run — record which path works. If neither path delivers for a hook type, note which; if NONE deliver, **STOP and report** (design risk materialized).

- [ ] **Step 4: Inspect the payload shapes**

```bash
for f in /tmp/clitab-probe/*.json; do echo "== $f"; head -c 600 "$f"; echo; done
```

Record: the exact key holding the prompt text (`prompt`), the notification text (`message`), and the structure of `tool_input` / `tool_response` for `AskUserQuestion` (where the user's selected option label lives). Paste these findings into the task report — Task 3's `choice_text` and salvage extractor are adjusted to match reality.

- [ ] **Step 5: Clean up and report**

```bash
rm -rf /tmp/clitab-probe
```

No commit (nothing in the repo changed). Report findings before starting Task 2.

---

### Task 2: `osc.rs` — `AgentEvent` variant + absolute stream offsets

**Files:**
- Modify: `src-tauri/src/osc.rs`
- Modify: `src-tauri/src/pty/session.rs` (call site only — keep it compiling; real wiring is Task 5)
- Test: inline `mod tests` in `src-tauri/src/osc.rs`

**Interfaces:**
- Consumes: nothing (leaf module).
- Produces: `OscEvent::AgentEvent(String)` (the base64 payload after `clitab-agent;`), and `OscParser::parse(&mut self, data: &[u8]) -> Vec<(OscEvent, u64)>` where the `u64` is the absolute stream position of the sequence's leading `ESC`. Tasks 3/5 rely on both.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `osc.rs`:

```rust
    #[test]
    fn agent_event_payload_and_offset() {
        let mut parser = OscParser::new();
        let parsed = parser.parse(b"hello\x1b]9;clitab-agent;QUJD\x1b\\tail");
        assert_eq!(parsed, vec![(OscEvent::AgentEvent("QUJD".into()), 5)]);
    }

    #[test]
    fn agent_event_offset_survives_split_across_reads() {
        let mut parser = OscParser::new();
        assert!(parser.parse(b"abcd\x1b]9;clitab-age").is_empty());
        let parsed = parser.parse(b"nt;QUJD\x07");
        assert_eq!(parsed, vec![(OscEvent::AgentEvent("QUJD".into()), 4)]);
    }

    #[test]
    fn agent_event_requires_prefix() {
        let mut parser = OscParser::new();
        // OSC 9 that is neither claude-done nor clitab-agent is ignored,
        // and must not corrupt the next sequence.
        assert!(parser.parse(b"\x1b]9;something-else\x07").is_empty());
        let parsed = parser.parse(b"\x1b]9;clitab-agent;QQ\x07");
        assert_eq!(parsed, vec![(OscEvent::AgentEvent("QQ".into()), 0)]);
    }

    #[test]
    fn offsets_count_every_fed_byte() {
        let mut parser = OscParser::new();
        assert!(parser.parse(b"12345678").is_empty());
        // Second feed: the BEL sits at absolute position 8.
        let parsed = parser.parse(b"\x07");
        assert_eq!(parsed, vec![(OscEvent::Bell, 8)]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib osc`
Expected: FAIL — `parse` returns `Vec<OscEvent>` (tuple assertions don't typecheck) and `AgentEvent` doesn't exist. Existing tests also fail to compile once the signature changes — that's Step 4's job.

- [ ] **Step 3: Implement the parser changes**

In `osc.rs`:

1. New enum variant:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OscEvent {
    TitleChanged(String),
    CwdChanged(String),
    Bell,
    PromptReady,
    /// OSC 9 `clitab-agent;<base64>`: an encoded agent conversation event,
    /// written into the PTY stream by the Claude Code hook script.
    AgentEvent(String),
}
```

2. New parser fields (position bookkeeping):

```rust
pub struct OscParser {
    state: State,
    buffer: Vec<u8>,
    params: Vec<Vec<u8>>,
    /// Total bytes ever fed to `parse()`. One parser instance per session,
    /// fed from stream position 0, so this equals the absolute seq — the
    /// same numbering `StreamState.position` uses.
    fed: u64,
    /// Absolute position of the ESC that began the OSC being accumulated.
    osc_start: u64,
    /// Absolute position of the most recently seen ESC (an OSC may begin at
    /// an ESC that first looked like something else, or re-begin after a
    /// nested-ESC resync).
    esc_pos: u64,
}
```

3. `parse()` counts positions and tags events. The state machine is unchanged except: every byte's absolute position is `self.fed` before increment; `Ground`'s `0x1b` arm and `InOsc`'s `0x1b` arm record `self.esc_pos = pos`; `AfterEsc`'s `b']'` arm and `InOscAfterEsc`'s `b']'` arm call `self.begin_osc(self.esc_pos)`; `Bell` is tagged with its own `pos`:

```rust
    pub fn parse(&mut self, data: &[u8]) -> Vec<(OscEvent, u64)> {
        let mut events = Vec::new();

        for &byte in data {
            let pos = self.fed;
            self.fed += 1;
            match self.state {
                State::Ground => match byte {
                    0x1b => {
                        self.esc_pos = pos;
                        self.state = State::AfterEsc;
                    }
                    0x07 => events.push((OscEvent::Bell, pos)),
                    _ => {}
                },
                State::AfterEsc => {
                    if byte == b']' {
                        self.begin_osc(self.esc_pos);
                    } else {
                        self.state = State::Ground;
                    }
                }
                State::InOsc => match byte {
                    0x07 => events.extend(self.finish_osc()),
                    b';' => self.push_param(),
                    0x1b => {
                        self.esc_pos = pos;
                        self.push_param();
                        self.state = State::InOscAfterEsc;
                    }
                    _ => {
                        if self.buffer.len() >= MAX_OSC_LEN {
                            self.abort_osc();
                        } else {
                            self.buffer.push(byte);
                        }
                    }
                },
                State::InOscAfterEsc => {
                    if byte == b'\\' {
                        events.extend(self.finish_osc());
                    } else if byte == b']' {
                        self.begin_osc(self.esc_pos);
                    } else {
                        self.buffer.push(0x1b);
                        self.buffer.push(byte);
                        self.state = State::InOsc;
                    }
                }
            }
        }

        events
    }
```

4. `begin_osc` / `finish_osc` carry the start position:

```rust
    fn begin_osc(&mut self, start: u64) {
        self.buffer.clear();
        self.params.clear();
        self.osc_start = start;
        self.state = State::InOsc;
    }

    /// Close the current OSC sequence and interpret it, tagged with the
    /// absolute position of its leading ESC.
    fn finish_osc(&mut self) -> Vec<(OscEvent, u64)> {
        self.push_param();
        let event = Self::interpret(&self.params);
        let start = self.osc_start;
        self.reset();
        event.map(|e| vec![(e, start)]).unwrap_or_default()
    }
```

5. `interpret` gains the OSC 9 branch (replace the existing `"9"` arm):

```rust
            "9" => {
                if value == "claude-done" {
                    Some(OscEvent::PromptReady)
                } else {
                    value
                        .strip_prefix("clitab-agent;")
                        .map(|payload| OscEvent::AgentEvent(payload.to_string()))
                }
            }
```

- [ ] **Step 4: Update the existing tests mechanically**

Every existing test asserts on `Vec<OscEvent>`; add a helper at the top of `mod tests` and route the old assertions through it (assertions themselves unchanged):

```rust
    /// Unwrap the offsets: legacy tests only care which events were decoded.
    fn ev(parser: &mut OscParser, data: &[u8]) -> Vec<OscEvent> {
        parser.parse(data).into_iter().map(|(e, _)| e).collect()
    }
```

Replace `parser.parse(X)` with `ev(&mut parser, X)` in every pre-existing test (also `assert!(parser.parse(X).is_empty())` → `assert!(ev(&mut parser, X).is_empty())`). The `runaway_osc_is_capped` test also touches `parser.buffer` — unchanged.

- [ ] **Step 5: Fix the `session.rs` call site (compile only)**

In `read_loop`:

```rust
                    for (event, _offset) in parser.parse(data) {
                        Self::handle_osc(
                            &tab_id,
                            &app,
                            &registry,
                            &program_active,
                            &last_activity,
                            event,
                        );
                    }
```

`handle_osc` gains an `OscEvent::AgentEvent(_) => {}` no-op arm for now (Task 5 fills it).

- [ ] **Step 6: Run the full Rust suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: PASS (all osc tests, new and updated).

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/osc.rs src-tauri/src/pty/session.rs
git commit -m "osc: decode clitab-agent payloads and report absolute stream offsets

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 3: `agent_events.rs` — payload decoding (pure, TDD)

**Files:**
- Create: `src-tauri/src/agent_events.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod agent_events;` next to the existing `mod osc;`)
- Test: inline `mod tests` in `agent_events.rs`

**Interfaces:**
- Consumes: nothing (pure functions over the base64 string from `OscEvent::AgentEvent`).
- Produces (Tasks 4/5 rely on these exact names):
  - `pub enum AgentEventKind { UserPrompt, AgentQuestion, AgentDone, UserChoice }` — serde-serialized kebab-case (`"user-prompt"` etc.), derives `Debug, Clone, Copy, PartialEq, Eq, Serialize`.
  - `pub struct AgentEvent { pub id: String, pub kind: AgentEventKind, pub text: String, pub time: u64, pub seq: u64 }` — derives `Debug, Clone, PartialEq, Serialize`.
  - `pub fn decode_agent_event(payload_b64: &str) -> Option<(AgentEventKind, String)>`
  - `pub const TEXT_LIMIT: usize = 140`

**Note:** the `choice_text` extraction below is written against the shape observed in Task 1. If the probe showed a different `tool_response` structure for `AskUserQuestion`, adjust `choice_text` AND its test to the real shape before implementing — that is the whole point of probing first.

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/agent_events.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

    fn payload(json: &str) -> String {
        BASE64.encode(json)
    }

    #[test]
    fn user_prompt_full_json() {
        let p = payload(r#"{"session_id":"s","cwd":"/tmp","hook_event_name":"UserPromptSubmit","prompt":"fix the build"}"#);
        assert_eq!(
            decode_agent_event(&p),
            Some((AgentEventKind::UserPrompt, "fix the build".into()))
        );
    }

    #[test]
    fn notification_full_json() {
        let p = payload(r#"{"hook_event_name":"Notification","message":"Claude needs your permission to use Bash"}"#);
        assert_eq!(
            decode_agent_event(&p),
            Some((AgentEventKind::AgentQuestion, "Claude needs your permission to use Bash".into()))
        );
    }

    #[test]
    fn stop_has_empty_text() {
        let p = payload(r#"{"hook_event_name":"Stop"}"#);
        assert_eq!(decode_agent_event(&p), Some((AgentEventKind::AgentDone, String::new())));
    }

    #[test]
    fn ask_user_question_choice() {
        // Shape per Task 1 probe; adjust if the real payload differs.
        let p = payload(r#"{"hook_event_name":"PostToolUse","tool_name":"AskUserQuestion","tool_response":{"answers":{"Which approach?":"Option B"}}}"#);
        assert_eq!(
            decode_agent_event(&p),
            Some((AgentEventKind::UserChoice, "Option B".into()))
        );
    }

    #[test]
    fn posttooluse_other_tool_ignored() {
        let p = payload(r#"{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_response":{}}"#);
        assert_eq!(decode_agent_event(&p), None);
    }

    #[test]
    fn unknown_hook_event_ignored() {
        let p = payload(r#"{"hook_event_name":"PreToolUse"}"#);
        assert_eq!(decode_agent_event(&p), None);
    }

    #[test]
    fn truncated_json_salvages_prompt() {
        // The hook script caps stdin at 2800 bytes: the JSON can arrive cut
        // mid-value with no closing braces.
        let p = payload(r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"refactor the parser and update all call sites"#);
        assert_eq!(
            decode_agent_event(&p),
            Some((AgentEventKind::UserPrompt, "refactor the parser and update all call sites".into()))
        );
    }

    #[test]
    fn truncated_json_with_escapes_is_unescaped() {
        let p = payload(r#"{"hook_event_name":"UserPromptSubmit","prompt":"line one\nline two \"quoted\""#);
        assert_eq!(
            decode_agent_event(&p),
            Some((AgentEventKind::UserPrompt, "line one\nline two \"quoted\"".into()))
        );
    }

    #[test]
    fn truncated_mid_multibyte_is_salvaged() {
        // Cut the raw bytes in the middle of a CJK char: from_utf8_lossy must
        // absorb it and the event kind must still decode.
        let json = r#"{"hook_event_name":"UserPromptSubmit","prompt":"中文输入"#;
        let bytes = json.as_bytes();
        let cut = &bytes[..bytes.len() - 1]; // splits the last UTF-8 char
        let p = BASE64.encode(cut);
        let (kind, text) = decode_agent_event(&p).expect("salvaged");
        assert_eq!(kind, AgentEventKind::UserPrompt);
        assert!(text.starts_with("中文"), "got {text:?}");
    }

    #[test]
    fn garbage_is_dropped() {
        assert_eq!(decode_agent_event("!!!not-base64!!!"), None);
        assert_eq!(decode_agent_event(&BASE64.encode([0x00u8, 0xff, 0xfe, 0x01])), None);
        assert_eq!(decode_agent_event(""), None);
    }

    #[test]
    fn text_truncated_to_limit_chars() {
        let long: String = "字".repeat(300);
        let p = payload(&format!(r#"{{"hook_event_name":"UserPromptSubmit","prompt":"{long}"}}"#));
        let (_, text) = decode_agent_event(&p).unwrap();
        assert_eq!(text.chars().count(), TEXT_LIMIT);
        assert!(text.chars().all(|c| c == '字'));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib agent_events`
Expected: FAIL — module empty / items missing.

- [ ] **Step 3: Implement the module**

Above the test module in `agent_events.rs`:

```rust
//! Decoding of `clitab-agent` OSC payloads.
//!
//! The Claude Code hook script (installed by `install_claude_hooks`, see
//! `claude_hooks.rs`) forwards each hook's stdin JSON — truncated to 2800
//! bytes and base64-encoded — through the PTY stream as an OSC 9 sequence.
//! Decoding is deliberately two-tiered: a strict JSON parse for intact
//! payloads, then a string-search salvage for payloads the truncation cut
//! open. Anything unrecognisable is dropped: a `cat` of binary noise must
//! never fabricate (or crash on) a conversation event.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::Serialize;

/// Maximum characters of summary text kept per event.
pub const TEXT_LIMIT: usize = 140;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentEventKind {
    UserPrompt,
    AgentQuestion,
    AgentDone,
    UserChoice,
}

/// One conversation event, anchored to the PTY byte stream via `seq`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentEvent {
    pub id: String,
    pub kind: AgentEventKind,
    /// Summary text, at most `TEXT_LIMIT` chars.
    pub text: String,
    /// Unix milliseconds: when Rust received the OSC.
    pub time: u64,
    /// Absolute PTY stream position where the OSC sequence started.
    pub seq: u64,
}

/// Decode the base64 body of a `clitab-agent` OSC into a kind and summary
/// text, or `None` when nothing trustworthy can be extracted.
pub fn decode_agent_event(payload_b64: &str) -> Option<(AgentEventKind, String)> {
    if payload_b64.is_empty() {
        return None;
    }
    let bytes = BASE64.decode(payload_b64.trim()).ok()?;
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
        if let Some(decoded) = decode_strict(&value) {
            return Some(decoded);
        }
    }
    decode_salvage(&bytes)
}

fn decode_strict(value: &serde_json::Value) -> Option<(AgentEventKind, String)> {
    match value.get("hook_event_name").and_then(|v| v.as_str())? {
        "UserPromptSubmit" => Some((AgentEventKind::UserPrompt, text_of(value, "prompt"))),
        "Notification" => Some((AgentEventKind::AgentQuestion, text_of(value, "message"))),
        "Stop" => Some((AgentEventKind::AgentDone, String::new())),
        "PostToolUse" => {
            if value.get("tool_name").and_then(|v| v.as_str()) != Some("AskUserQuestion") {
                return None;
            }
            Some((AgentEventKind::UserChoice, choice_text(value)))
        }
        _ => None,
    }
}

fn text_of(value: &serde_json::Value, key: &str) -> String {
    truncate_chars(value.get(key).and_then(|v| v.as_str()).unwrap_or(""), TEXT_LIMIT)
}

/// The user's selection from an `AskUserQuestion` PostToolUse payload. The
/// `tool_response` shape varies across Claude Code versions (Task 1 probe
/// pinned the current one): prefer the answers' values, fall back to the raw
/// response JSON so the row is never empty.
fn choice_text(value: &serde_json::Value) -> String {
    let response = value.get("tool_response");
    if let Some(answers) = response
        .and_then(|r| r.get("answers"))
        .and_then(|a| a.as_object())
    {
        let joined = answers
            .values()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if !joined.is_empty() {
            return truncate_chars(&joined, TEXT_LIMIT);
        }
    }
    match response {
        Some(r) => truncate_chars(&r.to_string(), TEXT_LIMIT),
        None => String::new(),
    }
}

/// Best-effort extraction from JSON the 2800-byte cap cut open.
fn decode_salvage(bytes: &[u8]) -> Option<(AgentEventKind, String)> {
    let raw = String::from_utf8_lossy(bytes);
    let kind = match json_string_field(&raw, "hook_event_name")?.as_str() {
        "UserPromptSubmit" => AgentEventKind::UserPrompt,
        "Notification" => AgentEventKind::AgentQuestion,
        "Stop" => AgentEventKind::AgentDone,
        "PostToolUse" => AgentEventKind::UserChoice,
        _ => return None,
    };
    let key = match kind {
        AgentEventKind::UserPrompt => "prompt",
        AgentEventKind::AgentQuestion => "message",
        _ => "",
    };
    let text = if key.is_empty() {
        String::new()
    } else {
        truncate_chars(&json_string_field(&raw, key).unwrap_or_default(), TEXT_LIMIT)
    };
    Some((kind, text))
}

/// Find `"key":"value"` in possibly-broken JSON. Handles `\"`, `\\`, `\n`,
/// `\t`; an unterminated value (the truncation case) yields what was read.
fn json_string_field(raw: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = raw.find(&needle)? + needle.len();
    let mut out = String::new();
    let mut chars = raw[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => return Some(out),
            },
            _ => out.push(c),
        }
        if out.chars().count() >= TEXT_LIMIT {
            return Some(out);
        }
    }
    Some(out)
}

fn truncate_chars(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        s.to_string()
    } else {
        s.chars().take(limit).collect()
    }
}
```

Also add `mod agent_events;` to `lib.rs` (alphabetically before `mod menu;`).

**Known salvage caveat (accepted):** `json_string_field` matches the first occurrence of `"prompt":"` anywhere in the payload — if a Notification message itself contains that literal substring, salvage could pick the wrong span. Strict parsing handles all intact payloads correctly; salvage only runs on truncated ones, where a slightly-wrong summary beats a dropped event.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib agent_events`
Expected: PASS (all 11 tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_events.rs src-tauri/src/lib.rs
git commit -m "agent_events: decode hook OSC payloads with strict + salvage tiers

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 4: `Registry` — per-tab in-memory event store

**Files:**
- Modify: `src-tauri/src/pty/registry.rs`
- Test: inline `mod tests` in `registry.rs`

**Interfaces:**
- Consumes: `crate::agent_events::AgentEvent` (Task 3).
- Produces (Tasks 5/6 rely on these):
  - `Registry::push_agent_event(&self, tab_id: &str, event: AgentEvent)`
  - `Registry::agent_events(&self, tab_id: &str) -> Vec<AgentEvent>`
  - `Registry::remove` additionally clears the tab's events.
  - Cap: `const AGENT_EVENT_LIMIT: usize = 200` (module-private).

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `registry.rs`:

```rust
    fn event(id: &str, seq: u64) -> crate::agent_events::AgentEvent {
        crate::agent_events::AgentEvent {
            id: id.to_string(),
            kind: crate::agent_events::AgentEventKind::UserPrompt,
            text: id.to_string(),
            time: 0,
            seq,
        }
    }

    #[test]
    fn agent_events_round_trip_in_order() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        registry.push_agent_event("t1", event("a", 1));
        registry.push_agent_event("t1", event("b", 2));
        let ids: Vec<_> = registry.agent_events("t1").iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids, vec!["a", "b"]);
        assert!(registry.agent_events("nope").is_empty());
    }

    #[test]
    fn agent_events_cap_evicts_oldest() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        for i in 0..205 {
            registry.push_agent_event("t1", event(&format!("e{i}"), i));
        }
        let events = registry.agent_events("t1");
        assert_eq!(events.len(), 200);
        assert_eq!(events.first().unwrap().id, "e5");
        assert_eq!(events.last().unwrap().id, "e204");
    }

    #[test]
    fn removing_a_tab_clears_its_events() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        registry.push_agent_event("t1", event("a", 1));
        registry.remove("t1");
        assert!(registry.agent_events("t1").is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib registry`
Expected: FAIL — `push_agent_event` / `agent_events` don't exist.

- [ ] **Step 3: Implement**

In `registry.rs`:

```rust
use crate::agent_events::AgentEvent;
use std::collections::{HashMap, VecDeque};

/// Per-tab cap on retained agent events; oldest are evicted first.
const AGENT_EVENT_LIMIT: usize = 200;

#[derive(Debug, Default)]
pub struct Registry {
    tabs: Mutex<Vec<TabRecord>>,
    /// Conversation events per tab. In-memory only: survives a webview
    /// reload (like titles), cleared when the tab is removed.
    agent_events: Mutex<HashMap<String, VecDeque<AgentEvent>>>,
}
```

New methods on `impl Registry`:

```rust
    /// Store one decoded conversation event for `tab_id`, evicting the oldest
    /// past the cap.
    pub fn push_agent_event(&self, tab_id: &str, event: AgentEvent) {
        let mut map = lock(&self.agent_events);
        let events = map.entry(tab_id.to_string()).or_default();
        events.push_back(event);
        while events.len() > AGENT_EVENT_LIMIT {
            events.pop_front();
        }
    }

    pub fn agent_events(&self, tab_id: &str) -> Vec<AgentEvent> {
        lock(&self.agent_events)
            .get(tab_id)
            .map(|events| events.iter().cloned().collect())
            .unwrap_or_default()
    }
```

And in the existing `remove`, after `tabs.retain(...)` (still fine to do under the same function, separate lock):

```rust
        lock(&self.agent_events).remove(id);
```

(`remove` computes `before`/returns based on `tabs`; add the events cleanup before the return.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib registry`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/pty/registry.rs
git commit -m "registry: store per-tab agent conversation events (cap 200)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 5: `session.rs` — wire decoded events into store, IPC, and legacy attention behavior

**Files:**
- Modify: `src-tauri/src/pty/session.rs` (`read_loop` call site + `handle_osc`)
- Test: no new unit tests (this is AppHandle-bound wiring; its parts are tested in Tasks 2–4). The full suite must stay green.

**Interfaces:**
- Consumes: `OscEvent::AgentEvent` + offset (Task 2), `decode_agent_event` / `AgentEvent` / `AgentEventKind` (Task 3), `Registry::push_agent_event` (Task 4), `uuid` crate (already a dependency).
- Produces: the Tauri event **`agent-event`** with payload `{ "tab_id": String, "event": AgentEvent }` — Task 7 listens to exactly this shape. Also emits `tab-flash` (kinds `agent-question`, `agent-done`) and `prompt-ready` (kind `agent-done`), so panel and legacy attention behavior arrive together.

- [ ] **Step 1: Pass the offset through `read_loop`**

Replace the Task-2 interim loop:

```rust
                    for (event, offset) in parser.parse(data) {
                        Self::handle_osc(
                            &tab_id,
                            &app,
                            &registry,
                            &program_active,
                            &last_activity,
                            event,
                            offset,
                        );
                    }
```

- [ ] **Step 2: Extend `handle_osc`**

Signature gains `offset: u64` (add `#[allow(clippy::too_many_arguments)]` — the function already has 5 params; the codebase uses this attribute on `read_loop`). Add imports at the top of `session.rs`:

```rust
use crate::agent_events::{self, AgentEvent, AgentEventKind};
```

New arm (replacing the Task-2 no-op):

```rust
            OscEvent::AgentEvent(payload) => {
                // Untrusted input: anything that fails to decode is dropped.
                let Some((kind, text)) = agent_events::decode_agent_event(&payload) else {
                    return;
                };
                let event = AgentEvent {
                    id: uuid::Uuid::new_v4().to_string(),
                    kind,
                    text,
                    time: now_ms(),
                    seq: offset,
                };
                registry.push_agent_event(tab_id, event.clone());
                let _ = app.emit(
                    "agent-event",
                    serde_json::json!({ "tab_id": tab_id, "event": event }),
                );
                if kind == AgentEventKind::AgentDone {
                    // Same effects as the legacy `claude-done` OSC: the turn
                    // ended, so drop the program title and re-arm the watcher.
                    program_active.store(false, Ordering::Relaxed);
                    registry.clear_program_title(tab_id);
                    *lock(last_activity) = Instant::now();
                    let _ = app.emit("prompt-ready", serde_json::json!({ "tab_id": tab_id }));
                }
                if matches!(kind, AgentEventKind::AgentQuestion | AgentEventKind::AgentDone) {
                    let _ = app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
                }
            }
```

Helper (module level, near `emit_output`):

```rust
/// Unix milliseconds; 0 if the clock is somehow before the epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
```

Add `use std::time::SystemTime;` only if you reference it unqualified — the helper above is fully qualified, so no import change beyond `agent_events`.

**Invariant check:** this arm runs inside `handle_osc`, i.e. *before* the stream lock is taken — same as every other OSC event today. Do not move any emit into or after the stream-lock section.

- [ ] **Step 3: Run the full Rust suite + build**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib && cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS / no warnings from the changed code.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/pty/session.rs
git commit -m "session: emit agent-event for decoded hook OSCs, reuse prompt-ready effects

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 6: One-click hook install — script, merge logic, Tauri commands

**Files:**
- Create: `src-tauri/hooks/clitab-hook.sh` (embedded into the binary at compile time)
- Create: `src-tauri/src/claude_hooks.rs`
- Modify: `src-tauri/src/lib.rs` (`mod claude_hooks;`, three commands, handler list)
- Modify: `src-tauri/src/pty/manager.rs` (`list_agent_events` passthrough)
- Test: inline `mod tests` in `claude_hooks.rs`

**Interfaces:**
- Consumes: `Registry::agent_events` (Task 4), `crate::agent_events::AgentEvent` (Task 3).
- Produces (Task 7 invokes these by name):
  - Tauri command `list_agent_events(tab_id: String) -> Result<Vec<AgentEvent>, String>` — `TAB_GONE`-prefixed error for a vanished tab.
  - Tauri command `claude_hooks_status() -> bool`.
  - Tauri command `install_claude_hooks() -> Result<(), String>`.
  - Pure fn `pub fn merge_hooks(settings: serde_json::Value, script_path: &str) -> serde_json::Value`.
  - `pub const SCRIPT_MARKER: &str = "clitab-hook.sh"` — ownership detection.

- [ ] **Step 1: Create the hook script**

`src-tauri/hooks/clitab-hook.sh` (Task 1's probe decides whether `/dev/tty` or the stdout fallback is the primary path — the script below tries tty first, falls back to stdout; if the probe showed only one path works, keep both anyway, the fallback is one `||`):

```sh
#!/bin/sh
# clitab Claude Code hook: forwards this hook's stdin JSON to clitab as an
# OSC 9 payload riding the PTY output stream, where clitab's parser turns it
# into a conversation-event panel entry.
#
# Installed by clitab (install_claude_hooks). Must never block or break
# Claude Code: every failure path is silent and exits 0.
payload=$(head -c 2800)
[ -n "$payload" ] || exit 0
b64=$(printf '%s' "$payload" | base64 | tr -d '\n')
[ -n "$b64" ] || exit 0
osc=$(printf '\033]9;clitab-agent;%s\033\\' "$b64")
printf '%s' "$osc" > /dev/tty 2>/dev/null || printf '%s' "$osc"
exit 0
```

`chmod +x` is NOT needed in-repo (permissions don't survive `include_str!`); the installer sets 0755 on the written copy.

- [ ] **Step 2: Write the failing merge tests**

Create `src-tauri/src/claude_hooks.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_adds_all_four_hooks_to_empty_settings() {
        let merged = merge_hooks(json({}), "/path/to/clitab-hook.sh");
        let hooks = merged.get("hooks").unwrap();
        for event in ["UserPromptSubmit", "Notification", "Stop", "PostToolUse"] {
            let groups = hooks.get(event).and_then(|g| g.as_array()).unwrap();
            assert_eq!(groups.len(), 1, "{event}");
            let cmd = groups[0]["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(cmd, "/path/to/clitab-hook.sh");
        }
        assert_eq!(hooks["Notification"][0]["matcher"], json(".*"));
        assert_eq!(hooks["PostToolUse"][0]["matcher"], json("AskUserQuestion"));
        assert!(hooks["UserPromptSubmit"][0].get("matcher").is_none());
        assert!(hooks["Stop"][0].get("matcher").is_none());
    }

    #[test]
    fn merge_preserves_foreign_hooks() {
        let settings = json({
            "hooks": {
                "Notification": [{ "matcher": ".*", "hooks": [
                    { "type": "command", "command": "printf '\\a'" }
                ]}],
                "PreToolUse": [{ "hooks": [
                    { "type": "command", "command": "my-own-tool" }
                ]}]
            },
            "theme": "dark"
        });
        let merged = merge_hooks(settings.clone(), "/p/clitab-hook.sh");
        // The user's own Notification hook is untouched, ours is appended.
        let groups = merged["hooks"]["Notification"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["hooks"][0]["command"], json("printf '\\a'"));
        assert_eq!(groups[1]["hooks"][0]["command"], json("/p/clitab-hook.sh"));
        // Unrelated keys and hook events survive verbatim.
        assert_eq!(merged["theme"], json("dark"));
        assert_eq!(merged["hooks"]["PreToolUse"], settings["hooks"]["PreToolUse"]);
    }

    #[test]
    fn merge_replaces_stale_clitab_entries() {
        let settings = json({
            "hooks": {
                "Notification": [{ "matcher": ".*", "hooks": [
                    { "type": "command", "command": "/old/path/clitab-hook.sh" }
                ]}],
                "Stop": [{ "hooks": [
                    { "type": "command", "command": "/old/path/clitab-hook.sh" }
                ]}]
            }
        });
        let merged = merge_hooks(settings, "/new/path/clitab-hook.sh");
        let cmd = |event: &str| merged["hooks"][event][0]["hooks"][0]["command"].clone();
        assert_eq!(cmd("Notification"), json("/new/path/clitab-hook.sh"));
        assert_eq!(cmd("Stop"), json("/new/path/clitab-hook.sh"));
        // Exactly one clitab group per event: no duplicates after reinstall.
        for event in ["UserPromptSubmit", "Notification", "Stop", "PostToolUse"] {
            let n = merged["hooks"][event].as_array().unwrap().iter()
                .filter(|g| group_owns_clitab(g)).count();
            assert_eq!(n, 1, "{event}");
        }
    }

    #[test]
    fn merge_normalizes_broken_shapes() {
        // `hooks` not an object, an event entry not an array, a group without
        // a hooks array: none of these may panic or block the install.
        let settings = json({ "hooks": "garbage" });
        let merged = merge_hooks(settings, "/p/clitab-hook.sh");
        assert_eq!(merged["hooks"]["Stop"][0]["hooks"][0]["command"], json("/p/clitab-hook.sh"));

        let settings = json({ "hooks": { "Stop": "garbage", "Notification": [ { "matcher": "x" } ] } });
        let merged = merge_hooks(settings, "/p/clitab-hook.sh");
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        // The foreign group without a hooks array is preserved (not ours).
        let notif = merged["hooks"]["Notification"].as_array().unwrap();
        assert_eq!(notif.len(), 2);
        assert_eq!(notif[0]["matcher"], json("x"));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib claude_hooks`
Expected: FAIL — `merge_hooks` / `group_owns_clitab` missing.

- [ ] **Step 4: Implement `claude_hooks.rs`**

Above the test module:

```rust
//! One-click installation of the Claude Code hooks that feed the agent event
//! panel. The hook script is embedded in the binary and written to the app
//! data dir; `~/.claude/settings.json` is merged — never clobbered: foreign
//! hooks survive, clitab-owned entries (recognised by the script filename in
//! their command) are replaced on reinstall, and a malformed settings file
//! is refused untouched.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Filament that identifies a hook entry as ours: the script's name appears
/// in its `command`. Path-independent so a moved app data dir still matches.
pub const SCRIPT_MARKER: &str = "clitab-hook.sh";

/// The hook script, embedded at compile time (see hooks/clitab-hook.sh).
pub const HOOK_SCRIPT: &str = include_str!("../hooks/clitab-hook.sh");

/// The four hook events clitab registers, with their matcher (None = omit).
fn registrations() -> [(&'static str, Option<&'static str>); 4] {
    [
        ("UserPromptSubmit", None),
        ("Notification", Some(".*")),
        ("Stop", None),
        ("PostToolUse", Some("AskUserQuestion")),
    ]
}

pub fn settings_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
        .join(".claude")
        .join("settings.json")
}

/// True when settings.json already contains clitab-owned hook entries.
pub fn hooks_installed(path: &Path) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .map(|settings| {
            settings
                .get("hooks")
                .map(|hooks| hooks.to_string().contains(SCRIPT_MARKER))
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// True when a hook group contains at least one clitab-owned command.
pub fn group_owns_clitab(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|list| {
            list.iter().any(|hook| {
                hook.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains(SCRIPT_MARKER))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Add (or replace) clitab's four hook registrations in a settings document.
/// Normalizes broken shapes instead of panicking; foreign data is preserved.
pub fn merge_hooks(mut settings: Value, script_path: &str) -> Value {
    if !settings.is_object() {
        settings = json({});
    }
    let root = settings.as_object_mut().expect("checked above");
    let hooks = root.entry("hooks").or_insert_with(|| json({}));
    if !hooks.is_object() {
        *hooks = json({});
    }
    for (event, matcher) in registrations() {
        let entry = hooks
            .as_object_mut()
            .expect("checked above")
            .entry(event)
            .or_insert_with(|| json([]));
        if !entry.is_array() {
            *entry = json([]);
        }
        let groups = entry.as_array_mut().expect("checked above");
        groups.retain(|group| !group_owns_clitab(group));
        let mut group = serde_json::Map::new();
        if let Some(m) = matcher {
            group.insert("matcher".into(), json(m));
        }
        group.insert(
            "hooks".into(),
            json([{ "type": "command", "command": script_path }]),
        );
        groups.push(Value::Object(group));
    }
    settings
}

/// Write the hook script and merge the registrations into settings.json.
/// `app_data` is the Tauri app data dir. Errors leave the settings file
/// untouched; the first successful install writes a `settings.json.bak`.
pub fn install(app_data: &Path) -> Result<(), String> {
    let hooks_dir = app_data.join("hooks");
    std::fs::create_dir_all(&hooks_dir).map_err(|e| format!("could not create hooks dir: {e}"))?;
    let script_path = hooks_dir.join(SCRIPT_MARKER);
    std::fs::write(&script_path, HOOK_SCRIPT).map_err(|e| format!("could not write hook script: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("could not mark hook script executable: {e}"))?;
    }

    let settings_path = settings_path();
    let existing = match std::fs::read_to_string(&settings_path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("could not read settings.json: {e}")),
    };
    let current: Value = match &existing {
        Some(text) => serde_json::from_str(text)
            .map_err(|_| "settings.json is not valid JSON; refusing to touch it".to_string())?,
        None => json({}),
    };

    if let Some(text) = &existing {
        let backup = settings_path.with_file_name("settings.json.bak");
        if !backup.exists() {
            std::fs::write(&backup, text)
                .map_err(|e| format!("could not write settings backup: {e}"))?;
        }
    }

    let merged = merge_hooks(current, &script_path.to_string_lossy());
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create ~/.claude: {e}"))?;
    }
    let pretty = serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?;
    std::fs::write(&settings_path, pretty).map_err(|e| format!("could not write settings.json: {e}"))?;
    Ok(())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib claude_hooks`
Expected: PASS (4 tests).

- [ ] **Step 6: Add the `TabManager` passthrough**

In `src-tauri/src/pty/manager.rs` (imports: `use crate::agent_events::AgentEvent;`):

```rust
    /// The tab's stored conversation events (panel restore after a reload).
    /// Unknown tabs report NotFound so the renderer sees the TAB_GONE marker.
    pub fn list_agent_events(&self, tab_id: &str) -> Result<Vec<AgentEvent>, ManagerError> {
        if self.registry.get(tab_id).is_none() {
            return Err(ManagerError::NotFound(tab_id.to_string()));
        }
        Ok(self.registry.agent_events(tab_id))
    }
```

- [ ] **Step 7: Add the three commands in `lib.rs`**

Imports: `use crate::agent_events::AgentEvent;` and (inside `install_claude_hooks`) `tauri::Manager` is already imported for `.path()`.

```rust
/// Stored conversation events of a tab, for panel restore after a webview
/// reload. Live updates arrive via the `agent-event` Tauri event.
#[tauri::command]
fn list_agent_events(
    state: State<'_, AppState>,
    tab_id: String,
) -> Result<Vec<AgentEvent>, String> {
    state.tab_manager.list_agent_events(&tab_id).map_err(ipc_error)
}

#[tauri::command]
fn claude_hooks_status() -> bool {
    claude_hooks::hooks_installed(&claude_hooks::settings_path())
}

/// Write the embedded hook script into the app data dir and merge the four
/// hook registrations into ~/.claude/settings.json (idempotent).
#[tauri::command]
fn install_claude_hooks(app: tauri::AppHandle) -> Result<(), String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("could not resolve app data dir: {e}"))?;
    claude_hooks::install(&app_data)
}
```

Add `mod claude_hooks;` at the top, and `list_agent_events, claude_hooks_status, install_claude_hooks` to the `generate_handler!` list.

- [ ] **Step 8: Run full suite + typecheck of the whole backend**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib && cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/hooks/clitab-hook.sh src-tauri/src/claude_hooks.rs src-tauri/src/lib.rs src-tauri/src/pty/manager.rs
git commit -m "hooks: one-click Claude Code hook install + list_agent_events command

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 7: Frontend plumbing — types, event state, seq-tagged output handler

**Files:**
- Modify: `src/types.ts`
- Modify: `src/hooks/useTabManager.ts`
- Modify: `src/components/Terminal.tsx` (signature adaptation only — anchoring is Task 8)
- Verify: `npm run typecheck`

**Interfaces:**
- Consumes: Tauri event `agent-event` `{tab_id, event}` and commands `list_agent_events` / `claude_hooks_status` / `install_claude_hooks` (Tasks 5/6).
- Produces (Tasks 8/9 rely on these):
  - `types.ts`: `AgentEventKind`, `AgentEvent`, `AgentEventPayload`, and `OutputHandler`-visible seq (below).
  - `TabManagerState` additions: `agentEvents: Record<string, AgentEvent[]>` (state, for rendering), `eventsForTab: (tabId: string) => AgentEvent[]` (reads a **synchronously-updated ref** — see rationale below), `hooksInstalled: boolean`, `installHooks: () => Promise<void>`.
  - `OutputHandler` becomes `(chunk: Uint8Array, seq: number, isReplay?: boolean) => void`.

**Why both state and a ref:** the `agent-event` Tauri event is emitted *before* the `pty-output` chunk containing the OSC bytes. If anchoring read events from React state, the chunk could be written before the state update re-renders — the anchor would be missed. `eventsForTab` therefore reads a plain `Map` ref that the listener mutates synchronously; the state mirror exists only to re-render the panel. This is the same "callbacks live in a ref" pattern `useTabManager` already uses for output handlers.

- [ ] **Step 1: Extend `src/types.ts`**

```ts
export type AgentEventKind = 'user-prompt' | 'agent-question' | 'agent-done' | 'user-choice';

/** One Claude Code conversation event, anchored to the PTY stream via `seq`. */
export interface AgentEvent {
  id: string;
  kind: AgentEventKind;
  /** Summary text, at most 140 chars; may be empty (e.g. agent-done). */
  text: string;
  /** Unix milliseconds. */
  time: number;
  /** Absolute PTY stream position where the event's OSC started. */
  seq: number;
}

export interface AgentEventPayload {
  tab_id: string;
  event: AgentEvent;
}
```

- [ ] **Step 2: Rework `useTabManager.ts`**

2a. Handler types and import:

```ts
import {
  TAB_GONE,
  type AgentEvent,
  type AgentEventPayload,
  type AttachStreamResponse,
  // ... existing imports unchanged
} from '../types';

/** `isReplay` marks the attach-time ring replay, as opposed to live output.
    `seq` is the absolute stream position of the chunk's first byte. */
type OutputHandler = (chunk: Uint8Array, seq: number, isReplay?: boolean) => void;
```

2b. New state + ref next to the existing `handlers` ref:

```ts
  const [agentEvents, setAgentEvents] = useState<Record<string, AgentEvent[]>>({});
  const [hooksInstalled, setHooksInstalled] = useState(false);
  // Synchronous mirror for terminal anchoring (see the ref rationale in the
  // plan / component comment): React state updates are batched, but the
  // output chunk carrying an event's OSC bytes can arrive in the same tick
  // as the event itself.
  const agentEventsRef = useRef(new Map<string, AgentEvent[]>());

  const pushAgentEvent = useCallback((tabId: string, event: AgentEvent) => {
    const list = agentEventsRef.current.get(tabId);
    if (list) list.push(event);
    else agentEventsRef.current.set(tabId, [event]);
    setAgentEvents((prev) => ({
      ...prev,
      [tabId]: [...(prev[tabId] ?? []), event],
    }));
  }, []);

  const eventsForTab = useCallback(
    (tabId: string): AgentEvent[] => agentEventsRef.current.get(tabId) ?? [],
    []
  );
```

2c. `abandonTab` also drops events (add inside the existing callback):

```ts
    agentEventsRef.current.delete(tabId);
    setAgentEvents((prev) => {
      const { [tabId]: _gone, ...rest } = prev;
      return rest;
    });
```

2d. `attachTab` — thread seq through every path (dedup logic unchanged):

```ts
      const subscription: StreamSink = (chunk, seq) => {
        if (live) handler(chunk, seq);
        else queued.push({ chunk, seq });
      };
```

replay call (ring starts at `replayEnd - bytes.length`):

```ts
        const bytes = base64ToBytes(response.data);
        if (bytes.length > 0) handler(bytes, replayEnd - bytes.length, true);
```

queued flush:

```ts
      for (const { chunk, seq } of queued) {
        if (seq >= replayEnd) {
          handler(chunk, seq);
        } else if (seq + chunk.length > replayEnd) {
          // Straddles the boundary: the replay already showed the head.
          handler(chunk.subarray(replayEnd - seq), replayEnd);
        }
        // Else the replay covered the whole chunk; drop it.
      }
```

2e. Startup — restore events **before** setting tabs, so a mounting Terminal sees them while its replay is written (otherwise replay anchoring would race the restore):

Replace the `invoke<TabResponse[]>('list_tabs').then((loaded) => { ... })` body with:

```ts
    invoke<TabResponse[]>('list_tabs')
      .then(async (loaded) => {
        if (disposed) return;
        // Restore stored conversation events before the tabs (and therefore
        // their Terminal views) exist: replay anchoring reads them
        // synchronously at mount time. TAB_GONE means the tab just closed —
        // an empty list is the right restore.
        const restored = await Promise.all(
          loaded.map((tab) =>
            invoke<AgentEvent[]>('list_agent_events', { tabId: tab.id }).catch(
              () => [] as AgentEvent[]
            )
          )
        );
        if (disposed) return;
        loaded.forEach((tab, i) => {
          if (restored[i].length > 0) agentEventsRef.current.set(tab.id, [...restored[i]]);
        });
        setAgentEvents((prev) => {
          const next = { ...prev };
          loaded.forEach((tab, i) => {
            if (restored[i].length > 0) next[tab.id] = restored[i];
          });
          return next;
        });

        const snapshot = loaded.map(toTab);
        const known = new Set(snapshot.map((tab) => tab.id));
        setTabs((prev) => [
          ...snapshot,
          // A tab created while this snapshot was in flight must not disappear
          // just because the backend computed its answer beforehand.
          ...prev.filter((tab) => !known.has(tab.id)),
        ]);
        setActiveTabId((current) => current ?? snapshot[0]?.id ?? null);
      })
      .catch((err) => report(setError, 'Could not load existing tabs', err));

    invoke<boolean>('claude_hooks_status')
      .then(setHooksInstalled)
      .catch(() => setHooksInstalled(false));
```

(The `known`/`snapshot` logic is the existing code, unchanged — it just moves after the restore.)

2f. New listener in the `listeners.push(...)` batch:

```ts
      listen<AgentEventPayload>('agent-event', ({ payload }) => {
        pushAgentEvent(payload.tab_id, payload.event);
      }),
```

Add `pushAgentEvent` to the effect's dependency list (it is stable — `useCallback` with `[]`).

2g. `installHooks`:

```ts
  const installHooks = useCallback(async () => {
    try {
      await invoke('install_claude_hooks');
      setHooksInstalled(true);
      setError(null);
    } catch (err) {
      report(setError, 'Could not install Claude Code hooks', err);
    }
  }, []);
```

2h. Extend `TabManagerState` and the returned object with: `agentEvents`, `eventsForTab`, `hooksInstalled`, `installHooks`.

- [ ] **Step 3: Adapt `Terminal.tsx` to the new handler signature (no behavior change)**

In the mount effect's attach call, the callback becomes `(chunk, _seq, isReplay)` — the underscore keeps `noUnusedParameters`-style checks quiet; Task 8 puts `_seq` to work. Replay branch passes through unchanged otherwise:

```ts
    void callbacks.current
      .attach(tabId, (chunk, _seq, isReplay) => {
        if (disposed) return;
        if (!isReplay) {
          term.write(chunk);
          return;
        }
        switch (classifyReplay(chunk)) {
          case 'alt-screen':
            return;
          case 'redraw':
            term.write(chunk);
            term.write('\x1b[H\x1b[2J');
            return;
          case 'plain':
            term.write(chunk);
            return;
        }
      });
```

- [ ] **Step 4: Verify**

Run: `npm run typecheck`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/types.ts src/hooks/useTabManager.ts src/components/Terminal.tsx
git commit -m "renderer: track agent events, seq-tag the output handler, restore on reload

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 8: `Terminal.tsx` — seq-anchored markers and jump-to-event

**Files:**
- Modify: `src/components/Terminal.tsx`
- Verify: `npm run typecheck` (behavior verified manually in Task 11)

**Interfaces:**
- Consumes: `eventsForTab` (Task 7), `AgentEvent` (Task 7), xterm public API `registerMarker(y?) -> IMarker | undefined`, `IMarker.line`, `IMarker.isDisposed`, `scrollLines(n)`, `buffer.active.baseY`.
- Produces (Task 9 relies on these): new **optional** props on `Terminal`:
  - `eventsForTab?: (tabId: string) => AgentEvent[]` (default `() => []`)
  - `jumpRequest?: { tabId: string; eventId: string; nonce: number } | null` (default `null`)

  Optional so this task typechecks standalone; Task 9 passes them for real.

- [ ] **Step 1: Add props, refs, and the marker store**

```tsx
import { Terminal as XTerm, type IMarker } from '@xterm/xterm';
import type { AgentEvent } from '../types';

interface TerminalProps {
  tabId: string;
  isActive: boolean;
  attach: (tabId: string, write: (chunk: Uint8Array, seq: number, isReplay?: boolean) => void) => Promise<void>;
  detach: (tabId: string) => void;
  onInput: (tabId: string, data: Uint8Array) => void;
  onResize: (tabId: string, rows: number, cols: number) => void;
  /** Synchronous reader for this tab's agent events (anchoring must not
      depend on React render timing — see useTabManager's ref rationale). */
  eventsForTab?: (tabId: string) => AgentEvent[];
  /** Bumped `nonce` re-triggers a jump to `eventId`; `tabId` guards against
      a stale request firing in another tab's terminal. */
  jumpRequest?: { tabId: string; eventId: string; nonce: number } | null;
}
```

Component-level (next to `termRef`):

```tsx
  // eventId -> xterm marker at the exact buffer line where the event's OSC
  // was written. Markers track scrollback trimming; `isDisposed` means the
  // line is gone and the jump becomes a no-op.
  const markersRef = useRef(new Map<string, IMarker>());
```

Add `eventsForTab` / `jumpRequest` to the `callbacks` ref object (same pattern as the existing four callbacks), with defaults in destructuring:

```tsx
export const Terminal: React.FC<TerminalProps> = ({
  tabId,
  isActive,
  attach,
  detach,
  onInput,
  onResize,
  eventsForTab = () => [],
  jumpRequest = null,
}) => {
  // ...
  const callbacks = useRef({ attach, detach, onInput, onResize, eventsForTab });
  callbacks.current = { attach, detach, onInput, onResize, eventsForTab };
```

(`jumpRequest` is consumed by a component-level effect, not inside the mount effect, so it does not go through `callbacks`.)

- [ ] **Step 2: Anchored writes inside the mount effect**

Replace the Task-7 attach block with:

```tsx
    /** Write `chunk` (absolute stream position `seq`), splitting the write
        at the seq of every agent event that falls inside it, so an xterm
        marker lands on the exact line the event happened at. xterm queues
        writes in order and runs each callback after parsing that segment,
        so `registerMarker(0)` in a callback sees the cursor exactly where
        the OSC (invisible bytes) sat. */
    const writeWithAnchors = (chunk: Uint8Array, seq: number) => {
      const pending = callbacks.current
        .eventsForTab(tabId)
        .filter((e) => !markersRef.current.has(e.id) && e.seq >= seq && e.seq < seq + chunk.length)
        .sort((a, b) => a.seq - b.seq);
      if (pending.length === 0) {
        term.write(chunk);
        return;
      }
      let cursor = seq;
      for (const event of pending) {
        const at = event.seq - seq;
        if (at > cursor - seq) term.write(chunk.subarray(cursor - seq, at));
        // Zero-length write whose callback registers the marker: works even
        // when the event sits at the very start of the chunk.
        term.write(new Uint8Array(0), () => {
          const marker = term.registerMarker(0);
          if (marker) markersRef.current.set(event.id, marker);
        });
        cursor = event.seq;
      }
      term.write(chunk.subarray(cursor - seq));
    };

    // Replay whatever the PTY produced before we were listening, then stream.
    // (classifyReplay comment above stays as-is.)
    void callbacks.current
      .attach(tabId, (chunk, seq, isReplay) => {
        if (disposed) return;
        if (!isReplay) {
          writeWithAnchors(chunk, seq);
          return;
        }
        switch (classifyReplay(chunk)) {
          case 'alt-screen':
            // Dropped entirely; events in this range stay unanchored and
            // their jumps become no-ops. Acceptable: an alt-screen app
            // (vim/less) has no meaningful scrollback position to jump to.
            return;
          case 'redraw':
            writeWithAnchors(chunk, seq);
            // Home + ED2: clear the visible screen, keep the replayed
            // history in the scrollback. Markers already point at scrollback
            // lines, which ED2 does not touch.
            term.write('\x1b[H\x1b[2J');
            return;
          case 'plain':
            writeWithAnchors(chunk, seq);
            return;
        }
      });
```

In the mount effect's cleanup (before `term.dispose()`):

```tsx
    markersRef.current.clear();
```

- [ ] **Step 3: Jump effect (component level, after the isActive effect)**

```tsx
  // Scroll to an event's marker when the panel requests a jump. A missing or
  // disposed marker (line trimmed out of scrollback, event older than the
  // replay ring after a reload, alt-screen replay dropped) is a silent no-op.
  useEffect(() => {
    if (!jumpRequest || jumpRequest.tabId !== tabId) return;
    const term = termRef.current;
    const marker = markersRef.current.get(jumpRequest.eventId);
    if (!term || !marker || marker.isDisposed) {
      console.debug('jump target unavailable:', jumpRequest.eventId);
      return;
    }
    term.scrollLines(marker.line - term.buffer.active.baseY);
    term.focus();
  }, [jumpRequest, tabId]);
```

- [ ] **Step 4: Verify**

Run: `npm run typecheck`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/components/Terminal.tsx
git commit -m "terminal: anchor agent events to buffer markers, scroll to them on jump

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 9: Panel UI — `AgentEventPanel.tsx`, `App.tsx` wiring, styles

**Files:**
- Create: `src/components/AgentEventPanel.tsx`
- Modify: `src/App.tsx`
- Modify: `src/App.css` (append)
- Verify: `npm run typecheck`

**Interfaces:**
- Consumes: `agentEvents`, `eventsForTab`, `hooksInstalled`, `installHooks` (Task 7); `Terminal`'s `eventsForTab` / `jumpRequest` props (Task 8).
- Produces: user-visible panel; no later task depends on it.

- [ ] **Step 1: Create `AgentEventPanel.tsx`**

```tsx
import React, { useEffect, useRef } from 'react';
import type { AgentEvent, AgentEventKind } from '../types';

const KIND_LABEL: Record<AgentEventKind, string> = {
  'user-prompt': 'You',
  'agent-question': 'Claude asks',
  'agent-done': 'Claude done',
  'user-choice': 'You chose',
};

interface AgentEventPanelProps {
  events: AgentEvent[];
  hooksInstalled: boolean;
  onJump: (eventId: string) => void;
  onInstall: () => void;
  onClose: () => void;
}

/** Right-side conversation timeline for the active tab. Every row is a
    button that scrolls the terminal to the event's anchored position. */
export const AgentEventPanel: React.FC<AgentEventPanelProps> = ({
  events,
  hooksInstalled,
  onJump,
  onInstall,
  onClose,
}) => {
  const listRef = useRef<HTMLDivElement>(null);

  // Keep the newest event in view as the conversation streams in.
  useEffect(() => {
    const el = listRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [events.length]);

  const formatTime = (ms: number) =>
    new Date(ms).toLocaleTimeString([], {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      hour12: false,
    });

  return (
    <aside className="agent-panel">
      <div className="agent-panel-header">
        <span>Conversation</span>
        <button onClick={onClose} aria-label="Close panel">
          ×
        </button>
      </div>
      <div className="agent-panel-list" ref={listRef}>
        {events.length === 0 && (
          <div className="agent-panel-empty">
            {hooksInstalled ? (
              <p>No events yet.</p>
            ) : (
              <>
                <p>No events yet. Install the Claude Code hooks to capture the conversation.</p>
                <button onClick={onInstall}>Install hooks</button>
              </>
            )}
          </div>
        )}
        {events.map((event) => (
          <button
            key={event.id}
            className={`agent-event kind-${event.kind}`}
            onClick={() => onJump(event.id)}
          >
            <span className="agent-event-label">{KIND_LABEL[event.kind]}</span>
            <span className="agent-event-time">{formatTime(event.time)}</span>
            <span className="agent-event-text">{event.text || '—'}</span>
          </button>
        ))}
      </div>
    </aside>
  );
};
```

- [ ] **Step 2: Wire `App.tsx`**

Imports and hook destructuring:

```tsx
import { useCallback, useEffect, useState } from 'react';
import { AgentEventPanel } from './components/AgentEventPanel';
// ...
  const {
    tabs,
    activeTabId,
    error,
    dismissError,
    createTab,
    closeTab,
    switchTab,
    attachTab,
    detachTab,
    writeInput,
    resizePty,
    agentEvents,
    eventsForTab,
    hooksInstalled,
    installHooks,
  } = useTabManager();
```

Panel visibility + jump state (after `activateTab`):

```tsx
  const activeTab = tabs.find((tab) => tab.id === activeTabId) ?? null;
  // null = follow the agent: the panel opens while a program (Claude Code)
  // owns the title and collapses when it reverts. true/false = the user's
  // manual choice, which holds until the next tab switch.
  const [panelOverride, setPanelOverride] = useState<boolean | null>(null);
  useEffect(() => setPanelOverride(null), [activeTabId]);
  const panelOpen = activeTab !== null && (panelOverride ?? activeTab.hasClaudeTitle);

  const [jumpRequest, setJumpRequest] = useState<{
    tabId: string;
    eventId: string;
    nonce: number;
  } | null>(null);
  const jumpToEvent = useCallback(
    (eventId: string) => {
      if (!activeTabId) return;
      // The panel only ever shows the active tab's events.
      setJumpRequest({ tabId: activeTabId, eventId, nonce: Date.now() });
    },
    [activeTabId]
  );
```

JSX — inside `.terminal-area`, after the `tabs.map(...)` block, the reopen toggle:

```tsx
        {activeTab && !panelOpen && (
          <button
            className="agent-panel-toggle"
            onClick={() => setPanelOverride(true)}
            title="Show agent events"
            aria-label="Show agent events"
          >
            ☰
          </button>
        )}
```

`.terminal-area` is `position: relative`, so the toggle floats at its top-right corner (CSS below). After the closing `</div>` of `.terminal-area`, the panel itself:

```tsx
      {activeTab && panelOpen && (
        <AgentEventPanel
          events={agentEvents[activeTab.id] ?? []}
          hooksInstalled={hooksInstalled}
          onJump={jumpToEvent}
          onInstall={installHooks}
          onClose={() => setPanelOverride(false)}
        />
      )}
```

`Terminal` gains the two Task-8 props (jump requests go to the active tab's terminal only):

```tsx
              <Terminal
                tabId={tab.id}
                isActive={tab.id === activeTabId}
                attach={attachTab}
                detach={detachTab}
                onInput={writeInput}
                onResize={resizePty}
                eventsForTab={eventsForTab}
                jumpRequest={jumpRequest}
              />
```

(Every terminal receives `jumpRequest`; each ignores it unless `jumpRequest.tabId` matches — that guard is Task 8's jump effect.)

- [ ] **Step 3: Append styles to `App.css`**

Palette matches the existing Catppuccin-mocha usage (`#181825` chrome, `#313244` borders, `#6c7086` muted, accent colors per event kind):

```css
/* Agent Event Panel */
.agent-panel {
  width: 280px;
  min-width: 280px;
  background: #181825;
  border-left: 1px solid #313244;
  display: flex;
  flex-direction: column;
}

.agent-panel-header {
  padding: 12px 16px;
  font-size: 12px;
  font-weight: 600;
  text-transform: uppercase;
  color: #6c7086;
  border-bottom: 1px solid #313244;
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.agent-panel-header button {
  background: none;
  border: none;
  color: #6c7086;
  cursor: pointer;
  font-size: 14px;
  line-height: 1;
}

.agent-panel-header button:hover {
  color: #cdd6f4;
}

.agent-panel-list {
  flex: 1;
  overflow-y: auto;
  padding: 8px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.agent-panel-empty {
  padding: 16px;
  color: #6c7086;
  font-size: 13px;
  text-align: center;
}

.agent-panel-empty button {
  margin-top: 8px;
  background: #313244;
  color: #cdd6f4;
  border: none;
  border-radius: 6px;
  padding: 6px 12px;
  cursor: pointer;
  font-size: 13px;
}

.agent-panel-empty button:hover {
  background: #45475a;
}

.agent-event {
  display: grid;
  grid-template-columns: 1fr auto;
  gap: 2px 8px;
  text-align: left;
  background: none;
  border: 1px solid transparent;
  border-radius: 6px;
  padding: 6px 8px;
  cursor: pointer;
  color: #cdd6f4;
  font: inherit;
}

.agent-event:hover {
  background: #1e1e2e;
  border-color: #313244;
}

.agent-event-label {
  font-size: 11px;
  font-weight: 600;
}

.agent-event-time {
  font-size: 11px;
  color: #6c7086;
}

.agent-event-text {
  grid-column: 1 / -1;
  font-size: 12px;
  color: #a6adc8;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.kind-user-prompt .agent-event-label { color: #89b4fa; }
.kind-agent-question .agent-event-label { color: #f9e2af; }
.kind-agent-done .agent-event-label { color: #a6e3a1; }
.kind-user-choice .agent-event-label { color: #cba6f7; }

.agent-panel-toggle {
  position: absolute;
  top: 8px;
  right: 8px;
  z-index: 5;
  background: #313244;
  color: #cdd6f4;
  border: none;
  border-radius: 6px;
  padding: 4px 8px;
  cursor: pointer;
  opacity: 0.6;
}

.agent-panel-toggle:hover {
  opacity: 1;
}
```

- [ ] **Step 4: Verify**

Run: `npm run typecheck`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/components/AgentEventPanel.tsx src/App.tsx src/App.css
git commit -m "ui: agent event panel with auto-open, jump-to-position, hook install

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 10: Documentation — READMEs + CLAUDE_HOOKS.md

**Files:**
- Modify: `README.md`, `README.zh-CN.md` (kept in sync with each other — repo rule)
- Rewrite: `CLAUDE_HOOKS.md`
- Verify: prose only; both READMEs say the same things.

**Interfaces:**
- Consumes: final behavior from Tasks 1–9.
- Produces: user-facing docs; no code depends on it.

- [ ] **Step 1: README.md — add a section**

Place after the existing attention-flash / Claude Code integration material (find the section that references `CLAUDE_HOOKS.md` and put this next to it):

```markdown
### Agent event panel

When a tab runs Claude Code, a right-side panel lists the conversation as it
happens — your prompts, Claude's questions, completed turns, and choice
submissions — each with a timestamp. Clicking an entry scrolls the terminal
back to where that event happened.

The panel needs a one-time Claude Code hook setup: click **Install hooks** in
the panel. clitab writes a small hook script into its app-data directory and
merges four hook registrations into `~/.claude/settings.json` — your own hooks
are preserved, a `settings.json.bak` backup is created on first install, and
reinstalling is safe (clitab replaces only its own entries). Events are kept
in memory per tab (most recent 200) and are not persisted to disk; jumps work
as long as the position is still in the terminal's scrollback. See
[CLAUDE_HOOKS.md](CLAUDE_HOOKS.md) for details and manual setup.
```

- [ ] **Step 2: README.zh-CN.md — the mirrored section**

Same placement as Step 1:

```markdown
### Agent 事件面板

当某个标签页运行 Claude Code 时，右侧面板会实时列出对话事件——你的发言、Claude 的提问、
回合完成、选择提交——每条带时间戳。点击条目可将终端滚动回事件发生的位置。

面板需要一次性安装 Claude Code hooks：点击面板中的 **Install hooks**。clitab 会把一个
小 hook 脚本写入应用数据目录，并把四条 hook 注册合并进 `~/.claude/settings.json`——
你自己的 hooks 会原样保留，首次安装会创建 `settings.json.bak` 备份，重复安装是安全的
（clitab 只替换自己的条目）。事件按标签页保存在内存中（最近 200 条），不落盘；只要对应
位置还在终端回滚缓冲区里，跳转就有效。详见 [CLAUDE_HOOKS.md](CLAUDE_HOOKS.md)。
```

- [ ] **Step 3: Rewrite `CLAUDE_HOOKS.md`**

Replace the whole file with:

````markdown
# Claude Code Integration

clitab's agent event panel (and the attention flash) are driven by Claude Code
hooks that write escape sequences into the terminal stream, where clitab's PTY
parser picks them up.

## One-click install (recommended)

Open the agent event panel in any tab and click **Install hooks**. clitab:

1. writes its hook script to
   `~/Library/Application Support/com.clitab.app/hooks/clitab-hook.sh`;
2. merges four registrations into `~/.claude/settings.json`:

   | Hook event | Matcher | Panel event |
   |---|---|---|
   | `UserPromptSubmit` | — | your prompt |
   | `Notification` | `.*` | Claude asks / waits |
   | `Stop` | — | turn completed |
   | `PostToolUse` | `AskUserQuestion` | your choice |

Your own hook entries are preserved. The first install writes a
`settings.json.bak` backup next to the original. Reinstalling replaces only
clitab-owned entries (recognized by the `clitab-hook.sh` filename in their
command), so it is safe after an app update. Restart any running Claude Code
sessions to pick up the new settings.

## How it works

- The hook script reads the hook's stdin JSON (truncated to 2800 bytes),
  base64-encodes it, and writes `OSC 9;clitab-agent;<base64>` to the terminal.
- Because the sequence rides the PTY byte stream, clitab knows the event's
  exact stream position — that is what makes click-to-jump land on the right
  terminal line.
- `Notification` events additionally flash the tab; `Stop` events additionally
  revert the tab title to the working directory (same effects the legacy
  `claude-done` sequence had).
- Everything is best-effort and silent on failure: a hook must never block or
  break Claude Code.

## Manual setup (alternative)

Point the four hook events at the script yourself, e.g. in
`~/.claude/settings.json`:

```json
{
  "hooks": {
    "Stop": [
      { "hooks": [{ "type": "command",
        "command": "$HOME/Library/Application Support/com.clitab.app/hooks/clitab-hook.sh" }] }
    ]
  }
}
```

(Repeat for `UserPromptSubmit`, `Notification` with matcher `.*`, and
`PostToolUse` with matcher `AskUserQuestion`.)

## Legacy: flash-only setup

Older docs suggested a bare BEL hook for the attention flash alone:

```json
{
  "hooks": {
    "Notification": [
      { "matcher": ".*",
        "hooks": [{ "type": "command", "command": "printf '\\a'" }] }
    ]
  }
}
```

This still works (clitab flashes on BEL), but it produces no panel events.
Use the one-click install instead; it supersedes this setup.
````

- [ ] **Step 4: Verify the sync**

Re-read both README sections side by side: same facts, same order, same link. Confirm `CLAUDE_HOOKS.md` has no leftover claim that conflicts with the spec (e.g. the old "Stop may not support command hooks" note is gone — Task 1's probe proved otherwise; if the probe showed Stop does NOT fire, restore that caveat and drop Stop from the table in all three files).

- [ ] **Step 5: Commit**

```bash
git add README.md README.zh-CN.md CLAUDE_HOOKS.md
git commit -m "docs: agent event panel + one-click hook install (READMEs, CLAUDE_HOOKS)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 11: End-to-end verification (manual acceptance)

**Files:** none (verification only; fix-forward commits if something fails).

**Interfaces:**
- Consumes: everything from Tasks 1–10.
- Produces: the acceptance record; the branch is ready for review/merge.

- [ ] **Step 1: Automated gates**

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib
npm run typecheck
npm run build
```

Expected: all green.

- [ ] **Step 2: Launch and install hooks**

```bash
npm run tauri dev
```

In the app: open a tab, confirm the `☰` toggle appears at the terminal's top-right; open the panel; click **Install hooks**; confirm `~/.claude/settings.json` now contains the four entries and `settings.json.bak` exists (first install only). Re-click install (via toggle) and confirm no duplicate groups.

- [ ] **Step 3: Live conversation acceptance**

In the tab, run `claude` in a scratch directory. Check each behavior:

- Panel auto-opens when Claude Code takes the title.
- Send a prompt → a **You** row appears with your prompt text and a sane time.
- Let a turn finish → a **Claude done** row appears; tab title reverts to the cwd (prompt-ready behavior); the tab flashes if not focused.
- Trigger a permission prompt or idle notification → a **Claude asks** row appears; tab flashes when unfocused.
- Ask Claude to use AskUserQuestion (`请用 AskUserQuestion 工具问我一个单选题`) and answer → a **You chose** row appears naming your selection.
- Scroll away, then click an early row → the terminal jumps back to roughly where that event happened (within a few lines: the anchor is the OSC's position, and ink repaints around it).
- Send ~50 more messages, scroll to top: rows whose lines were trimmed (scrollback is 5000) do nothing on click — no crash, terminal keeps working.

- [ ] **Step 4: Reload and multi-tab acceptance**

- With a conversation in the panel, reload the webview (dev: cmd-R in the webview / restart frontend): panel rows are restored; clicking a row whose position is still inside the 256 KB replay ring jumps correctly; older rows no-op silently.
- Open a second tab without Claude Code: no panel (toggle still available; its panel shows the empty state). Switch between tabs: each panel shows its own tab's events; the `☰`/auto-open rule follows the active tab.
- Close the Claude tab: its events are gone from state (reopen a tab, no stale rows).

- [ ] **Step 5: Legacy coexistence check**

In a terminal *outside* clitab, add the legacy `printf '\a'` Notification hook alongside clitab's entries (or reuse an existing legacy config): confirm BEL still flashes the tab and nothing double-fires badly (a duplicate flash is acceptable per spec; a duplicate panel row is not — the legacy hook writes no `clitab-agent` OSC, so no row must appear from it). Remove the legacy entry afterwards.

- [ ] **Step 6: Record results and finish**

Note pass/fail per checklist item in the task report. Any failure: fix in a focused commit (`fix: ...` + attribution line) referencing the behavior, re-run the affected step. When green, the branch is ready — follow superpowers:finishing-a-development-branch for merge/PR.

---

## Self-Review Notes (author)

- **Spec coverage:** every spec section maps to a task — data flow (2–5, 7–8), event model (3), hook script + install (6), attention-behavior preservation (5), panel UI + auto-open (9), reload restore (7 step 2e), scrollback-trim no-op (8 step 3), docs (10), acceptance (11). No gaps found.
- **Placeholder scan:** all code steps carry full code; the only deliberately deferred values are Task 1's probe findings, which Task 3 and Task 6 Step 1 explicitly instruct to fold in.
- **Type consistency:** `AgentEvent`/`AgentEventKind` names and fields identical across Tasks 3/4/5/7; `OutputHandler` seq signature consistent across 7/8; `jumpRequest {tabId, eventId, nonce}` consistent across 8/9; command names `list_agent_events`/`claude_hooks_status`/`install_claude_hooks` consistent across 6/7.
- **Review Focus pins:** item 1 → Task 1 Step 3 (stop-the-line); item 2 → Task 3 `truncated_mid_multibyte_is_salvaged`; item 3 → Task 2 `agent_event_requires_prefix` + Task 3 `garbage_is_dropped`; item 4 → Task 6 `merge_*` tests; item 5 → Task 8 Step 3 guard + Task 11 Steps 3–4.
