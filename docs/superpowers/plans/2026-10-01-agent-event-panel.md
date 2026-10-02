# Agent Event Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a right-side panel listing Claude Code conversation events (user prompt, agent question, agent done, user choice) with text + timestamps; clicking an entry scrolls the terminal to the position where the event happened.

**Architecture:** Claude Code hooks already speak the **OSC 7777 protocol** to their tab (the shipped dashboard: turn state, tool timer, notifications — see `status.rs` and `CLAUDE_HOOKS.md`). This plan extends the same wire with conversation text: `{"e":"prompt","text":…}` and `{"e":"notify","msg":…}` carry user-visible text, `{"e":"stop"}` marks turn end, and a new `{"e":"choice","text":…}` (PostToolUse, matcher `AskUserQuestion`) reports selections. Two things are added on the Rust side: the OSC parser reports each decoded event's **absolute byte-stream offset**, and `handle_status` maps decoded events into per-tab **`AgentEvent`** records — stored in the Registry, emitted as an `agent-event` Tauri event carrying `seq`. The renderer writes output chunks tagged with `seq`; when a write reaches an event's `seq` it registers an xterm marker at that exact buffer line. Clicking a panel row scrolls to the marker.

**Tech Stack:** Tauri 2 / Rust (portable-pty; existing `uuid`, `serde_json` crates — **no new crates**), React 18 + xterm.js (`registerMarker` / `scrollLines` are existing public API — **no new npm deps**).

**Spec:** `docs/superpowers/specs/2026-10-01-agent-event-panel-design.md` (revision 2)

**Execution note (user requirement):** implementation runs in a git worktree — create it via superpowers:using-git-worktrees before Task 1.

**Revision 2 (2026-10-02):** rebased onto the shipped OSC 7777 dashboard work and the probe verdict of commit efd762d (hooks have no controlling terminal and their stdout is captured — only the ancestor-tty transport works). Dropped from revision 1: the OSC 9 `clitab-agent` channel, the base64 hook script, the salvage decoder, and all "legacy attention effects" (the shipped `handle_status` already owns flash/queue behavior; the `stop`-never-touches-the-title invariant stands). The installer now writes the same inline commands `CLAUDE_HOOKS.md` documents and upgrades existing manual installs in place.

## Global Constraints

