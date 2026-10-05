# clitab

[中文 README](README.zh-CN.md) · 📖 [User Manual (Wiki)](https://github.com/dongritengfei/clitab/wiki/Manual)

A tabbed terminal built for Claude Code sessions: every tab is a real PTY
running your shell, named after its working directory — or after the session,
once Claude Code gives it a title. Tabs flash when Claude Code needs your
attention, so a row of parallel agents stays glanceable.

Built with [Tauri 2](https://tauri.app) (Rust + portable-pty) and
[xterm.js](https://xtermjs.org).

## Screenshots

![Three tabs, each a live shell named after its working directory](docs/screenshots/tabs.png)

Every tab is a real shell named after its working directory — three
projects side by side in one window.

![A Claude Code session running in a tab](docs/screenshots/session.png)

Run `claude` in any tab: while the session runs, the tab switches from
the directory to the session's own title, and reverts when the turn ends.

![A background tab lit up because its session needs attention](docs/screenshots/attention.png)

When a session needs you, its tab flashes (BEL / OSC 9) — a row of
parallel agents stays glanceable from whichever tab you are in.

## Features

- **Real PTY tabs** — each tab spawns your `$SHELL` through a per-tab
  pseudo-terminal; tabs stay alive while hidden and keep their size. A new tab
  opens in the current tab's working directory.
- **Open from Finder** — right-click a folder in Finder and choose
  Services → "New clitab Tab Here" to open a tab in that directory. Works on
  files (opens the containing folder) and on the Finder window background
  (opens the window's folder), whether or not clitab is running.
- **Automatic tab names** — the title is the working directory (reported by a
  shell-integration hook on every prompt), and switches to the session name
  when Claude Code sets a terminal title. It reverts to the directory when the
  assistant's turn ends.
- **Attention flash** — a tab flashes when Claude Code asks for input or sends
  a notification (BEL / OSC 9). See [CLAUDE_HOOKS.md](CLAUDE_HOOKS.md) for the
  one-time hook setup.
- **Attention triage** — a session that needs input joins a waiting queue:
  the Dock badge counts them, `⌘J` jumps to the next one, and while clitab
  is in the background a macOS notification announces each; clicking a
  notification goes straight to that tab. A tab leaves the queue when you
  type in it — switching alone does not.
- **Tab dashboard** — with the optional Claude Code hooks (`CLAUDE_HOOKS.md`),
  each tab shows what its session is doing: the running tool with a live
  timer, the last turn's duration, and notifications waiting for you. Built on
  a private OSC 7777 protocol other terminals simply ignore.
- **Session timeline** — the right-hand panel logs what you sent in the
  active tab's Claude Code session: each prompt you submitted (clamped to two
  lines with the full text on hover) and each answer you picked in Claude's
  multiple-choice questions. Click an entry to scroll the terminal to that
  point; entries whose output has scrolled out of the 5000-line history are
  grayed out. Powered by the same hooks as the dashboard.
- **Terminal search** — ⌘F finds text in the active tab's screen and
  scrollback: matches highlight as you type, Enter / Shift+Enter cycle
  through them, Esc returns the caret to the shell.
- **Desktop-grade shortcuts** — a native menu drives ⌘T / ⌘W / ⌃Tab / ⌘1–9,
  so they work even when the terminal does not have keyboard focus. Closing a
  tab with a running process asks first.
- **Click a tab, type immediately** — activating a tab moves the caret into
  its terminal; no second click needed.
- **Survives window reloads** — the most recent 256 KB of each tab's output is
  kept in a ring buffer and replayed on re-attach. Repaint-style TUI output
  (Claude Code, vim, …) gets a clean start instead of a ghosted replay.
- **Shell integration without typing into your terminal** — bash gets a
  `--rcfile` wrapper, zsh a `ZDOTDIR` wrapper that links your own startup
  files, so your config loads untouched. Per-tab files live in `$TMPDIR` and
  are removed with the session.
- 5000-line scrollback, ⌘C/⌘V/⌘A copy-paste, trackpad tap-to-drag does not
  accidentally select text.

## Install (macOS)

Download the `.dmg` for your chip from [Releases](../../releases) —
`aarch64` for Apple Silicon (M1–M4), `x64` for Intel — and drag **clitab**
into Applications.

Release builds are ad-hoc signed but **not notarized**, and downloaded
files carry the quarantine attribute, so on a fresh download macOS
Gatekeeper intervenes twice — once for the `.dmg`, once for the app.
Both are one-time **Open Anyway** clicks:

1. Double-click the `.dmg`; macOS refuses with a dialog saying the file
   "cannot be opened" / does not have permission to open it. Click
   **OK**.
2. Open **System Settings → Privacy & Security** and scroll to the
   **Security** section: "*clitab_….dmg* was blocked to protect
   your Mac" with an **Open Anyway** button. Click it and confirm with
   your password (or Touch ID); the disk image then mounts normally.
3. Drag **clitab** into Applications. The first time you open it, the
   app itself is blocked the same way: back in **Privacy & Security**,
   click **Open Anyway** next to "*clitab* was blocked…". One-time —
   later launches open normally.

Terminal shortcut (skips both prompts): clear the quarantine flag on the
downloaded `.dmg` *before* opening it; apps copied from it then inherit
nothing:

```bash
xattr -d com.apple.quarantine ~/Downloads/clitab_*.dmg
```

(On macOS 15 Sequoia and later, right-click → Open no longer bypasses
Gatekeeper for a signature like this one — use **Open Anyway** above.)

Building from source (`npm run package:macos`) needs no such step: the
bundle is ad-hoc signed and never quarantined, so it opens directly.

Both builds are native; nothing needs Rosetta.

## Claude Code integration

Run `claude` in any tab like you would in a normal terminal — clitab picks up
the session title from the escape sequences Claude Code already emits.

The attention flash, the tab dashboard and the session timeline are powered by
Claude Code hooks:
add the configuration from [CLAUDE_HOOKS.md](CLAUDE_HOOKS.md) to your Claude
Code settings (`~/.claude/settings.json`). That page has the details and
troubleshooting.

Or skip the manual edit entirely: paste the prompt below into Claude Code
running in a clitab tab and press Enter — it merges the hooks into
`~/.claude/settings.json` for you and validates the result. Restart Claude Code
when it finishes.

````text
Merge the clitab terminal's Claude Code hooks config into my ~/.claude/settings.json:
- Read the existing file first (create it if missing) and preserve everything already in it; for the five hook events (UserPromptSubmit / PreToolUse / PostToolUse / Stop / Notification) that already have entries, append the entries below to the existing arrays instead of replacing them; if one of the commands below is already present, skip it rather than adding a duplicate;
- Merge the command strings verbatim — do not alter any escaping, quoting or whitespace;
- When done, validate the file with `jq . ~/.claude/settings.json` (or `python3 -m json.tool` if jq is unavailable);
- Check for jq with `command -v jq`; if it is missing, tell me `brew install jq` is optional (without it the dashboard just omits tool names and notification text; timers and flashing still work);
- Finally, remind me to restart Claude Code so the hooks take effect.

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); j=$(jq -c '{e:\"prompt\",msg:.prompt}' 2>/dev/null || printf '{\"e\":\"prompt\"}'); [ -n \"$t\" ] && [ \"$t\" != '??' ] && printf '\\033]7777;%s\\033\\\\' \"$j\" > /dev/$t 2>/dev/null; true"
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); j=$(jq -c '{e:\"tool\",tool:.tool_name}' 2>/dev/null); [ -n \"$t\" ] && [ \"$t\" != '??' ] && [ -n \"$j\" ] && printf '\\033]7777;%s\\033\\\\' \"$j\" > /dev/$t 2>/dev/null; true"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); j=$(jq -c 'select(.tool_name==\"AskUserQuestion\") | {e:\"answer\",msg:(.tool_response|if type==\"string\" then (sub(\"^Your questions have been answered: \"; \"\") | sub(\"[.]? You can now continue with these answers in mind[.]?$\"; \"\")) elif type==\"object\" and (.answers|type)==\"object\" then (.answers|to_entries|map((.key|@json) + \"=\" + (.value|if type==\"array\" then join(\"/\") else tostring end|@json))|join(\", \")) else tostring end)}' 2>/dev/null); [ -n \"$t\" ] && [ \"$t\" != '??' ] && [ -n \"$j\" ] && printf '\\033]7777;%s\\033\\\\' \"$j\" > /dev/$t 2>/dev/null; true"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); [ -n \"$t\" ] && [ \"$t\" != '??' ] && printf '\\033]7777;{\"e\":\"stop\"}\\033\\\\' > /dev/$t 2>/dev/null; true"
          }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); j=$(jq -c '{e:\"notify\",msg:.message}' 2>/dev/null || printf '{\"e\":\"notify\"}'); [ -n \"$t\" ] && [ \"$t\" != '??' ] && printf '\\033]7777;%s\\033\\\\' \"$j\" > /dev/$t 2>/dev/null; true"
          }
        ]
      }
    ]
  }
}
```
````

## Shortcuts

The tab shortcuts are defined in the native menu (`src-tauri/src/menu.rs`),
so they work even when the terminal does not have keyboard focus. `⌘C` /
`⌘V` / `⌘A` are the standard macOS Edit menu items and act on the current
text selection.

| Key | Action |
| --- | --- |
| `⌘T` | New tab |
| `⌘W` | Close tab (asks first if a process is still running) |
| `⌃Tab` / `⌃⇧Tab` | Next / previous tab |
| `⌘J` | Jump to the next tab waiting for input |
| `⌘1` … `⌘8` | Jump to tab 1–8 |
| `⌘9` | Jump to the last tab |
| `⌘F` | Search the active terminal (Enter / ⇧Enter cycle matches, Esc closes) |
| `⌘C` / `⌘V` / `⌘A` | Copy / paste / select all |

> Notifications appear only while clitab is in the background. If they never
> show up, check System Settings → Notifications → clitab.

In the tab list, arrow keys / Home / End move between tabs.

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

- **CSP** is configured in `tauri.conf.json`. It only reaches the *bundled*
  app: in dev the page is served by Vite, and Tauri applies the policy to the
  assets it embeds itself. Two entries are load-bearing —
  `connect-src ipc: http://ipc.localhost` is how `invoke()` and events reach
  Rust, and `style-src 'unsafe-inline'` is required because xterm.js builds
  `<style>` elements at runtime. If IPC stops working, check the first one.
- **Shell integration files** go to a per-tab directory under `$TMPDIR`
  (`clitab-<tab-id>`), never a fixed shared path, and are deleted with the
  session.
- **Capabilities** live in `src-tauri/capabilities/default.json`; the `windows`
  list there must match `app.windows[].label` in the config.

## License

MIT — see [LICENSE](LICENSE).
