# Agent Event Panel — Design Spec

Date: 2026-10-01
Status: approved in chat, pending spec review

## Goal

When a tab runs Claude Code and the user has a multi-turn conversation with it,
show a right-side panel listing the conversation events — user prompts, agent
questions, agent turn completion (Stop), and user choice submissions — each with
summary text and a timestamp. Clicking an entry scrolls the terminal to the
position where that event happened.

## Non-goals (v1)

- Agents other than Claude Code (architecture keeps the event source pluggable,
  but only Claude Code hooks are implemented).
- Permission allow/deny decisions (no direct hook; only AskUserQuestion
  selections are recorded).
- Persisting events to disk (in-memory per tab; survives webview reload, cleared
  when the tab closes).
- Jumping to events whose terminal lines were trimmed out of scrollback.

## Approach decision

Events travel **in-band**: Claude Code hooks run a small script that writes a
custom OSC sequence into the PTY stream itself. Alternatives considered and
rejected:

- **Local socket / side-channel** (hook script talks to clitab directly): no
  stream position, so click-to-jump could only guess alignment by timestamp.
- **Transcript JSONL watching**: richest data but zero terminal-position
  information and ambiguous session-to-tab mapping.

In-band OSC reuses the existing, proven `claude-done` path (hook → tty →
`OscParser` → Tauri event) and gives every event an exact byte-stream position
(`seq`), which is what makes jump anchoring reliable.

## End-to-end data flow

```
Claude Code fires a hook
  → hook script reads the hook's stdin JSON, truncates, base64-encodes,
    writes  OSC 9 ; clitab-agent ; <base64>  to the tty
  → PtySession reader thread: OscParser decodes the event AND its absolute
    byte offset in the stream
  → Rust: base64 decode → lenient JSON parse → AgentEvent
      ├─ stored in the per-tab event list in Registry (cap 200, in-memory)
      └─ emitted as `agent-event` { tab_id, event } (event carries seq)
  → renderer: useTabManager appends to the tab's event list; panel displays it
  → Terminal.tsx writes output chunks tagged with seq; when a write reaches an
    event's seq it registers an xterm marker at that exact position
  → clicking a panel entry switches tab if needed and scrolls to the marker
```

## Event model

```ts
type AgentEventKind =
  | 'user-prompt'      // user message        ← hook UserPromptSubmit (prompt text)
  | 'agent-question'   // agent asks / waits  ← hook Notification (message text)
  | 'agent-done'       // agent turn finished ← hook Stop
  | 'user-choice';     // user picked an option ← hook PostToolUse, matcher AskUserQuestion

interface AgentEvent {
  id: string;    // uuid, generated in Rust
  kind: AgentEventKind;
  text: string;  // summary, truncated to 140 chars (char-safe)
  time: number;  // unix ms, when Rust received the OSC
  seq: number;   // absolute PTY stream position where the OSC started
}
```

Event JSON keys stay snake_case (`tab_id`), matching the existing Tauri event
convention.

## Hook integration

### Hook script

POSIX sh, no jq/python dependency:

```sh
#!/bin/sh
payload=$(head -c 2800)                      # hook JSON on stdin, truncated
b64=$(printf '%s' "$payload" | base64)
osc=$(printf '\033]9;clitab-agent;%s\033\\' "$b64")
printf '%s' "$osc" > /dev/tty 2>/dev/null || printf '%s' "$osc"
```

- 2800 bytes keeps the base64 payload (~3.8 KB) under the parser's existing
  `MAX_OSC_LEN` of 4096.
- Prefers `/dev/tty`, falls back to stdout (the path the existing BEL hook
  demonstrably uses).
- Truncation can produce malformed JSON; the Rust decoder is lenient (below).
- The script is embedded in the binary via `include_str!` and written out at
  install time; no runtime dependency on the app bundle layout.

### Hook registrations (installed into `~/.claude/settings.json`)

| Hook event | Matcher | Produces |
|---|---|---|
| `UserPromptSubmit` | — | `user-prompt` |
| `Notification` | `.*` | `agent-question` |
| `Stop` | — | `agent-done` |
| `PostToolUse` | `AskUserQuestion` | `user-choice` |

One-click install is idempotent: clitab-owned entries are identified by the
script path inside their `command` string and replaced on reinstall; unrelated
user hooks are preserved untouched. Legacy `claude-done` / BEL hook entries set
up per the old `CLAUDE_HOOKS.md` are replaced as well — the Rust side
reproduces their effects (next section), so no behavior is lost.