- No new Rust crates, no new npm dependencies.
- OSC 7777 is extended, never forked: wire payloads stay valid JSON; `text` is optional (old builds ignore unknown fields — the serde forward-compat already documented in `status.rs`). No new OSC codes.
- `MAX_OSC_LEN` (4096) in `osc.rs` stays unchanged; hook commands slice text at the wire with jq (`.[0:140]`), and Rust truncates to `TEXT_LIMIT = 140` chars (char-safe, CJK) as the authoritative cap.
- Hook commands must use the ancestor-tty transport (probe verdict efd762d): `t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' ')`, write `> /dev/$t` guarded by `[ -n "$t" ] && [ "$t" != '??' ]`, end with `; true`. `/dev/tty` and stdout do NOT reach the PTY — do not "simplify" them back in.
- Every hook degrades silently without jq (bare payload or no payload), matching the shipped dashboard's degradation philosophy.
- Per-tab event list capped at 200 (oldest evicted); duplicate guard: same kind + same text within 2000 ms is rejected (hooks fire once per settings level — the dashboard's duplicate-Stop guard proves this happens).
- `stop`/panel handling must NOT touch `program_active` or the title: the shipped invariant in `session.rs` says title revert stays owned by the shell integration's `claude-done`.
- The re-attach dedup invariant is untouchable: `pty-output` chunks are emitted **while holding the stream lock**, each carrying its absolute `seq`. `agent-event` is emitted from `handle_osc` **before** the stream lock is taken, i.e. before the `pty-output` chunk containing its OSC bytes — the renderer must therefore anchor from a synchronous ref, not React state.
- Tauri event payload keys stay snake_case (`tab_id`); command responses are camelCase via serde. `AgentEvent` fields (`id`/`kind`/`text`/`time`/`seq`) are single words — identical in any casing convention; `kind` serializes kebab-case.
- Checks: Rust = `cargo test --manifest-path src-tauri/Cargo.toml --lib`; frontend = `npm run typecheck` (no test runner exists; do not add one).
- Every commit message ends with the attribution line:
  `Co-Authored-By: Claude Code <noreply@anthropic.com>`

## Review Focus

The failure modes no single task's tests fully exercise, most likely first. Each is pinned to the owning task below.

1. **`PostToolUse` with matcher `AskUserQuestion` never fires, or `tool_response` has an unexpected shape** → the `user-choice` kind silently missing while others work. Pinned by Task 1 (probe before any product code; if the hook never fires, STOP and report — `user-choice` is dropped from v1 and Tasks 3/6/9/10 shed it).
2. **Duplicate hook fires** (same hook registered at user + project settings level) → doubled panel rows that also diverge from the restored-after-reload list. Pinned by Task 4's dedup test and Task 5's emit-only-when-accepted rule.
3. **Existing manual OSC 7777 installs must be upgraded, not duplicated** (the current `CLAUDE_HOOKS.md` commands are in real users' settings.json — including this repo's author). Pinned by Task 6 test `merge_replaces_existing_manual_osc_entries`, whose fixture is the exact shipped command strings.
4. **Forged / garbage / oversized OSC 7777 payloads** from any program `cat`-ing binary noise → dropped without panic (existing `status::decode` tolerance + parser cap); long prompt text → truncated at 140 chars, never mid-codepoint. Pinned by Task 3 tests `text_truncated_to_limit_chars` + existing `status.rs` malformed-input tests.
5. **Jump target no longer reachable** (marker trimmed out of the 5000-line scrollback, event seq older than the 256 KB replay ring after a webview reload, alt-screen replay dropped) → silent no-op, terminal keeps working. Pinned by Task 8's `isDisposed`/missing-marker guard + Task 11 manual acceptance items.

---

### Task 1: Probe — verify the AskUserQuestion PostToolUse payload shape

Throwaway verification. **No product code.** The transport question is already verdict'd (commit efd762d; the shipped dashboard hooks prove OSC 7777 reaches the PTY via the ancestor tty in production). What remains unknown: whether `PostToolUse` with matcher `AskUserQuestion` fires at all, and where the user's selection lives in its stdin JSON. Task 6's `choice` jq expression is written against this shape.

**Files:**
- Create (throwaway, outside the repo): `/tmp/clitab-probe/.claude/settings.json`
- Output artifacts: `/tmp/clitab-probe/*.json`

**Interfaces:**
- Consumes: nothing.
- Produces: verified facts — (a) `PostToolUse`+`AskUserQuestion` fires (or the stop-the-line verdict); (b) the exact `tool_response` structure and a working jq expression extracting the user's selection; (c) confirmation that `UserPromptSubmit` stdin carries the prompt in `.prompt` as a plain string.

- [ ] **Step 1: Write the probe hook config**

```bash
mkdir -p /tmp/clitab-probe/.claude
cat > /tmp/clitab-probe/.claude/settings.json <<'EOF'
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command",
      "command": "cat > /tmp/clitab-probe/prompt.json" }] }],
    "PostToolUse": [{ "matcher": "AskUserQuestion", "hooks": [{ "type": "command",
      "command": "cat > /tmp/clitab-probe/choice.json" }] }]
  }
}
EOF
```

Each hook dumps its stdin JSON to a file. No tty writes needed — we are probing payload shapes, not transport. (A project-level `.claude/settings.json` merges with the user's global hooks; if your global `~/.claude/settings.json` has OSC 7777 hooks, they fire too — harmless for this probe.)

- [ ] **Step 2: Run Claude Code in the probe directory**

```bash
cd /tmp/clitab-probe && claude
```

Inside the session: (1) send any short prompt and let the turn finish; (2) send `请用 AskUserQuestion 工具问我一个单选题` and answer the choice; (3) exit Claude.

- [ ] **Step 3: Inspect the payload shapes**

```bash
for f in /tmp/clitab-probe/*.json; do echo "== $f"; head -c 800 "$f"; echo; done
```

Record: the key holding the prompt text (expected: `.prompt`, a plain string), and the structure of `tool_input` / `tool_response` for `AskUserQuestion` (where the selected option label(s) live).

- [ ] **Step 4: Pin the choice jq expression**

Test the candidate expression against the captured payload:

```bash
jq -c '{e:"choice",text:((.tool_response.answers // .tool_response // "")|tostring|.[0:140])}' < /tmp/clitab-probe/choice.json
```

If `text` is not a readable summary of the user's selection, adjust the expression (e.g. index into the real `tool_response` shape) and re-test until it is. **Paste the final expression into the task report** — Task 6 embeds it verbatim.

- [ ] **Step 5: Stop-the-line check**

If `choice.json` was never created, the `PostToolUse`/`AskUserQuestion` hook does not fire: **STOP and report**. The fallback scope is dropping the `user-choice` kind from v1 — Tasks 3/6/9/10 then omit the `Choice` variant, the `PostToolUse` registration, the `You chose` label, and the docs row respectively.

- [ ] **Step 6: Clean up and report**

```bash
rm -rf /tmp/clitab-probe
```

No commit (nothing in the repo changed). Report findings before starting Task 2.

---

### Task 2: `osc.rs` — absolute stream offsets

**Files:**
- Modify: `src-tauri/src/osc.rs`
- Modify: `src-tauri/src/pty/session.rs` (call site only — keep it compiling; real wiring is Task 5)
- Test: inline `mod tests` in `src-tauri/src/osc.rs`

**Interfaces:**
- Consumes: nothing (leaf module).
- Produces: `OscParser::parse(&mut self, data: &[u8]) -> Vec<(OscEvent, u64)>` where the `u64` is the absolute stream position of the sequence's leading `ESC` (for a standalone BEL, the BEL's own position). **No enum changes** — OSC 7777 payloads keep arriving as `OscEvent::Clitab(String)`. Tasks 3/5 rely on the offset.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `osc.rs`:

```rust
    #[test]
    fn offsets_count_every_fed_byte() {
        let mut parser = OscParser::new();
        assert!(ev(&mut parser, b"12345678").is_empty());
        // Second feed: the BEL sits at absolute position 8.
        let parsed = parser.parse(b"\x07");
        assert_eq!(parsed, vec![(OscEvent::Bell, 8)]);
    }

    #[test]
    fn osc_offset_is_the_leading_esc() {
        let mut parser = OscParser::new();
        let parsed = parser.parse(b"ab\x1b]7777;{\"e\":\"stop\"}\x1b\\tail");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].1, 2);
        assert!(matches!(parsed[0].0, OscEvent::Clitab(_)));
    }

    #[test]
    fn osc_offset_survives_split_across_reads() {
        let mut parser = OscParser::new();
        assert!(parser.parse(b"abcd\x1b]7777;{\"e\":").is_empty());
        let parsed = parser.parse(b"\"prompt\"}\x07");
        assert_eq!(
            parsed,
            vec![(OscEvent::Clitab("{\"e\":\"prompt\"}".into()), 4)]
        );
    }

    #[test]
    fn nested_osc_offset_points_at_inner_esc() {
        let mut parser = OscParser::new();
        // ESC ] a b c ESC ] 0 ; T BEL — the garbage outer sequence restarts at
        // the second ESC (position 5); that is the inner sequence's start.
        let parsed = parser.parse(b"\x1b]abc\x1b]0;T\x07");
        assert_eq!(parsed, vec![(OscEvent::TitleChanged("T".into()), 5)]);
    }
```

(`ev` is the helper Step 4 introduces for the pre-existing tests; add it first or write these tests expecting it — either order fails correctly at Step 2.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib osc`
Expected: FAIL — `parse` returns `Vec<OscEvent>` (tuple assertions don't typecheck) and `ev` doesn't exist.

- [ ] **Step 3: Implement the offset bookkeeping**

In `osc.rs` — three new fields on the existing struct (the `payload_len` cap machinery stays exactly as it is):

```rust
#[derive(Debug, Default)]
pub struct OscParser {
    state: State,
    buffer: Vec<u8>,
    params: Vec<Vec<u8>>,
    /// Bytes accumulated into `params` after the code parameter, including
    /// the `;` separators between them — i.e. the length the rejoined
    /// payload will have. (existing field, existing doc comment — unchanged)
    payload_len: usize,
    /// Total bytes ever fed to `parse()`. One parser instance per session,
    /// fed every byte from stream position 0, so this equals the absolute
    /// seq — the same numbering `StreamState.position` uses.
    fed: u64,
    /// Absolute position of the ESC that began the OSC being accumulated.
    osc_start: u64,
    /// Absolute position of the most recently seen ESC (an OSC may begin at
    /// an ESC that first looked like something else, or re-begin after a
    /// nested-ESC resync).
    esc_pos: u64,
}
```

`parse()` gains position bookkeeping. The state machine logic is unchanged except where noted; in particular the `InOsc` accumulator arm keeps the existing two-part cap check (`buffer.len() >= MAX_OSC_LEN || payload_len + buffer.len() > MAX_OSC_LEN`) and the `0x1b` arm keeps calling `flush_param()` (not `push_param`):

```rust
    /// Feed a chunk of PTY output, returning every OSC event it completed,
    /// each tagged with the absolute stream position of its leading ESC
    /// (a standalone BEL: the BEL's own position).
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
                        // CSI / DCS / anything else: we only care about OSC.
                        self.state = State::Ground;
                    }
                }
                State::InOsc => match byte {
                    0x07 => events.extend(self.finish_osc()),
                    b';' => self.push_param(),
                    0x1b => {
                        // Either the start of an `ESC \` (ST) terminator, or a
                        // nested OSC from truncated / binary output.
                        self.esc_pos = pos;
                        self.flush_param();
                        self.state = State::InOscAfterEsc;
                    }
                    _ => {
                        if self.buffer.len() >= MAX_OSC_LEN
                            || self.payload_len + self.buffer.len() > MAX_OSC_LEN
                        {
                            // Runaway sequence: drop it and resynchronise.
                            self.abort_osc();
                        } else {
                            self.buffer.push(byte);
                        }
                    }
                },
                State::InOscAfterEsc => {
                    if byte == b'\\' {
                        // ST terminator.
                        events.extend(self.finish_osc());
                    } else if byte == b']' {
                        // A new OSC started before the previous one was
                        // terminated: the partial sequence is garbage, so throw
                        // away everything accumulated so far and start clean.
                        self.begin_osc(self.esc_pos);
                    } else {
                        // Not a terminator after all: keep the payload going.
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

`begin_osc` takes the start position; `finish_osc` tags with it:

```rust
    fn begin_osc(&mut self, start: u64) {
        self.buffer.clear();
        self.params.clear();
        self.payload_len = 0;
        self.osc_start = start;
        self.state = State::InOsc;
    }

    /// Close the current OSC sequence and interpret it, tagged with the
    /// absolute position of its leading ESC.
    fn finish_osc(&mut self) -> Vec<(OscEvent, u64)> {
        self.flush_param();
        let event = Self::interpret(&self.params);
        let start = self.osc_start;
        self.reset();
        event.map(|e| vec![(e, start)]).unwrap_or_default()
    }
```

`reset()` also zeroes `osc_start` (cosmetic; `begin_osc` always sets it before use). `push_param`, `flush_param`, `abort_osc`, `interpret` are unchanged.

- [ ] **Step 4: Update the existing tests mechanically**

Every pre-existing test asserts on `Vec<OscEvent>`; add a helper at the top of `mod tests` and route the old assertions through it (assertions themselves unchanged):

```rust
    /// Unwrap the offsets: legacy tests only care which events were decoded.
    fn ev(parser: &mut OscParser, data: &[u8]) -> Vec<OscEvent> {
        parser.parse(data).into_iter().map(|(e, _)| e).collect()
    }
```

Replace `parser.parse(X)` with `ev(&mut parser, X)` in every pre-existing test (also `assert!(parser.parse(X).is_empty())` → `assert!(ev(&mut parser, X).is_empty())`). The `runaway_osc_is_capped` / `semicolon_run_osc_is_capped` tests also touch `parser.buffer` / `parser.params` — unchanged.

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
                            &flashed,
                            event,
                        );
                    }
