# clitab

A terminal tab manager for Claude Code sessions: every tab is a real PTY running
your shell, named after the working directory — or after the session, once
Claude Code gives it a title.

Built with [Tauri 2](https://tauri.app) (Rust backend) and
[xterm.js](https://xtermjs.org) (renderer).

## Development

```bash
npm install
npm run tauri dev     # vite + tauri dev build
npm run dev:log       # the same, tee'd to /tmp/clitab-dev.log for reporting
npm run tauri build   # release bundle
```

Checks and tests:

```bash
npm run typecheck   # tsc --noEmit
npm run build       # typecheck + vite build
npm test            # cargo test --lib: OSC parser, registry, shell integration
```

## Security

- **CSP** is configured in `tauri.conf.json`. It only reaches the *bundled* app:
  in dev the page is served by Vite, and Tauri applies the policy to the assets
  it embeds itself. Two entries are load-bearing — `connect-src ipc:
  http://ipc.localhost` is how `invoke()` and events reach Rust, and
  `style-src 'unsafe-inline'` is required because xterm.js builds `<style>`
  elements at runtime. If IPC stops working, check the first one.
- **Shell integration files** go to a per-tab directory under `$TMPDIR`
  (`clitab-<tab-id>`), never a fixed shared path, and are deleted with the
  session.
- **Capabilities** live in `src-tauri/capabilities/default.json`; the `windows`
  list there must match `app.windows[].label` in the config.

## Shortcuts

Defined in the native menu (`src-tauri/src/menu.rs`), so they work even when the
terminal does not have keyboard focus.

| Key | Action |
| --- | --- |
| `⌘T` | New tab |
| `⌘W` | Close tab (asks first if a process is still running) |
| `⌃Tab` / `⌃⇧Tab` | Next / previous tab |
| `⌘1` … `⌘8` | Jump to tab |
| `⌘9` | Jump to the last tab |

## How it fits together

```
shell (PTY child)
  └─ output bytes ──► pty::session  ── reader thread ──┬─► OSC parser ─► registry + events
                                                       └─► `pty-output` event ─► xterm.js
keyboard / paste ──► xterm.js ─► `pty_input` command ─► PTY writer
```

- `pty::session` — one PTY plus three threads per tab: a reader (output + OSC),
  an idle watcher (attention flash) and an exit watcher (liveness).
- `pty::registry` — the authoritative tab list. Titles and working directories
  are updated here by the reader threads, so `list_tabs` still reports the right
  thing after a renderer reload.
- `pty::manager` — owns the sessions and maps `tab_id` to them.
- `osc` — incremental parser for OSC 0/1/2 (title), OSC 7 (cwd), OSC 9
  (notification) and bare BEL.

### Commands (renderer → Rust)

| Command | Purpose |
| --- | --- |
| `create_tab` / `close_tab` / `list_tabs` | tab lifecycle |
| `pty_input(tab_id, data)` | keystrokes, base64 |
| `resize_pty(tab_id, rows, cols)` | sync the PTY window size |
| `attach_stream(tab_id)` | replay what the view missed, then stream live |
| `detach_tab(tab_id)` | stop streaming to an unmounted view |
| `has_active_process(tab_id)` | confirm-before-close |

### Events (Rust → renderer)

| Event | Payload | Meaning |
| --- | --- | --- |
| `pty-output` | `{ tab_id, data }` | PTY bytes, base64 (Tauri payloads are JSON) |
| `tab-title` / `tab-cwd` | `{ tab_id, title \| cwd }` | OSC 0/1/2 and OSC 7 |
| `tab-flash` / `prompt-ready` | `{ tab_id }` | needs attention / turn finished |
| `tab-exit` | `{ tab_id, code }` | shell exited; the tab is dropped |
| `menu-shortcut` | `{ id }` | native menu accelerator |

The backend keeps a **256 KB replay ring per tab** and hands it over whenever a
terminal view attaches (`attach_stream`). That closes the race between spawning
the process and subscribing the listener — the shell's first prompt is never
lost — and it also means a webview reload rebuilds the screen instead of leaving
a blank terminal.

One caveat: the ring trims at a byte boundary, so after it has wrapped the
*oldest* line of a replay can start in the middle of an escape sequence and show
a few stray glyphs. Everything from the next full line onward is exact.

### Why the DOM renderer, not WebGL

Hidden tabs use `visibility: hidden` rather than `display: none` so they stay
measurable and their buffers stay warm. That means every tab keeps a mounted
terminal, so a WebGL renderer would mean a GL context per tab — and WKWebView
reclaims contexts once you have several. The recovery path for a lost context is
`WebglAddon.dispose()`, whose teardown writes a replacement renderer into a
`RenderService` that may already be disposed; `RenderService.dimensions` is an
unguarded `this._renderer.value.dimensions`, so the next scroll tick throws
`undefined is not an object (evaluating 'this._renderer.value.dimensions')`.

The DOM renderer has no context budget and no renderer swap. If output ever
becomes too slow for it, `@xterm/addon-canvas` is the upgrade: a 2D canvas
renderer, still no GL contexts.

## Shell integration

To know *which* directory a tab is in, clitab has to be in the shell's startup
path. Instead of typing a command into the terminal (which is visible, clobbers
input, and does nothing if the shell is busy), each tab gets its own rc file:

- **bash**: `bash --rcfile <tmp>/bashrc`, which sources your `~/.bashrc` first.
- **zsh**: `ZDOTDIR=<tmp>`, where the wrapper `.zshrc` sources your own and
  `.zshenv` / `.zprofile` / `.zlogin` are linked through unchanged.
- **anything else**: launched untouched, no integration.

The wrapper directory lives in the system temp dir and is deleted when the tab
closes. `$CLITAB_SHELL_INTEGRATION=1` is set in the child environment if you
want to branch on it in your own rc file.

## Claude Code flash

See [CLAUDE_HOOKS.md](./CLAUDE_HOOKS.md) for the hook configuration that makes
Claude Code ring the tab (`printf '\a'` → BEL → flash) when it needs input.