### Preserving existing attention/title behavior

The hook script only forwards JSON; it does not emit legacy sequences. Instead,
when Rust decodes an event it also drives the existing behavior:

- `agent-question` → additionally emit `tab-flash` (replaces the old BEL flash).
- `agent-done` → additionally run the current `PromptReady` handling
  (`program_active = false`, `clear_program_title`, `tab-flash`, `prompt-ready`
  event), replacing the old `claude-done` OSC.

If a user still has the legacy hooks configured alongside the new ones, both
paths fire; the effects are idempotent (an extra flash/title-revert is
harmless).

### Risk to verify first (implementation step 1)

- That a Claude Code hook process reliably reaches the PTY via `/dev/tty` (or
  the stdout fallback). The existing BEL hook proves stdout works; the script
  covers both. If neither delivers for some hook type, that event kind degrades
  to absent — the panel still works for the others.
- The exact JSON shape of `tool_response` for a `PostToolUse` hook on
  `AskUserQuestion` (where the user's selection lives). Confirmed by probe in
  implementation step 1; the decoder's salvage path bounds the damage if the
  shape differs across Claude Code versions.

## Rust backend changes

### `osc.rs`

- New variant `OscEvent::AgentEvent(String)` — OSC 9 whose payload starts with
  `clitab-agent;` carries the base64 remainder. `claude-done` handling is
  unchanged.
- `parse()` returns `Vec<(OscEvent, u64)>`: the offset is the absolute stream
  position where the OSC sequence **started**. The parser counts every byte fed
  to it (one instance per session, fed from position 0), so its internal count
  matches `StreamState.position` without threading state through; OSC sequences
  split across reads still report the correct start offset.

### New module `agent_events.rs`

- `AgentEvent` struct (serde).
- `decode_agent_event(b64: &str) -> Option<(AgentEventKind, String)>`:
  base64 decode → strict `serde_json::from_slice` → map `hook_event_name` to a
  kind and extract text (`prompt` for UserPromptSubmit, `message` for
  Notification, selection from `tool_response` for AskUserQuestion PostToolUse,
  no text for Stop). On strict-parse failure (truncated JSON), fall back to
  string-search salvage for the event name and text field; on salvage failure,
  return `None` (drop the event). Pure function, unit-tested.
- Text truncated to 140 chars.

### `pty/registry.rs`

Per-tab `Vec<AgentEvent>` alongside the existing metadata (Registry is already
the lock that reader threads update directly and that survives webview reload):

- `push_agent_event(tab_id, event)` — capped at 200, oldest dropped first.
- `agent_events(tab_id) -> Vec<AgentEvent>`.
- Cleared when the tab is removed.

### `pty/session.rs`

`handle_osc` receives `(event, offset)`. For `AgentEvent`: build the full
`AgentEvent` (uuid + now + seq=offset), store in Registry, then
`app.emit("agent-event", {tab_id, event})`. This happens before the stream lock
is taken, same as today's OSC handling; the re-attach dedup ordering guarantee
(emit under the stream lock) is untouched.

### `lib.rs` — new commands

- `list_agent_events(tab_id) -> Vec<AgentEvent>` — panel restore after webview
  reload; returns the `TAB_GONE`-prefixed error for a vanished tab.
- `claude_hooks_status() -> bool` — whether clitab's hook entries exist in
  `~/.claude/settings.json`.
- `install_claude_hooks()` —
  1. write the embedded script to
     `~/Library/Application Support/clitab/hooks/clitab-hook.sh`, `chmod 0755`;
  2. merge hooks into `~/.claude/settings.json` via pure function
     `merge_hooks(settings: Value, script_path: &str) -> Result<Value>`
     (missing file → create; malformed JSON → error, file untouched;
     clitab entries replaced; other hooks preserved);
  3. write a `settings.json.bak` backup once, before the first modification.

The existing `claude-done` → `prompt-ready` / flash / title-revert code path
stays as-is (legacy hooks keep working); decoded `agent-done` /
`agent-question` events drive the same effects, as described under "Preserving
existing attention/title behavior".

## Frontend changes

### `types.ts`

Mirror `AgentEventKind`, `AgentEvent`, and the `agent-event` payload
(`{ tab_id, event }`).

### `hooks/useTabManager.ts` (still the only place that talks to Rust)

- New state `agentEvents: Record<tabId, AgentEvent[]>`; `listen('agent-event')`
  appends; `abandonTab` cleans up.
- On startup, next to the `list_tabs` snapshot, call `list_agent_events` per
  tab to restore the panel after a reload (`TAB_GONE` tolerated silently).
- New `installHooks()` and `hooksInstalled` passthroughs.
- **`OutputHandler` signature extension**: `(chunk, seq, isReplay?) => void`.
  Live chunks carry their existing `seq`; the replay chunk's seq is
  `replayEnd - bytes.length`. The attach/dedup protocol itself is unchanged.

### New `components/AgentEventPanel.tsx`

- One row per event: kind icon/label (prompt / question / done / choice),
  summary text, time as HH:MM:SS; auto-scrolls to the newest event.
- Click → `onJump(eventId)`.
- When hooks are not installed: empty state with an "Install Claude Code hooks"
  button calling `installHooks()`.
- Event text is rendered as data only (React escaping); never written to a
  terminal.

### `App.tsx`

- Panel sits to the right of `terminal-area`, showing the active tab's events.
- Visibility rule: `open = activeTab.hasClaudeTitle || manuallyOpened` —
  auto-opens when an agent is detected, auto-collapses when the program title
  reverts, manual toggle button always available.

### `components/Terminal.tsx` — anchoring and jumping (the core piece)

- New props: `agentEvents: AgentEvent[]` (this tab's), and
  `jumpRequest: { eventId, nonce } | null`.
- **Anchoring**: component keeps `markers: Map<eventId, IMarker>`. Each write
  is seq-tagged; for every unanchored event whose `seq` falls inside the chunk
  `[seq, seq+len)`, the write is split: bytes before the anchor →
  `term.registerMarker(0)` inside the write callback (fires when xterm has
  parsed to that point) → remaining bytes. Multiple anchors in one chunk are
  sorted and written as segments. Markers, not raw line numbers: they track
  scrollback trimming automatically and report `isDisposed` when their line is
  gone.
- Replay uses the same mechanism (its seq is now provided). For `redraw`
  replays the trailing home+ED2 does not touch scrollback, so markers recorded
  during the replay stay valid.
- **Jumping**: when `jumpRequest` changes, look up the marker →
  `term.scrollLines(marker.line - term.buffer.active.baseY)` → focus the
  terminal. If the marker is missing or disposed (line trimmed out of the 5000
  line scrollback, or event seq older than the replay ring after a reload), the
  jump is a silent no-op with a `console.debug` (acceptable for v1).
- Hidden tabs stay mounted (existing behavior), so markers survive tab
  switches; a webview reload rebuilds anchors for events still inside the
  replay ring.

## Error handling

- Hook script failures are silent (`|| true` paths); a hook must never block or
  break Claude Code.
- Malformed/truncated JSON: strict parse → salvage → drop (never panics; the
  OSC length cap and defensive parser already exist).
- Arbitrary programs can forge `clitab-agent` OSC sequences — same trust model
  as the existing OSC 9 `claude-done`; forged payloads that fail decode are
  dropped.
- `settings.json` unreadable/malformed at install → error surfaced in UI, file
  untouched, backup written only when a modification will actually happen.
- Events for closed tabs dropped; `list_agent_events` on a gone tab returns the
  `TAB_GONE` error which the renderer tolerates silently.

## Testing & acceptance

- Rust unit tests (`cargo test --lib`):
  - parser offsets, including OSC sequences split across `parse()` calls;
  - `decode_agent_event`: full JSON, truncated JSON (salvage), garbage (drop);
  - Registry event list: 200 cap, eviction order, cleanup on tab removal;
  - `merge_hooks`: fresh install, preserve foreign hooks, replace stale clitab
    entries, reject malformed settings JSON.
- Frontend: `npm run typecheck` only (no test runner exists; none added).
- Manual acceptance in `npm run tauri dev` with a real Claude Code session:
  verify each of the four event kinds appears with correct text/time, and that
  clicking each scrolls to the right terminal position. Implementation step 1
  is a standalone probe that hook output reaches the PTY.

## Documentation

- `README.md` and `README.zh-CN.md`: document the panel, the one-click hook
  install, and jump behavior (both files updated together, per repo rule).
- `CLAUDE_HOOKS.md`: rewritten around the one-click install; the manual
  configuration kept as an appendix.

## Development process

Implementation runs in a git worktree (user request), following the plan
produced by the writing-plans skill.
