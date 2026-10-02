# Agent Event Panel — Design Spec

Date: 2026-10-01 · Revised 2026-10-02 (revision 2)
Status: approved in chat; revision 2 rebases onto the shipped OSC 7777 work

## Revision 2 (2026-10-02)

The repository shipped the OSC 7777 hook dashboard (status.rs, registry turn
state, attention triage) after this spec was approved, and the probe verdict
(commit efd762d) proved that hook processes have **no controlling terminal**
(`/dev/tty` fails) and their stdout is captured by Claude Code — the only
reliable transport is the ancestor tty. Revision 2 therefore changes:

- Events ride the **existing OSC 7777 protocol**, extended with an optional
  `text` field and a new `choice` kind — not a new OSC 9 `clitab-agent`
  channel. The base64 + two-tier salvage decoder is gone: jq extracts and
  truncates text at the wire, Rust caps it authoritatively.
- Hook commands use the **ancestor-tty transport**, identical to the shipped
  dashboard hooks.
- No hook script file: the one-click installer writes the same inline commands
  `CLAUDE_HOOKS.md` documents, recognising (and upgrading) existing manual
  installs by the `]7777;` marker in their command strings.
- **No new attention/title effects.** The shipped `handle_status` already
  flashes and queues on `stop`/`notify`; panel events piggyback on the same
  decoded payload. The shipped invariant stands: `stop` never touches the
  title (title revert stays owned by `claude-done`).

Everything below is the revised, authoritative design.

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
- Panel rows for tool executions (they drive the existing dashboard only).

## Approach decision

Events travel **in-band**: Claude Code hooks printf OSC 7777 payloads into the
tab's PTY stream (the shipped dashboard transport). Alternatives considered and
rejected:

- **Local socket / side-channel** (hook talks to clitab directly): no stream
  position, so click-to-jump could only guess alignment by timestamp.
- **Transcript JSONL watching**: richest data but zero terminal-position
  information and ambiguous session-to-tab mapping.

In-band OSC reuses the shipped, proven path (hook → ancestor tty → `OscParser`
→ `status::decode`) and gives every event an exact byte-stream position (`seq`)
— that is what makes jump anchoring reliable.

## Wire protocol (OSC 7777 extensions)

| Hook event | Matcher | Payload | Dashboard effect (existing) | Panel row (new) |
|---|---|---|---|---|
| `UserPromptSubmit` | — | `{"e":"prompt","text":"…"}` | begin turn | `user-prompt` |
| `PreToolUse` | `""` | `{"e":"tool","tool":"Bash"}` | tool + timer | — |
| `Notification` | `""` | `{"e":"notify","msg":"…"}` | notice + flash | `agent-question` |
| `Stop` | — | `{"e":"stop"}` | end turn + flash | `agent-done` |
| `PostToolUse` | `AskUserQuestion` | `{"e":"choice","text":"…"}` | — | `user-choice` |

- `text` is a new optional field on `prompt` and the payload of the new
  `choice` kind. Old builds ignore unknown fields (serde forward-compat is
  already documented in `status.rs`), so a new hook config keeps working with
  an old clitab.
- jq slices text to 140 codepoints at the wire (`.[0:140]`); without jq the
  hook degrades to the bare payload (panel row without text) or, for
  `tool`/`choice`, sends nothing.
- Transport (probe verdict efd762d): every command discovers the ancestor tty
  with `t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' ')`, writes
  `printf '\033]7777;%s\033\\' "$j" > /dev/$t` guarded by
  `[ -n "$t" ] && [ "$t" != '??' ]`, and ends with `; true`.

## End-to-end data flow

```
Claude Code fires a hook
  → hook command reads the hook's stdin JSON, builds a small OSC 7777 payload
    with jq, writes it to the ancestor tty (/dev/$t)
  → PtySession reader thread: OscParser decodes the payload AND its absolute
    byte offset in the stream (new: parse() reports offsets)
  → status::decode → StatusEvent (prompt{text} / tool / stop / notify{msg} /
    choice{text})
      ├─ existing dashboard transitions + attention effects (unchanged)
      └─ agent_events::panel_event() maps it to a panel row
          → AgentEvent { id: uuid, kind, text (≤140 chars), time: now, seq: offset }
              ├─ stored in the per-tab event list in Registry (cap 200,
              │   duplicate-hook guard, in-memory)
              └─ emitted as `agent-event` { tab_id, event } — only when the
                  store accepted it (duplicates must not double-row the
                  renderer's live list)
  → renderer: useTabManager appends to the tab's event list; panel displays it
  → Terminal.tsx writes output chunks tagged with seq; when a write reaches an
    event's seq it registers an xterm marker at that exact position
  → clicking a panel entry scrolls to the marker
```

## Event model