```

`handle_osc` itself is untouched in this task (Task 5 threads the offset through).

- [ ] **Step 6: Run the full Rust suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: PASS (all osc tests, new and updated).

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/osc.rs src-tauri/src/pty/session.rs
git commit -m "osc: report absolute stream offsets with decoded events

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 3: `status.rs` text fields + new `agent_events.rs` mapping

**Files:**
- Modify: `src-tauri/src/status.rs`
- Create: `src-tauri/src/agent_events.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod agent_events;`)
- Modify: `src-tauri/src/pty/session.rs` (match-arm compile fixes only)
- Test: inline `mod tests` in both modules

**Interfaces:**
- Consumes: nothing new (pure decode/mapping).
- Produces (Tasks 5/6 rely on these exact names):
  - `StatusEvent::Prompt { text: Option<String> }` (variant reshaped) and `StatusEvent::Choice { text: Option<String> }` (new), decoded from `{"e":"prompt","text":…}` / `{"e":"choice","text":…}`.
  - `agent_events::AgentEventKind { UserPrompt, AgentQuestion, AgentDone, UserChoice }` — serde kebab-case (`"user-prompt"` etc.), derives `Debug, Clone, Copy, PartialEq, Eq, Serialize`.
  - `agent_events::AgentEvent { pub id: String, pub kind: AgentEventKind, pub text: String, pub time: u64, pub seq: u64 }` — derives `Debug, Clone, PartialEq, Serialize`.
  - `agent_events::panel_event(&StatusEvent) -> Option<(AgentEventKind, String)>`
  - `agent_events::TEXT_LIMIT: usize = 140`

- [ ] **Step 1: Update + extend the `status.rs` tests (failing)**

In `mod tests` of `status.rs`:

Mechanical updates to existing tests — `decodes_each_event`:

```rust
        assert_eq!(decode(r#"{"e":"prompt"}"#), Some(StatusEvent::Prompt { text: None }));
```

(the `tool` / `stop` / `notify` assertions in that test are unchanged).

New tests:

```rust
    #[test]
    fn prompt_carries_optional_text() {
        assert_eq!(
            decode(r#"{"e":"prompt","text":"fix the build"}"#),
            Some(StatusEvent::Prompt { text: Some("fix the build".into()) })
        );
        // An empty text is treated like a missing one (same rule as tool names).
        assert_eq!(
            decode(r#"{"e":"prompt","text":""}"#),
            Some(StatusEvent::Prompt { text: None })
        );
    }

    #[test]
    fn choice_decodes() {
        assert_eq!(
            decode(r#"{"e":"choice","text":"Option B"}"#),
            Some(StatusEvent::Choice { text: Some("Option B".into()) })
        );
        assert_eq!(decode(r#"{"e":"choice"}"#), Some(StatusEvent::Choice { text: None }));
        assert_eq!(decode(r#"{"e":"choice","text":""}"#), Some(StatusEvent::Choice { text: None }));
    }

    /// The hooks slice text with jq, but a manual payload may carry anything:
    /// semicolons and JSON escapes must survive the parser rejoin + decode.
    #[test]
    fn text_with_semicolons_and_escapes_survives() {
        assert_eq!(
            decode(r#"{"e":"prompt","text":"a;b\n\"c\""}"#),
            Some(StatusEvent::Prompt { text: Some("a;b\n\"c\"".into()) })
        );
    }
```

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib status`
Expected: FAIL (compile error — variants don't match).

- [ ] **Step 2: Implement the `status.rs` changes**

Variant reshaping + new variant (update the doc comments to match):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusEvent {
    /// UserPromptSubmit: an assistant turn began. `text` is the user's prompt
    /// (sliced at the wire by jq); missing when the hook ran without jq.
    Prompt { text: Option<String> },
    /// PreToolUse: a tool is about to run.
    Tool { name: String },
    /// Stop: the turn ended. Duration is computed by the receiver, not sent.
    Stop,
    /// Notification: the session wants attention. `msg` is optional because
    /// the hook degrades to a bare notify when jq is unavailable.
    Notify { msg: Option<String> },
    /// PostToolUse for AskUserQuestion: the user submitted a choice.
    /// Panel-only — drives no dashboard transition.
    Choice { text: Option<String> },
}
```

`Wire` gains the field:

```rust
#[derive(Debug, Deserialize)]
struct Wire {
    e: String,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    text: Option<String>,
}
```

`decode` — factor the empty-string rule into a helper and use it for all three optional strings:

```rust
/// Empty strings carry no information; treat them like missing ones.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

pub fn decode(json: &str) -> Option<StatusEvent> {
    let wire: Wire = serde_json::from_str(json).ok()?;
    match wire.e.as_str() {
        "prompt" => Some(StatusEvent::Prompt { text: non_empty(wire.text) }),
        // An empty tool name would render as a blank dashboard cell; treat it
        // like a missing one.
        "tool" => Some(StatusEvent::Tool { name: non_empty(wire.tool)? }),
        "stop" => Some(StatusEvent::Stop),
        "notify" => Some(StatusEvent::Notify { msg: non_empty(wire.msg) }),
        "choice" => Some(StatusEvent::Choice { text: non_empty(wire.text) }),
        _ => None,
    }
}
```

Also extend the module doc comment's event list with `choice`.

- [ ] **Step 3: Keep `session.rs` compiling (mechanical)**

In `handle_status`, the match arm becomes `StatusEvent::Prompt { .. } => registry.begin_turn(tab_id, now),` and a new arm is added (Task 5 replaces the no-op body's context; for now):

```rust
            // Panel-only event; Task 5 wires the panel row. No dashboard
            // transition, and the `tab-status` re-emit below is a harmless
            // full-state replacement of unchanged state.
            StatusEvent::Choice { .. } => {}
```

- [ ] **Step 4: Write the failing `agent_events.rs` tests**

Create `src-tauri/src/agent_events.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::StatusEvent;

    #[test]
    fn prompt_maps_to_user_prompt() {
        assert_eq!(
            panel_event(&StatusEvent::Prompt { text: Some("fix the build".into()) }),
            Some((AgentEventKind::UserPrompt, "fix the build".into()))
        );
        // Without jq there is no text; the row still exists.
        assert_eq!(
            panel_event(&StatusEvent::Prompt { text: None }),
            Some((AgentEventKind::UserPrompt, String::new()))
        );
    }

    #[test]
    fn notify_maps_to_agent_question() {
        assert_eq!(
            panel_event(&StatusEvent::Notify { msg: Some("needs permission".into()) }),
            Some((AgentEventKind::AgentQuestion, "needs permission".into()))
        );
    }

    #[test]
    fn stop_maps_to_agent_done_without_text() {
        assert_eq!(
            panel_event(&StatusEvent::Stop),
            Some((AgentEventKind::AgentDone, String::new()))
        );
    }

    #[test]
    fn choice_maps_to_user_choice() {
        assert_eq!(
            panel_event(&StatusEvent::Choice { text: Some("Option B".into()) }),
            Some((AgentEventKind::UserChoice, "Option B".into()))
        );
    }

    #[test]
    fn tool_produces_no_panel_row() {
        assert_eq!(panel_event(&StatusEvent::Tool { name: "Bash".into() }), None);
    }

    #[test]
    fn text_truncated_to_limit_chars() {
        let long = "字".repeat(300);
        let (_, text) = panel_event(&StatusEvent::Prompt { text: Some(long) }).unwrap();
        assert_eq!(text.chars().count(), TEXT_LIMIT);
        assert!(text.chars().all(|c| c == '字'));
    }

    #[test]
    fn whitespace_flattens_to_single_spaces() {
        let (_, text) = panel_event(&StatusEvent::Prompt {
            text: Some("  line one\n\tline  two  ".into()),
        })
        .unwrap();
        assert_eq!(text, "line one line two");
    }

    #[test]
    fn control_chars_become_spaces() {
        let (_, text) =
            panel_event(&StatusEvent::Prompt { text: Some("a\u{1}b\u{1b}c".into()) }).unwrap();
        assert_eq!(text, "a b c");
    }

    /// Pins the IPC contract for `src/types.ts`: kebab-case kind, single-word
    /// fields identical in any casing convention.
    #[test]
    fn agent_event_serializes_for_ipc() {
        let event = AgentEvent {
            id: "e1".into(),
            kind: AgentEventKind::UserPrompt,
            text: "hi".into(),
            time: 1700000000000,
            seq: 42,
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({"id": "e1", "kind": "user-prompt", "text": "hi", "time": 1700000000000u64, "seq": 42})
        );
    }
}
```

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib agent_events`
Expected: FAIL — items missing.

- [ ] **Step 5: Implement `agent_events.rs`**

Above the test module:

```rust
//! Conversation events for the agent panel.
//!
//! The OSC 7777 hook protocol (`crate::status`) already reports the turn
//! lifecycle; this module maps those decoded events to panel rows — what
//! happened, summary text, and (filled in by the caller) when and at which
//! position in the PTY byte stream, which is what anchors click-to-jump.

use crate::status::StatusEvent;
use serde::Serialize;

/// Maximum characters of summary text kept per event. The hooks' jq slices
/// text to the same limit at the wire; this is the authoritative cap (a
/// manual payload can carry more, up to the parser's MAX_OSC_LEN).
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
    /// Summary text: whitespace-flattened, at most `TEXT_LIMIT` chars.
    pub text: String,
    /// Unix milliseconds: when Rust received the OSC.
    pub time: u64,
    /// Absolute PTY stream position where the OSC sequence started.
    pub seq: u64,
}

/// The panel row for a decoded protocol event, or `None` when it is not
/// conversation-visible (tool events drive the dashboard only).
pub fn panel_event(event: &StatusEvent) -> Option<(AgentEventKind, String)> {
    let (kind, text) = match event {
        StatusEvent::Prompt { text } => (AgentEventKind::UserPrompt, text.as_deref().unwrap_or("")),
        StatusEvent::Notify { msg } => (AgentEventKind::AgentQuestion, msg.as_deref().unwrap_or("")),
        StatusEvent::Stop => (AgentEventKind::AgentDone, ""),
        StatusEvent::Choice { text } => (AgentEventKind::UserChoice, text.as_deref().unwrap_or("")),
        StatusEvent::Tool { .. } => return None,
    };
    Some((kind, normalize_text(text)))
}

/// Panel rows are single-line: control characters become spaces, whitespace
/// runs collapse, edges trim, and the result is truncated to `TEXT_LIMIT`
/// chars (char-based, so CJK text is never cut mid-codepoint).
fn normalize_text(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= TEXT_LIMIT {
        flat
    } else {
        flat.chars().take(TEXT_LIMIT).collect()
    }
}
```

- [ ] **Step 6: Register the module**

In `lib.rs`, add `mod agent_events;` — alphabetically first, before `mod attention;`.

- [ ] **Step 7: Run the full Rust suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib && cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS (status + agent_events tests, no warnings from changed code).

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/status.rs src-tauri/src/agent_events.rs src-tauri/src/lib.rs src-tauri/src/pty/session.rs
git commit -m "status: carry prompt/choice text; agent_events: map protocol events to panel rows

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 4: `Registry` — per-tab in-memory event store with duplicate guard

**Files:**
- Modify: `src-tauri/src/pty/registry.rs`
- Test: inline `mod tests` in `registry.rs`

**Interfaces:**
- Consumes: `crate::agent_events::{AgentEvent, AgentEventKind}` (Task 3).
- Produces (Tasks 5/6 rely on these):
  - `Registry::push_agent_event(&self, tab_id: &str, event: AgentEvent) -> bool` — true when stored; false for unknown tabs and rejected duplicates.
  - `Registry::agent_events(&self, tab_id: &str) -> Vec<AgentEvent>`
  - `Registry::remove` additionally clears the tab's events.
  - Module-private: `const AGENT_EVENT_LIMIT: usize = 200`, `const AGENT_DUP_WINDOW_MS: u64 = 2000`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `registry.rs`:

```rust
    use crate::agent_events::{AgentEvent, AgentEventKind};

    fn agent_event(id: &str, kind: AgentEventKind, time: u64, seq: u64) -> AgentEvent {
        AgentEvent {
            id: id.to_string(),
            kind,
            text: id.to_string(),
            time,
            seq,
        }
    }

    #[test]
    fn agent_events_round_trip_in_order() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        assert!(registry.push_agent_event("t1", agent_event("a", AgentEventKind::UserPrompt, 1, 1)));
        assert!(registry.push_agent_event("t1", agent_event("b", AgentEventKind::AgentDone, 2, 2)));
        let ids: Vec<_> = registry.agent_events("t1").iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids, vec!["a", "b"]);
        assert!(registry.agent_events("nope").is_empty());
    }

    #[test]
    fn agent_events_cap_evicts_oldest() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        for i in 0..205 {
            // Distinct times well outside the dedup window; distinct texts.
            registry.push_agent_event(
                "t1",
                agent_event(&format!("e{i}"), AgentEventKind::UserPrompt, (i * 10_000) as u64, i),
            );
        }
        let events = registry.agent_events("t1");
        assert_eq!(events.len(), 200);
        assert_eq!(events.first().unwrap().id, "e5");
        assert_eq!(events.last().unwrap().id, "e204");
    }

    /// Hooks fire once per settings level: the same event can arrive twice
    /// within moments. The second copy must not double-row the panel.
    #[test]
    fn duplicate_within_window_is_rejected() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        assert!(registry.push_agent_event("t1", agent_event("a", AgentEventKind::AgentDone, 1000, 10)));
        // Same kind + same text, 500 ms later, different seq/id.
        let dup = AgentEvent { id: "b".into(), kind: AgentEventKind::AgentDone, text: "a".into(), time: 1500, seq: 90 };
        assert!(!registry.push_agent_event("t1", dup));
        assert_eq!(registry.agent_events("t1").len(), 1);
    }

    #[test]
    fn duplicate_outside_window_or_different_content_is_accepted() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        registry.push_agent_event("t1", agent_event("a", AgentEventKind::AgentDone, 1000, 10));
        // Same kind+text but 3 s later: a genuine second turn end.
        let later = AgentEvent { id: "b".into(), kind: AgentEventKind::AgentDone, text: "a".into(), time: 3001, seq: 90 };
        assert!(registry.push_agent_event("t1", later));
        // Inside the window but different text: a genuine new prompt.
        let different = AgentEvent { id: "c".into(), kind: AgentEventKind::UserPrompt, text: "new".into(), time: 3100, seq: 120 };
        assert!(registry.push_agent_event("t1", different));
        assert_eq!(registry.agent_events("t1").len(), 3);
    }

    #[test]
    fn unknown_tab_agent_events_are_noops() {
        let registry = Registry::new();
        assert!(!registry.push_agent_event("nope", agent_event("a", AgentEventKind::UserPrompt, 1, 1)));
        assert!(registry.agent_events("nope").is_empty());
    }

    #[test]
    fn removing_a_tab_clears_its_events() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        registry.push_agent_event("t1", agent_event("a", AgentEventKind::UserPrompt, 1, 1));
        registry.remove("t1");
        assert!(registry.agent_events("t1").is_empty());
    }
```

Note the `agent_events_cap_evicts_oldest` times: consecutive pushes 10 s apart with distinct texts also exercise that the dedup guard never misfires on a healthy stream.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib registry`
Expected: FAIL — `push_agent_event` / `agent_events` don't exist.

- [ ] **Step 3: Implement**

In `registry.rs` — imports and constants:

```rust
use crate::agent_events::{AgentEvent, AgentEventKind};
use std::collections::{HashMap, VecDeque};

/// Per-tab cap on retained conversation events; oldest are evicted first.
const AGENT_EVENT_LIMIT: usize = 200;
/// Same kind + same text within this window is a duplicate hook fire (hooks
/// merge across settings levels), not a second conversation event.
const AGENT_DUP_WINDOW_MS: u64 = 2000;
```

Struct gains the second lock (lock order: `tabs` then `agent_events` — the same order `remove` establishes; no path takes them the other way):

```rust
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
    /// Store one decoded conversation event for `tab_id`. Returns false (and
    /// stores nothing) for unknown tabs — a push racing a close must not
    /// orphan an entry — and for duplicate hook fires: same kind and text as
    /// the newest event, within `AGENT_DUP_WINDOW_MS`.
    pub fn push_agent_event(&self, tab_id: &str, event: AgentEvent) -> bool {
        if !lock(&self.tabs).iter().any(|t| t.id == tab_id) {
            return false;
        }
        let mut map = lock(&self.agent_events);
        let events = map.entry(tab_id.to_string()).or_default();
        if let Some(last) = events.back() {
            if last.kind == event.kind
                && last.text == event.text
                && event.time.saturating_sub(last.time) <= AGENT_DUP_WINDOW_MS
            {
                return false;
            }
        }
        events.push_back(event);
        while events.len() > AGENT_EVENT_LIMIT {
            events.pop_front();
        }
        true
    }

    pub fn agent_events(&self, tab_id: &str) -> Vec<AgentEvent> {
        lock(&self.agent_events)
            .get(tab_id)
            .map(|events| events.iter().cloned().collect())
            .unwrap_or_default()
    }
```

And in the existing `remove`, after `tabs.retain(...)` — release the `tabs` lock first (scope), then clean up, keeping the documented lock order:

```rust
    pub fn remove(&self, id: &str) -> bool {
        let removed = {
            let mut tabs = lock(&self.tabs);
            let before = tabs.len();
            tabs.retain(|t| t.id != id);
            tabs.len() != before
        };
        lock(&self.agent_events).remove(id);
        removed
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib registry`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/pty/registry.rs
git commit -m "registry: per-tab agent event store (cap 200, duplicate-hook guard)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 5: `session.rs` — wire offsets, panel events, and the `agent-event` IPC

**Files:**
- Modify: `src-tauri/src/pty/session.rs`
- Test: no new unit tests (AppHandle-bound wiring; its parts are tested in Tasks 2–4). The full suite must stay green.

**Interfaces:**
- Consumes: parser offsets (Task 2), `StatusEvent::Prompt{text}`/`Choice` (Task 3), `agent_events::{panel_event, AgentEvent}` (Task 3), `Registry::push_agent_event` (Task 4), `uuid` (already a dependency).
- Produces: the Tauri event **`agent-event`** with payload `{ "tab_id": String, "event": AgentEvent }` — Task 7 listens to exactly this shape. Emitted only when the registry accepted the event (duplicates must not double-row the renderer's live list, which would diverge from the restored-after-reload list).

- [ ] **Step 1: Thread the offset through `read_loop` and `handle_osc`**

Replace the Task-2 interim loop:

```rust
                    for (event, offset) in parser.parse(data) {
                        Self::handle_osc(
                            &tab_id,
                            &app,
                            &registry,
                            &program_active,
                            &last_activity,
                            &flashed,
                            event,
                            offset,
                        );
                    }
```

`handle_osc` gains `offset: u64` as its last parameter (it already carries `#[allow(clippy::too_many_arguments)]`-worthy arity via `read_loop`'s pattern; add the attribute to `handle_osc` if clippy complains — the codebase tolerates it on `read_loop`). Its `OscEvent::Clitab(json)` arm passes the offset on:

```rust
            OscEvent::Clitab(json) => {
                // Unknown kinds and malformed payloads decode to None and are
                // dropped: a hook emitting something newer than this build
                // must be a no-op, never an error.
                if let Some(event) = status::decode(&json) {
                    Self::handle_status(tab_id, app, registry, flashed, event, offset);
                }
            }
```

Every other `handle_osc` arm is unchanged (the `offset` parameter is simply unused there).

- [ ] **Step 2: Extend `handle_status`**

New import at the top of `session.rs`:

```rust
use crate::agent_events::{self, AgentEvent};
```

Restructured function — the existing transitions and effects stay byte-for-byte identical; the panel block and the `Choice` skip are the only additions. `panel_event` borrows the event before the by-value match consumes it:

```rust
    /// Apply one hook-protocol event: registry transition, panel row, then
    /// broadcast the tab's full protocol state so the renderer replaces (not
    /// merges) it. `offset` is the absolute stream position of the OSC that
    /// carried the event — the panel row's jump anchor.
    fn handle_status(
        tab_id: &str,
        app: &AppHandle,
        registry: &Registry,
        flashed: &AtomicBool,
        event: StatusEvent,
        offset: u64,
    ) {
        let now = now_ms();
        // Borrow before the match consumes the event.
        let panel = agent_events::panel_event(&event);
        // Choice is panel-only: no dashboard transition, no state re-emit.
        let panel_only = matches!(event, StatusEvent::Choice { .. });

        match event {
            StatusEvent::Prompt { .. } => registry.begin_turn(tab_id, now),
            StatusEvent::Tool { name } => registry.set_tool(tab_id, &name, now),
            StatusEvent::Stop => {
                registry.end_turn(tab_id, now);
                // (existing body unchanged — flash, triage queue, and the
                // invariant comment about not touching program_active/title)
                flashed.store(true, Ordering::Relaxed);
                crate::attention::enter_waiting(app, registry, tab_id);
                let _ = app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
            }
            StatusEvent::Notify { msg } => {
                registry.set_notice(tab_id, msg, now);
                crate::attention::enter_waiting(app, registry, tab_id);
                let _ = app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
            }
            StatusEvent::Choice { .. } => {}
        }

        // Panel row, anchored at the OSC's stream position. Emitted only when
        // the store accepted it: a duplicate hook fire (hooks merge across
        // settings levels) must not double-row the renderer's live list.
        if let Some((kind, text)) = panel {
            let agent_event = AgentEvent {
                id: uuid::Uuid::new_v4().to_string(),
                kind,
                text,
                time: now,
                seq: offset,
            };
            if registry.push_agent_event(tab_id, agent_event.clone()) {
                let _ = app.emit(
                    "agent-event",
                    serde_json::json!({ "tab_id": tab_id, "event": agent_event }),
                );
            }
        }

        if !panel_only {
            if let Some(tab) = registry.get(tab_id) {
                let _ = app.emit(
                    "tab-status",
                    serde_json::json!({
                        "tab_id": tab_id,
                        "status": tab.status,
                        "notice": tab.notice,
                    }),
                );
            }
        }
    }
```

Preserve the existing doc comments inside the `Stop` arm verbatim (the "explicit turn-end beats the 2s idle heuristic" and "Deliberately does NOT touch program_active or the title" comments).

**Invariant checks:**
- Everything above runs inside `handle_osc`, i.e. *before* the stream lock is taken — same as every other OSC event today. Do not move any emit into or after the stream-lock section.
- No new `program_active` / title effects anywhere. `stop` still never touches the title (shipped invariant; `claude-done` owns the revert).
- `now_ms()` already exists in `session.rs` — reuse it, do not add a second helper.

- [ ] **Step 3: Run the full Rust suite + build**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib && cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS / no warnings from the changed code.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/pty/session.rs
git commit -m "session: anchor decoded hook events into the panel store, emit agent-event

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 6: One-click hook install — merge logic, commands, IPC

**Files:**
- Create: `src-tauri/src/claude_hooks.rs`
- Modify: `src-tauri/src/lib.rs` (`mod claude_hooks;`, three commands, handler list)
- Modify: `src-tauri/src/pty/manager.rs` (`list_agent_events` passthrough)
- Test: inline `mod tests` in `claude_hooks.rs`

**Interfaces:**
- Consumes: `Registry::agent_events` (Task 4), `crate::agent_events::AgentEvent` (Task 3), Task 1's pinned choice jq expression.
- Produces (Task 7 invokes these by name):
  - Tauri command `list_agent_events(tab_id: String) -> Result<Vec<AgentEvent>, String>` — `TAB_GONE`-prefixed error for a vanished tab.
  - Tauri command `claude_hooks_status() -> bool`.
  - Tauri command `install_claude_hooks() -> Result<(), String>`.
  - Pure fn `pub fn merge_hooks(settings: serde_json::Value) -> serde_json::Value`.
  - `pub const OSC_MARKER: &str = "]7777;"` — ownership detection: a hook group is clitab-owned when any of its commands contains the marker. This recognises both previous one-click installs and the manual inline setup from `CLAUDE_HOOKS.md` (OSC 7777 is clitab's private code; no foreign tool emits it).

**Design note:** no hook script file, no app-data-dir dependency. The installer writes the same inline commands `CLAUDE_HOOKS.md` documents (Task 10 syncs the doc to match byte-for-byte), so `settings.json` stays self-contained and an existing manual install is upgraded in place.

- [ ] **Step 1: Write the failing merge tests**

Create `src-tauri/src/claude_hooks.rs` with the test module first. The `merge_replaces_existing_manual_osc_entries` fixture uses the **exact command strings from the current `CLAUDE_HOOKS.md`** — that is the upgrade path real users hit:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The manual UserPromptSubmit command as shipped in CLAUDE_HOOKS.md
    /// (pre-panel: no text field).
    const MANUAL_PROMPT: &str = r#"t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); [ -n "$t" ] && [ "$t" != '??' ] && printf '\033]7777;{"e":"prompt"}\033\\' > /dev/$t 2>/dev/null; true"#;
    /// The manual Notification command as shipped in CLAUDE_HOOKS.md.
    const MANUAL_NOTIFY: &str = r#"t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); j=$(jq -c '{e:"notify",msg:.message}' 2>/dev/null || printf '{"e":"notify"}'); [ -n "$t" ] && [ "$t" != '??' ] && printf '\033]7777;%s\033\\' "$j" > /dev/$t 2>/dev/null; true"#;

    fn manual_settings() -> serde_json::Value {
        json!({
            "hooks": {
                "UserPromptSubmit": [{ "hooks": [
                    { "type": "command", "command": MANUAL_PROMPT }
                ]}],
                "PreToolUse": [{ "matcher": "", "hooks": [
                    { "type": "command", "command": "j=$(jq -c '{e:\"tool\",tool:.tool_name}' 2>/dev/null); printf 'x'" }
                ]}],
                "Notification": [{ "matcher": "", "hooks": [
                    { "type": "command", "command": MANUAL_NOTIFY }
                ]}]
            },
            "theme": "dark"
        })
    }

    #[test]
    fn merge_adds_all_five_hooks_to_empty_settings() {
        let merged = merge_hooks(json({}));
        let hooks = merged.get("hooks").unwrap();
        for (event, matcher) in [
            ("UserPromptSubmit", None),
            ("PreToolUse", Some("")),
            ("Notification", Some("")),
            ("Stop", None),
            ("PostToolUse", Some("AskUserQuestion")),
        ] {
            let groups = hooks.get(event).and_then(|g| g.as_array()).unwrap();
            assert_eq!(groups.len(), 1, "{event}");
            assert_eq!(groups[0].get("matcher").and_then(|m| m.as_str()), matcher, "{event}");
            let cmd = groups[0]["hooks"][0]["command"].as_str().unwrap();
            assert!(cmd.contains(OSC_MARKER), "{event} command must speak OSC 7777");
            assert!(cmd.ends_with("; true"), "{event} command must never fail the hook");
        }
        // The panel-bearing payloads are present.
        assert!(hooks["UserPromptSubmit"][0]["hooks"][0]["command"]
            .as_str().unwrap().contains(r#"e:"prompt",text:"#));
        assert!(hooks["PostToolUse"][0]["hooks"][0]["command"]
            .as_str().unwrap().contains(r#"e:"choice""#));
    }

    #[test]
    fn merge_preserves_foreign_hooks() {
        let settings = json!({
            "hooks": {
                "Notification": [{ "matcher": "", "hooks": [
                    { "type": "command", "command": "my-own-notifier" }
                ]}]
            },
            "theme": "dark"
        });
        let merged = merge_hooks(settings);
        // The user's own Notification hook is untouched, ours is appended.
        let groups = merged["hooks"]["Notification"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["hooks"][0]["command"], json!("my-own-notifier"));
        assert!(groups[1]["hooks"][0]["command"].as_str().unwrap().contains(OSC_MARKER));
        // Unrelated keys survive verbatim.
        assert_eq!(merged["theme"], json!("dark"));
    }

    /// The upgrade path: a user who set up the dashboard manually per
    /// CLAUDE_HOOKS.md must end up with exactly the installer's commands —
    /// no duplicates, no orphaned pre-panel variants.
    #[test]
    fn merge_replaces_existing_manual_osc_entries() {
        let merged = merge_hooks(manual_settings());
        for event in ["UserPromptSubmit", "PreToolUse", "Notification", "Stop", "PostToolUse"] {
            let groups = merged["hooks"][event].as_array().unwrap();
            let owned: Vec<_> = groups.iter().filter(|g| group_owns_clitab(g)).collect();
            assert_eq!(owned.len(), 1, "{event}: exactly one clitab group");
        }
        // The stale prompt command (no text field) is gone, replaced by the
        // installer's current one.
        let prompt = merged["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(prompt.len(), 1);
        assert_ne!(prompt[0]["hooks"][0]["command"], json!(MANUAL_PROMPT));
        // The fixture's PreToolUse stub mentions jq but carries no OSC
        // marker: ownership keys on the marker, so it counts as foreign —
        // preserved, with ours appended.
        let tool = merged["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(tool.len(), 2, "foreign PreToolUse survives, ours appended");
        assert!(!group_owns_clitab(&tool[0]));
    }

    #[test]
    fn merge_is_idempotent() {
        let once = merge_hooks(json({}));
        let twice = merge_hooks(once.clone());
        assert_eq!(once, twice);
    }

    #[test]
    fn merge_normalizes_broken_shapes() {
        // `hooks` not an object, an event entry not an array, a group without
        // a hooks array: none of these may panic or block the install.
        let merged = merge_hooks(json!({ "hooks": "garbage" }));
        assert!(merged["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str().unwrap().contains(OSC_MARKER));

        let merged = merge_hooks(json!({
            "hooks": { "Stop": "garbage", "Notification": [ { "matcher": "x" } ] }
        }));
        assert_eq!(merged["hooks"]["Stop"].as_array().unwrap().len(), 1);
        // The foreign group without a hooks array is preserved (not ours).
        let notif = merged["hooks"]["Notification"].as_array().unwrap();
        assert_eq!(notif.len(), 2);
        assert_eq!(notif[0]["matcher"], json!("x"));
    }

    #[test]
    fn hooks_installed_detects_the_marker() {
        assert!(hooks_installed_in(&manual_settings()));
        assert!(!hooks_installed_in(&json!({})));
        assert!(!hooks_installed_in(&json!({ "hooks": { "Stop": [] } })));
    }

    /// Testable core of `hooks_installed` (which adds file IO).
    fn hooks_installed_in(settings: &serde_json::Value) -> bool {
        settings
            .get("hooks")
            .map(|hooks| hooks.to_string().contains(OSC_MARKER))
            .unwrap_or(false)
    }
}
```

**Fixture caveat:** the `manual_settings()` PreToolUse stub deliberately does NOT contain `]7777;` (it is a foreign command that merely mentions jq) — this pins that ownership detection keys on the OSC marker, not on jq or tty usage. If Step 3's implementation disagrees with any comment embedded in a test, fix the **implementation** to match the test's intent, not the reverse.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib claude_hooks`
Expected: FAIL — `merge_hooks` / `group_owns_clitab` / `OSC_MARKER` missing.

- [ ] **Step 3: Implement `claude_hooks.rs`**

Above the test module:

```rust
//! One-click installation of the Claude Code hooks that feed the tab
//! dashboard and the agent event panel. The registrations are the exact
//! inline commands `CLAUDE_HOOKS.md` documents (kept in sync by hand — the
//! doc's manual-setup JSON must match these strings); `~/.claude/settings.json`
//! is merged, never clobbered: foreign hooks survive, clitab-owned entries
//! (recognised by the OSC 7777 marker in their command) are replaced on
//! reinstall — which is also how an existing manual setup gets upgraded —
//! and a malformed settings file is refused untouched.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Ownership marker: OSC 7777 is clitab's private code, so any hook command
/// writing it is a clitab hook (ours or the documented manual equivalent).
pub const OSC_MARKER: &str = "]7777;";

// Every command below uses the ancestor-tty transport (probe verdict, commit
// efd762d): hooks have no controlling terminal and their stdout is captured
// by Claude Code, so the only reliable path is the parent's tty device.
// Every command ends in `; true`: a failing hook must never block Claude Code.

const PROMPT_CMD: &str = r#"t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); j=$(jq -c '{e:"prompt",text:(.prompt//""|tostring|.[0:140])}' 2>/dev/null || printf '{"e":"prompt"}'); [ -n "$t" ] && [ "$t" != '??' ] && printf '\033]7777;%s\033\\' "$j" > /dev/$t 2>/dev/null; true"#;

const TOOL_CMD: &str = r#"t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); j=$(jq -c '{e:"tool",tool:.tool_name}' 2>/dev/null); [ -n "$t" ] && [ "$t" != '??' ] && [ -n "$j" ] && printf '\033]7777;%s\033\\' "$j" > /dev/$t 2>/dev/null; true"#;

const STOP_CMD: &str = r#"t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); [ -n "$t" ] && [ "$t" != '??' ] && printf '\033]7777;{"e":"stop"}\033\\' > /dev/$t 2>/dev/null; true"#;

const NOTIFY_CMD: &str = r#"t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); j=$(jq -c '{e:"notify",msg:.message}' 2>/dev/null || printf '{"e":"notify"}'); [ -n "$t" ] && [ "$t" != '??' ] && printf '\033]7777;%s\033\\' "$j" > /dev/$t 2>/dev/null; true"#;

/// Task 1's probe pinned this jq expression against a real AskUserQuestion
/// `tool_response`; if the probe report adjusted it, this constant carries
/// the adjusted form.
const CHOICE_CMD: &str = r#"t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); j=$(jq -c '{e:"choice",text:((.tool_response.answers // .tool_response // "")|tostring|.[0:140])}' 2>/dev/null); [ -n "$t" ] && [ "$t" != '??' ] && [ -n "$j" ] && printf '\033]7777;%s\033\\' "$j" > /dev/$t 2>/dev/null; true"#;

/// The five hook events clitab registers: (event, matcher, command).
/// `None` matcher means the group omits the key entirely.
fn registrations() -> [(&'static str, Option<&'static str>, &'static str); 5] {
    [
        ("UserPromptSubmit", None, PROMPT_CMD),
        ("PreToolUse", Some(""), TOOL_CMD),
        ("Notification", Some(""), NOTIFY_CMD),
        ("Stop", None, STOP_CMD),
        ("PostToolUse", Some("AskUserQuestion"), CHOICE_CMD),
    ]
}

pub fn settings_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
        .join(".claude")
        .join("settings.json")
}

/// True when settings.json already contains clitab-owned hook entries
/// (manual or installed).
pub fn hooks_installed(path: &Path) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .map(|settings| {
            settings
                .get("hooks")
                .map(|hooks| hooks.to_string().contains(OSC_MARKER))
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
                    .map(|c| c.contains(OSC_MARKER))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Add (or replace) clitab's five hook registrations in a settings document.
/// Normalizes broken shapes instead of panicking; foreign data is preserved.
pub fn merge_hooks(mut settings: Value) -> Value {
    if !settings.is_object() {
        settings = json!({});
    }
    let root = settings.as_object_mut().expect("checked above");
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    for (event, matcher, command) in registrations() {
        let entry = hooks
            .as_object_mut()
            .expect("checked above")
            .entry(event)
            .or_insert_with(|| json!([]));
        if !entry.is_array() {
            *entry = json!([]);
        }
        let groups = entry.as_array_mut().expect("checked above");
        // Replaces stale clitab entries (any prior one-click install or the
        // documented manual commands); foreign groups stay.
        groups.retain(|group| !group_owns_clitab(group));
        let mut group = serde_json::Map::new();
        if let Some(m) = matcher {
            group.insert("matcher".into(), json!(m));
        }
        group.insert(
            "hooks".into(),
            json!([{ "type": "command", "command": command }]),
        );
        groups.push(Value::Object(group));
    }
    settings
}

/// Merge the registrations into ~/.claude/settings.json. Errors leave the
/// file untouched; the first successful install writes a `settings.json.bak`.
pub fn install() -> Result<(), String> {
    let settings_path = settings_path();
    let existing = match std::fs::read_to_string(&settings_path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("could not read settings.json: {e}")),
    };
    let current: Value = match &existing {
        Some(text) => serde_json::from_str(text)
            .map_err(|_| "settings.json is not valid JSON; refusing to touch it".to_string())?,
        None => json!({}),
    };

    if let Some(text) = &existing {
        let backup = settings_path.with_file_name("settings.json.bak");
        if !backup.exists() {
            std::fs::write(&backup, text)
                .map_err(|e| format!("could not write settings backup: {e}"))?;
        }
    }

    let merged = merge_hooks(current);
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create ~/.claude: {e}"))?;
    }
    let pretty = serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?;
    std::fs::write(&settings_path, pretty).map_err(|e| format!("could not write settings.json: {e}"))?;
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib claude_hooks`
Expected: PASS (6 tests).

- [ ] **Step 5: Add the `TabManager` passthrough**

In `src-tauri/src/pty/manager.rs` (import: `use crate::agent_events::AgentEvent;`):

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

(Adjust `self.registry` to however `TabManager` names its registry field — check the struct; the `ack_notice` passthrough added for the dashboard is the pattern to copy.)

- [ ] **Step 6: Add the three commands in `lib.rs`**

Import: `use crate::agent_events::AgentEvent;`.

```rust
/// Stored conversation events of a tab, for panel restore after a webview
/// reload. Live updates arrive via the `agent-event` Tauri event.
#[tauri::command]
fn list_agent_events(
    state: State<'_, AppState>,
    tab_id: String,
) -> Result<Vec<AgentEvent>, String> {
    state
        .tab_manager
        .list_agent_events(&tab_id)
        .map_err(ipc_error)
}

/// Whether ~/.claude/settings.json carries clitab's hook registrations
/// (manual or installed). Drives the panel's "Install hooks" empty state.
#[tauri::command]
fn claude_hooks_status() -> bool {
    claude_hooks::hooks_installed(&claude_hooks::settings_path())
}

/// Merge the five OSC 7777 hook registrations into ~/.claude/settings.json
/// (idempotent; upgrades manual installs; backs the file up once).
#[tauri::command]
fn install_claude_hooks() -> Result<(), String> {
    claude_hooks::install()
}
```

Add `mod claude_hooks;` at the top (alphabetically after `mod attention;`), and `list_agent_events, claude_hooks_status, install_claude_hooks` to the `generate_handler!` list.

- [ ] **Step 7: Run full suite + build**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib && cargo check --manifest-path src-tauri/Cargo.toml`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/claude_hooks.rs src-tauri/src/lib.rs src-tauri/src/pty/manager.rs
git commit -m "hooks: one-click OSC 7777 install + list_agent_events command

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
  - `types.ts`: `AgentEventKind`, `AgentEvent`, `AgentEventPayload`.
  - `TabManagerState` additions: `agentEvents: Record<string, AgentEvent[]>` (state, for rendering), `eventsForTab: (tabId: string) => AgentEvent[]` (reads a **synchronously-updated ref** — see rationale below), `hooksInstalled: boolean`, `installHooks: () => Promise<void>`.
  - `OutputHandler` becomes `(chunk: Uint8Array, seq: number, isReplay?: boolean) => void`. The internal `StreamSink` already carries `seq` — this task only surfaces it.

**Why both state and a ref:** the `agent-event` Tauri event is emitted *before* the `pty-output` chunk containing the OSC bytes (backend invariant, Task 5). If anchoring read events from React state, the chunk could be written before the state update re-renders — the anchor would be missed. `eventsForTab` therefore reads a plain `Map` ref that the listener mutates synchronously; the state mirror exists only to re-render the panel. This is the same "callbacks live in a ref" pattern `useTabManager` already uses for output handlers.

- [ ] **Step 1: Extend `src/types.ts`**

```ts
export type AgentEventKind = 'user-prompt' | 'agent-question' | 'agent-done' | 'user-choice';

/** One Claude Code conversation event, anchored to the PTY stream via `seq`.
 *  Mirrors `AgentEvent` in `src-tauri/src/agent_events.rs`. */
export interface AgentEvent {
  id: string;
  kind: AgentEventKind;
  /** Summary text, at most 140 chars, whitespace-flattened; may be empty
   *  (e.g. agent-done, or a hook that ran without jq). */
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

2a. Imports and handler type:

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

(`StreamSink` is unchanged — it already carries `seq`.)

2b. New state + ref next to the existing `handlers` ref:

```ts
  const [agentEvents, setAgentEvents] = useState<Record<string, AgentEvent[]>>({});
  const [hooksInstalled, setHooksInstalled] = useState(false);
  // Synchronous mirror for terminal anchoring: the `agent-event` arrives
  // before the pty-output chunk containing its OSC bytes, and React state
  // updates are batched — anchoring that read state could miss the chunk.
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

2d. `attachTab` — surface seq on every path (dedup logic unchanged):

```ts
      const subscription: StreamSink = (chunk, seq) => {
        if (live) handler(chunk, seq);
        else queued.push({ chunk, seq });
      };
```

replay call (the ring ends at `replayEnd`, so it starts at `replayEnd - bytes.length`):

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

2e. Startup — restore events **before** setting tabs, so a mounting Terminal sees them while its replay is written (otherwise replay anchoring would race the restore). Replace the `invoke<TabResponse[]>('list_tabs').then((loaded) => { ... })` body with:

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

2h. Extend `TabManagerState` (the interface) and the returned object with: `agentEvents`, `eventsForTab`, `hooksInstalled`, `installHooks`.

- [ ] **Step 3: Adapt `Terminal.tsx` to the new handler signature (no behavior change)**

The `TerminalProps.attach` type and the mount effect's attach callback become `(chunk, _seq, isReplay)` — the underscore keeps unused-parameter checks quiet; Task 8 puts `_seq` to work. The replay branch body is unchanged:

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

Add `eventsForTab` to the `callbacks` ref object (same pattern as the existing four callbacks), with defaults in destructuring:

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
    // (The classifyReplay comment above stays as-is.)
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

### Task 10: Documentation — READMEs + CLAUDE_HOOKS.md + CLAUDE.md

**Files:**
- Modify: `README.md`, `README.zh-CN.md` (kept in sync with each other — repo rule)
- Modify: `CLAUDE_HOOKS.md` (update in place — it already documents OSC 7777 and the ancestor-tty transport)
- Modify: `CLAUDE.md` (architecture bullets)
- Verify: prose only; both READMEs say the same things; the manual-setup JSON in `CLAUDE_HOOKS.md` matches `claude_hooks.rs`'s command constants byte-for-byte.

**Interfaces:**
- Consumes: final behavior from Tasks 1–9; the five command constants in `claude_hooks.rs`.
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
the panel. clitab merges five hook registrations into `~/.claude/settings.json`
(the same inline commands [CLAUDE_HOOKS.md](CLAUDE_HOOKS.md) documents) — your
own hooks are preserved, an existing manual dashboard setup is upgraded in
place, a `settings.json.bak` backup is created on first install, and
reinstalling is safe. Events are kept in memory per tab (most recent 200) and
are not persisted to disk; jumps work as long as the position is still in the
terminal's scrollback.
```

- [ ] **Step 2: README.zh-CN.md — the mirrored section**

Same placement as Step 1:

```markdown
### Agent 事件面板

当某个标签页运行 Claude Code 时，右侧面板会实时列出对话事件——你的发言、Claude 的提问、
回合完成、选择提交——每条带时间戳。点击条目可将终端滚动回事件发生的位置。

面板需要一次性安装 Claude Code hooks：点击面板中的 **Install hooks**。clitab 会把五条
hook 注册合并进 `~/.claude/settings.json`（与 [CLAUDE_HOOKS.md](CLAUDE_HOOKS.md) 文档中
的内联命令完全一致）——你自己的 hooks 会原样保留，已有的手动 dashboard 配置会被就地
升级，首次安装会创建 `settings.json.bak` 备份，重复安装是安全的。事件按标签页保存在
内存中（最近 200 条），不落盘；只要对应位置还在终端回滚缓冲区里，跳转就有效。
```

- [ ] **Step 3: Update `CLAUDE_HOOKS.md` in place**

Keep its structure (intro / Setup / How it works / Legacy notes / Troubleshooting) and make these changes:

1. **Intro**: add one sentence — the same hooks also feed the agent event panel (conversation timeline with click-to-jump).
2. **New section after the intro: "One-click install (recommended)"** — open the panel in any tab and click **Install hooks**; clitab merges the five registrations below into `~/.claude/settings.json`, preserving foreign hooks, replacing any clitab-owned entries (recognised by the `]7777;` marker — including a previous manual setup), and writing a `settings.json.bak` on first install. Restart running Claude Code sessions to pick up changes.
3. **Setup section (manual)**: retitle to "Manual setup (alternative)" and replace the JSON block with the five commands **exactly as they appear in `claude_hooks.rs`** — the two changed/new ones are:
   - `UserPromptSubmit` now builds `{"e":"prompt","text":…}` (jq slices `.prompt` to 140 chars, with the bare-payload fallback when jq is missing);
   - new `PostToolUse` entry, matcher `AskUserQuestion`, building `{"e":"choice","text":…}` (Task 1's pinned jq expression).
   `PreToolUse`, `Stop`, `Notification` are unchanged.
4. **How it works**: keep the transport bullets (ancestor tty, `/dev/$t`, `; true`); extend the events bullet list with `text` on prompt and the new `choice` payload; add one bullet: because payloads ride the PTY byte stream, clitab knows each event's exact stream position — that is what makes the panel's click-to-jump land on the right terminal line.
5. **Legacy notes**: keep; add that installs predating the panel (prompt without `text`, no `PostToolUse`) keep working — the panel simply shows text-less prompt rows until the one-click install upgrades them.
6. **Troubleshooting**: keep items 1–3; add: "Panel shows rows but jumps do nothing — the target line was trimmed out of the scrollback (5000 lines) or the event predates the replay ring after a reload; this is expected."

- [ ] **Step 4: Update `CLAUDE.md`**

Surgical additions only (match the existing bullet style):

- Backend bullets: `agent_events.rs` (maps decoded OSC 7777 events to panel rows; `TEXT_LIMIT` 140, whitespace-flattened) and `claude_hooks.rs` (one-click install; ownership marker `]7777;`; merge never clobbers foreign hooks).
- `status.rs` bullet: mention the `text` field on prompt and the `choice` kind.
- `pty/registry.rs` bullet: mention the per-tab agent-event store (cap 200, 2 s duplicate guard).
- `lib.rs` command list: add `list_agent_events`, `claude_hooks_status`, `install_claude_hooks`.
- Events list (Data flow / invariants): add `agent-event`; note the invariant that it is emitted before the `pty-output` chunk containing its OSC (renderer anchors via a synchronous ref).
- Frontend bullets: `AgentEventPanel.tsx` and the marker-anchoring line in `Terminal.tsx`.
- Deliberate-choices section: `stop` never touches the title (extend the existing invariant sentence with the panel/`choice` fact: `choice` is panel-only, no dashboard transition).

- [ ] **Step 5: Verify the sync**

Re-read both README sections side by side: same facts, same order, same link. Diff the manual-setup JSON commands in `CLAUDE_HOOKS.md` against the five constants in `claude_hooks.rs` — they must be identical (that equality is what makes the manual setup upgradeable by the installer).

- [ ] **Step 6: Commit**

```bash
git add README.md README.zh-CN.md CLAUDE_HOOKS.md CLAUDE.md
git commit -m "docs: agent event panel + one-click hook install (READMEs, CLAUDE_HOOKS, CLAUDE.md)

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

Precondition for a meaningful test: start from a **manual** dashboard setup — put the current `CLAUDE_HOOKS.md` commands into a scratch `~/.claude/settings.json` (back up the real one first). In the app: open a tab, confirm the `☰` toggle appears at the terminal's top-right; open the panel — with only the manual (pre-panel) hooks, `hooksInstalled` is already true (the marker matches), so the empty state says "No events yet." Click **Install hooks** anyway and verify: the `UserPromptSubmit` command gained the `text` field, `PostToolUse` appeared, no duplicate groups, `settings.json.bak` was written (first install only), foreign hooks untouched. Re-click install: still no duplicates.

- [ ] **Step 3: Live conversation acceptance**

In the tab, run `claude` in a scratch directory. Check each behavior:

- Panel auto-opens when Claude Code takes the title.
- Send a prompt → a **You** row appears with your prompt text and a sane time.
- Let a turn finish → a **Claude done** row appears; the tab flashes if not focused; the existing dashboard still shows the turn duration. The title does NOT revert on Stop (it reverts on `claude-done` when you exit or the shell integration fires — shipped invariant).
- Trigger a permission prompt or idle notification → a **Claude asks** row appears with the message text; tab flashes when unfocused.
- Ask Claude to use AskUserQuestion (`请用 AskUserQuestion 工具问我一个单选题`) and answer → a **You chose** row appears naming your selection.
- Send a prompt longer than 140 chars → the row's text is truncated, no crash, dashboard unaffected.
- Scroll away, then click an early row → the terminal jumps back to roughly where that event happened (within a few lines: the anchor is the OSC's position, and ink repaints around it).
- Send ~50 more messages, scroll to top: rows whose lines were trimmed (scrollback is 5000) do nothing on click — no crash, terminal keeps working.

- [ ] **Step 4: Duplicate-hook acceptance**

Add one of the five registrations (e.g. `Stop`) to a scratch project's `.claude/settings.json` as well, run `claude` there, and finish a turn: exactly **one** new "Claude done" row (the registry's 2 s guard rejects the second fire), and the dashboard duration is still correct. Remove the project-level hook afterwards.

- [ ] **Step 5: Reload and multi-tab acceptance**

- With a conversation in the panel, reload the webview (dev: cmd-R in the webview / restart frontend): panel rows are restored; clicking a row whose position is still inside the 256 KB replay ring jumps correctly; older rows no-op silently.
- Open a second tab without Claude Code: no panel (toggle still available; its panel shows the empty state). Switch between tabs: each panel shows its own tab's events; the `☰`/auto-open rule follows the active tab.
- Close the Claude tab: its events are gone from state (reopen a tab, no stale rows).

- [ ] **Step 6: Record results and finish**

Note pass/fail per checklist item in the task report. Any failure: fix in a focused commit (`fix: ...` + attribution line) referencing the behavior, re-run the affected step. Restore the user's real `~/.claude/settings.json` from the Step-2 backup. When green, the branch is ready — follow superpowers:finishing-a-development-branch for merge/PR.

---

## Self-Review Notes (author, revision 2)

- **Rebase check:** every code step was re-derived from the current sources (osc.rs with `payload_len`/`flush_param`, status.rs with `Wire`/`non_empty`-style filtering, registry.rs with turn-state fields, session.rs `handle_status` with its flash/triage effects and title invariant, useTabManager with `StreamSink`, App/Terminal as shipped). No step references removed revision-1 machinery (OSC 9 channel, base64 script, salvage decoder, legacy-effect re-emission).
- **Spec coverage:** every revision-2 spec section maps to a task — wire protocol (1, 6), offsets (2), decode + mapping (3), store + dedup (4), session wiring (5), one-click install (6), renderer plumbing + restore (7), anchoring + jump (8), panel + auto-open (9), docs incl. CLAUDE.md (10), acceptance (11). No gaps found.
- **Placeholder scan:** all code steps carry full code. The single deliberately deferred value is Task 1's pinned choice jq expression, which Task 6's `CHOICE_CMD` comment explicitly instructs to fold in.
- **Type consistency:** `StatusEvent::Prompt{text}`/`Choice{text}` identical across Tasks 3/5; `panel_event -> Option<(AgentEventKind, String)>` across 3/5; `push_agent_event -> bool` across 4/5; `AgentEvent` fields `id/kind/text/time/seq` across 3/4/5/6/7; `agent-event` payload `{tab_id, event}` across 5/7; command names `list_agent_events`/`claude_hooks_status`/`install_claude_hooks` across 6/7; `OutputHandler (chunk, seq, isReplay?)` across 7/8; `eventsForTab` + `jumpRequest {tabId, eventId, nonce}` across 7/8/9; `OSC_MARKER "]7777;"` across 6/10.
- **Review Focus pins:** item 1 → Task 1 Step 5 (stop-the-line); item 2 → Task 4 `duplicate_within_window_is_rejected` + Task 5 emit-only-when-accepted; item 3 → Task 6 `merge_replaces_existing_manual_osc_entries` (fixture = shipped commands) + Task 11 Step 2; item 4 → Task 3 `text_truncated_to_limit_chars` / `control_chars_become_spaces` + existing status/osc tolerance tests; item 5 → Task 8 Step 3 guard + Task 11 Steps 3/5.
