# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

clitab is a tabbed terminal for macOS built for Claude Code sessions: each tab is a real PTY running the user's shell, titled by its working directory (OSC 7) or by the Claude Code session title (OSC 0/1/2), with attention flashing on BEL / OSC 9. Tauri 2 (Rust + portable-pty) backend, React 18 + xterm.js frontend.

## Commands

```bash
npm install
npm run tauri dev       # dev: vite + tauri (requires Rust toolchain)
npm run dev:log         # same, tee'd to /tmp/clitab-dev.log for bug reports
npm run typecheck       # tsc --noEmit (no JS test suite or linter exists)
npm run build           # typecheck + vite build
npm test                # cargo test --lib (Rust only: OSC parser, registry, shell integration, replay ring, menu ids)
npm run package:macos   # tauri build --bundles app + ad-hoc codesign (scripts/macos-sign.sh)
```

Run a single Rust test: `cargo test --manifest-path src-tauri/Cargo.toml --lib <test_name>`.

There is no frontend test runner and no linter; `npm run typecheck` is the only frontend check.

## Architecture

Two processes, one tab identity (`tab_id`, a UUID) shared across the IPC boundary.

### Rust backend (`src-tauri/src/`)

- `lib.rs` — Tauri commands (`create_tab`, `close_tab`, `list_tabs`, `pty_input`, `resize_pty`, `attach_stream`, `detach_tab`, `has_active_process`, `ack_tab_notice`) and app setup. Also defines `TAB_GONE` (`"clitab:tab-gone:"`): commands for a vanished tab return this prefixed error string so the renderer can tolerate it silently instead of pattern-matching prose. `src/types.ts` mirrors the constant — keep them in sync.
- `pty/manager.rs` — `TabManager` owns the session map. Tab *metadata* lives in `pty/registry.rs` (`Registry`), a separate lock that reader threads update directly; sessions never touch the manager's map. This keeps lock ordering trivial and lets `list_tabs` survive a webview reload with correct titles.
- `pty/session.rs` — one `PtySession` per tab: spawns the shell via portable-pty and runs three threads (output reader, attention watcher, exit watcher). The reader thread feeds an `OscParser`, pushes every byte into a 256 KB replay ring, and emits `pty-output` events **while holding the stream lock** — that ordering guarantee is what makes re-attach dedup (see below) correct; don't move the emit outside the lock.
- `osc.rs` — byte-level OSC parser (title / cwd / BEL / `claude-done` / OSC 7777 hook JSON), stateful across `parse()` calls so sequences split across PTY reads still decode. JSON payloads containing `;` survive because parameters are rejoined with `;` before interpretation. `looks_like_path()` classifies titles: path → cwd title, non-path → program (Claude) title, which drives the title-revert behavior.
- `status.rs` — decodes OSC 7777 payloads (`prompt` / `tool` / `stop` / `notify`, see `CLAUDE_HOOKS.md`) into `StatusEvent` and defines `TabStatus` / `Notice`; unknown or malformed payloads decode to `None` and are silently ignored. Per-tab status lives in registry `TabRecord` fields (epoch-ms timestamps so timers survive a webview reload). Invariant: `stop` never touches the title state — the `claude-done` title-revert path still owns it.
- `attention.rs` — side effects of the waiting-for-input triage queue: `tab-waiting` events, the Dock badge (count of waiting tabs), and macOS notifications via `UNUserNotificationCenter` (posted async; the request identifier is the tab id plus a per-episode suffix, because UN only presents identifiers it has not delivered before — the deprecated `NSUserNotificationCenter` path silently stopped delivering on modern macOS; a click routes through an objc2-defined delegate that splits the suffix off and emits `focus-tab`). The queue state itself is `Registry.waiting`: set at every `tab-flash` trigger, cleared **only** by `pty_input` — switching tabs does not clear it.
- `pty/shell_integration.rs` — per-tab rc-file wrappers (bash `--rcfile`, zsh `ZDOTDIR` with the user's `.zshenv`/`.zprofile`/`.zlogin` symlinked in) so cwd/turn-end reporting is injected without typing into the terminal. Files live in `$TMPDIR/clitab-<tab-id>` and are deleted in `PtySession::Drop`. Unsupported shells are left untouched.
- `menu.rs` — native menu owns all keyboard shortcuts (⌘T/⌘W/⌃Tab/⌘J/⌘1–9) so they work without webview focus; tab actions are forwarded to the renderer as a `menu-shortcut` event. Do not reimplement shortcuts as renderer keydown handlers.
- `services.rs` — the Finder "New clitab Tab Here" NSServices integration: pasteboard-text → path classification, the `ServiceState` cold-start handshake (requests during the 400 ms startup grace become the initial tab's cwd), and the objc2-defined `ClitabServices` provider (macOS-gated). Backend-created tabs are announced to the renderer as a `tab-created` event carrying the same `TabResponse` shape as `create_tab`.

Mutexes are locked via `pty::lock()`, which recovers from poisoning rather than panicking (a panic in a reader thread would silently kill a terminal).

### Frontend (`src/`)

- `hooks/useTabManager.ts` — all IPC and event handling; the only place that talks to Rust. `components/Terminal.tsx` renders one xterm instance per tab; hidden tabs stay mounted (`visibility: hidden`) so they remain measurable and keep their size.
- `lib/timeline.ts` — `TimelineTracker` derives the per-tab timeline (turn-start / notice / turn-end) from `tab-status` payload *transitions*; notice detection keys on `notice.at` changes, not null transitions (payloads re-send unchanged notices). Renderer-only by design: the history and its navigation targets die with the webview, and that is accepted. `lib/termRegistry.ts` maps tabId → xterm instance; `useTabManager` binds each timeline event to a terminal line with `registerMarker(0)` — markers track scrollback trimming (`onDispose` ⇒ the event grays out), and click-to-navigate is `scrollToLine(marker.line)` plus a 1.2 s `registerDecoration` highlight. The panel itself is `components/TimelinePanel.tsx` (constant right column, active tab only).
- `types.ts` — payload types mirroring the Rust side (Rust responses are `camelCase` via serde to match).

### Data flow / invariants

- **Byte transport is base64 in both directions** (PTY output events and input commands) because Tauri payloads are JSON; a number array would cost ~4× bandwidth.
- **Re-attach dedup:** every `pty-output` chunk carries `seq` (absolute stream position); `attach_stream` returns the ring plus `replayEnd`. The renderer queues live chunks during attach, then drops/trims anything the replay already covered. When touching attach/replay code, preserve this protocol on both sides.
- **Replay classification** (`Terminal.tsx` `classifyReplay`): a ring of repaint-style TUI output (many cursor-ups ⇒ Claude Code/ink) is replayed for scrollback then cleared with home+ED2; a ring ending in alt-screen (vim/less) is dropped entirely. Plain output replays as-is.
- **Events (Rust → renderer):** `pty-output`, `tab-title`, `tab-cwd`, `tab-flash`, `tab-status`, `tab-waiting`, `focus-tab`, `prompt-ready`, `tab-exit`, `menu-shortcut`, `tab-created`. `tab-status` carries the tab's full `{status, notice}` (replacement, not merge). `tab-exit` is also listened to inside Rust (`lib.rs`) so a shell that exits on its own is removed from the session map.
- The backend classifies whether a title is program-set and ships that verdict with the event; the renderer must not re-derive the heuristic (it would drift).

### Deliberate choices documented in code comments — read them before "fixing"

- **No WebGL renderer** (`Terminal.tsx`): one GL context per mounted tab exceeds WKWebView's budget and the context-lost recovery path crashes xterm. DOM renderer is intentional; `@xterm/addon-canvas` is the sanctioned fallback if output gets slow.
- **IME workaround** (`Terminal.tsx`): WKWebView delivers IME-committed punctuation before the keystroke's keydown (WebKit #25119), which xterm drops; local listeners recover exactly those commits, and the hidden textarea is cleared after commits to keep xterm's composition invariant.
- **Trackpad tap-to-drag filtering** (`Terminal.tsx`): synthesized buttonless mouse events are blocked from xterm's selection service by physical `buttons` state, not timing.
- **Shift+Enter** is hand-encoded as kitty CSI-u (`\x1b[13;2u`) because xterm implements neither modifyOtherKeys nor the kitty protocol, and Claude Code would otherwise submit on Shift+Enter.
- **CSP** (`tauri.conf.json`): `connect-src ipc: http://ipc.localhost` is required for `invoke()`/events; `style-src 'unsafe-inline'` is required by xterm.js. Only affects the bundled app (dev is served by Vite).
- **Capabilities**: `src-tauri/capabilities/default.json`'s `windows` list must match `app.windows[].label` in `tauri.conf.json`.
- **Signing**: releases are ad-hoc signed, not notarized (see `scripts/macos-sign.sh` header for why and when to switch to a Developer ID).

## Docs

- `README.md` / `README.zh-CN.md` — user-facing, kept in sync with each other; update both when features/shortcuts/install steps change.
- `CLAUDE_HOOKS.md` — the one-time Claude Code hook setup (OSC 7777 protocol) that powers the attention flash and the tab dashboard.