```ts
type AgentEventKind =
  | 'user-prompt'      // user message         ← UserPromptSubmit (prompt text)
  | 'agent-question'   // agent asks / waits   ← Notification (message text)
  | 'agent-done'       // agent turn finished  ← Stop
  | 'user-choice';     // user picked an option ← PostToolUse/AskUserQuestion

interface AgentEvent {
  id: string;    // uuid, generated in Rust
  kind: AgentEventKind;   // kebab-case on the wire
  text: string;  // summary, ≤140 chars, whitespace flattened; may be empty
  time: number;  // unix ms, when Rust received the OSC
  seq: number;   // absolute PTY stream position where the OSC started
}
```

Event JSON keys stay snake_case (`tab_id`), matching the existing Tauri event
convention.

## One-click install

- `install_claude_hooks` merges five registrations (table above) into
  `~/.claude/settings.json` via the pure function
  `merge_hooks(settings: Value) -> Value`: missing file → create; malformed
  JSON → error, file untouched; a `settings.json.bak` backup is written once,
  before the first modification.
- Ownership marker: a hook group is clitab-owned when any of its commands
  contains `]7777;`. This recognises **both** previous one-click installs and
  the manual inline setup from `CLAUDE_HOOKS.md`, so installing upgrades an
  existing manual dashboard config in place. Foreign hooks are preserved.
- `claude_hooks_status` reports whether any owned entries exist (drives the
  panel's "Install hooks" empty state).
- No script file, no app-data-dir dependency: settings.json is self-contained.

## Rust backend changes

### `osc.rs`

- `parse()` returns `Vec<(OscEvent, u64)>`: the offset is the absolute stream
  position where the OSC sequence **started** (for a standalone BEL, the BEL's
  position). No enum changes — the payload still arrives as `OscEvent::Clitab`.
  The parser counts every byte fed to it (one instance per session, fed from
  position 0), so its internal count matches `StreamState.position` without
  threading state through; sequences split across reads still report the
  correct start offset.

### `status.rs`

- `StatusEvent::Prompt` gains `text: Option<String>`; new variant
  `Choice { text: Option<String> }` decodes `{"e":"choice"}`. Empty strings
  filter to `None`, like tool names today. Malformed/unknown payloads keep
  decoding to `None` (silently ignored).

### New module `agent_events.rs` (small)

- `AgentEventKind` (serde kebab-case), `AgentEvent`, `TEXT_LIMIT = 140`.
- `panel_event(&StatusEvent) -> Option<(AgentEventKind, String)>`: prompt →
  user-prompt, notify → agent-question, stop → agent-done (empty text), choice
  → user-choice, tool → `None`. Text is whitespace-flattened (panel rows are
  single-line) and truncated to 140 chars, char-safe for CJK.

### `pty/registry.rs`

- `agent_events: Mutex<HashMap<String, VecDeque<AgentEvent>>>` alongside the
  existing `tabs` lock (Registry already survives webview reloads).
- `push_agent_event(tab_id, event) -> bool`: capped at 200 (oldest evicted);
  **duplicate guard** — an event with the same kind and text as the last one,
  within 2 s, is rejected (hooks fire once per settings level; the dashboard's
  duplicate-Stop guard proves this happens). Returns false for unknown tabs
  and for rejected duplicates.
- `agent_events(tab_id) -> Vec<AgentEvent>`; `remove` clears the tab's events.
  Lock order: `tabs` then `agent_events` (same as `remove`).

### `pty/session.rs`

- `handle_osc` / `handle_status` receive the offset. Inside `handle_status`,
  after the existing transitions: map via `panel_event`, build the
  `AgentEvent` (uuid + now + seq=offset), push, and emit `agent-event` only
  when the push was accepted. All of this happens before the stream lock is
  taken (as today's OSC handling); the re-attach dedup ordering guarantee
  (emit under the stream lock) is untouched.
- `Choice` gets no dashboard transition and no `tab-status` re-emit.
- No changes to attention/title behavior anywhere: `stop`/`notify` already
  flash and queue via the shipped code, and `stop` still never touches the
  title.

### `lib.rs` — new commands

- `list_agent_events(tab_id) -> Vec<AgentEvent>` — panel restore after webview
  reload; `TAB_GONE`-prefixed error for a vanished tab (via `TabManager`
  passthrough).
- `claude_hooks_status() -> bool`.
- `install_claude_hooks() -> Result<(), String>`.

## Frontend changes

### `types.ts`

Mirror `AgentEventKind`, `AgentEvent`, and the `agent-event` payload
(`{ tab_id, event }`).

### `hooks/useTabManager.ts` (still the only place that talks to Rust)

- New state `agentEvents: Record<tabId, AgentEvent[]>` **plus a synchronous
  `Map` ref mirror**: the `agent-event` Tauri event is emitted *before* the
  `pty-output` chunk containing its OSC bytes, and React state updates are
  batched — anchoring that read state could miss the chunk. `eventsForTab()`
  reads the ref; the state mirror exists only to re-render the panel.
- `listen('agent-event')` appends to both; `abandonTab` cleans up both.
- On startup, restore via `list_agent_events` per tab **before** `setTabs`, so
  mounting Terminals see the events while their replay is written
  (`TAB_GONE` tolerated silently).
- New `installHooks()` / `hooksInstalled` passthroughs.
- **`OutputHandler` signature extension**: `(chunk, seq, isReplay?) => void`.
  Live chunks already carry `seq` in the internal `StreamSink`; the replay
  chunk's seq is `replayEnd - bytes.length`. The attach/dedup protocol itself
  is unchanged.

### New `components/AgentEventPanel.tsx`

- One row per event: kind label (You / Claude asks / Claude done / You chose),
  summary text, time as HH:MM:SS; auto-scrolls to the newest event.
- Click → `onJump(eventId)`.
- When hooks are not installed: empty state with an "Install hooks" button
  calling `installHooks()`.
- Event text is rendered as data only (React escaping); never written to a
  terminal.

### `App.tsx`

- Panel sits to the right of `terminal-area`, showing the active tab's events.
- Visibility rule: `open = activeTab.hasClaudeTitle || manuallyOpened` —
  auto-opens when an agent is detected, auto-collapses when the program title
  reverts; manual toggle always available; a manual choice holds until the
  next tab switch.

### `components/Terminal.tsx` — anchoring and jumping (the core piece)

- New optional props: `eventsForTab?: (tabId) => AgentEvent[]` (synchronous
  reader) and `jumpRequest?: { tabId, eventId, nonce } | null`.
- **Anchoring**: component keeps `markersRef: Map<eventId, IMarker>`. Each
  write is seq-tagged; for every unanchored event whose `seq` falls inside the
  chunk `[seq, seq+len)`, the write is split: bytes before the anchor →
  zero-length `term.write` whose callback runs `term.registerMarker(0)` (fires
  when xterm has parsed to that point) → remaining bytes. Multiple anchors in
  one chunk are sorted and written as segments. Markers, not raw line numbers:
  they track scrollback trimming automatically and report `isDisposed` when
  their line is gone.
- Replay uses the same mechanism (its seq is now provided). For `redraw`
  replays the trailing home+ED2 does not touch scrollback, so markers recorded
  during the replay stay valid. `alt-screen` replays are dropped entirely;
  events in that range stay unanchored (jumps become no-ops — acceptable).
- **Jumping**: when `jumpRequest` changes (and its `tabId` matches), look up
  the marker → `term.scrollLines(marker.line - term.buffer.active.baseY)` →
  focus. Missing or disposed marker (trimmed out of the 5000-line scrollback,
  or event seq older than the replay ring after a reload) → silent no-op with
  a `console.debug`.
- Hidden tabs stay mounted (existing behavior), so markers survive tab
  switches; a webview reload rebuilds anchors for events still inside the
  replay ring.

## Error handling

- Hook failures are silent (`; true` tails); a hook must never block or break
  Claude Code. Missing jq degrades to text-less events (or no event for
  tool/choice), matching the shipped dashboard's degradation.
- Malformed/forged OSC 7777 payloads decode to `None` and are dropped
  (existing `status::decode` tolerance); payloads over `MAX_OSC_LEN` (4096)
  are dropped by the existing parser cap — jq's 140-codepoint slice keeps
  wire payloads far below it.
- Duplicate hook fires (per settings level) are rejected by the registry's
  dedup guard, so neither the live list nor the restored list double-rows.
- `settings.json` unreadable/malformed at install → error surfaced in UI, file
  untouched, backup written only when a modification will actually happen.
- Events for closed tabs dropped; `list_agent_events` on a gone tab returns the
  `TAB_GONE` error, which the renderer tolerates silently.

## Testing & acceptance

- Rust unit tests (`cargo test --lib`):
  - parser offsets, including sequences split across `parse()` calls and
    nested-ESC resync;
  - `status::decode`: prompt/choice text, empty-text filtering, forward-compat;
  - `panel_event`: each mapping, tool exclusion, truncation (CJK), whitespace
    flattening;
  - Registry store: order, 200 cap + eviction, dedup window, cleanup on tab
    removal, unknown-tab no-ops;
  - `merge_hooks`: fresh install, preserve foreign hooks, **replace the exact
    manual commands from `CLAUDE_HOOKS.md`** (upgrade path), normalize broken
    shapes, idempotent reinstall.
- Frontend: `npm run typecheck` only (no test runner exists; none added).
- Manual acceptance in `npm run tauri dev` with a real Claude Code session:
  verify each of the four event kinds appears with correct text/time, that
  clicking each scrolls to the right terminal position, reload restore,
  multi-tab isolation, and the manual-install upgrade path.

## Documentation

- `README.md` and `README.zh-CN.md`: document the panel, the one-click hook
  install, and jump behavior (both files updated together, per repo rule).
- `CLAUDE_HOOKS.md`: updated in place — one-click install section, the manual
  JSON block extended to the five text-carrying commands, panel event table.
- `CLAUDE.md`: architecture bullets for `agent_events.rs` / `claude_hooks.rs`,
  the new commands, and the `agent-event` event.

## Development process

Implementation runs in a git worktree (user request), executed via
superpowers:subagent-driven-development against the plan
(`docs/superpowers/plans/2026-10-01-agent-event-panel.md`, revision 2).
