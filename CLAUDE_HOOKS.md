# Claude Code ↔ clitab integration

clitab understands a private OSC protocol (**OSC 7777**) that Claude Code hooks
use to turn each tab into a small dashboard: the tool currently running with a
live timer, the last turn's duration, and notification messages waiting for
you. The same hooks also drive the attention flash. Other terminals (iTerm2,
Terminal.app) silently ignore these sequences, so this config is safe to keep
in your global settings.

## Setup

[jq](https://jqlang.github.io/jq/) is recommended: it powers tool names and
notification text. Without jq everything degrades gracefully — turn start/stop
and flashing still work, only the detail text is missing.

Add this to `~/.claude/settings.json` (or a project's `.claude/settings.json`):

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "t=$(ps -o tty= -p $PPID 2>/dev/null | tr -d ' '); [ -n \"$t\" ] && [ \"$t\" != '??' ] && printf '\\033]7777;{\"e\":\"prompt\"}\\033\\\\' > /dev/$t 2>/dev/null; true"
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

## How it works

- Each hook `printf`s one JSON payload wrapped in `ESC ] 7777 ; … ESC \` to
  **the tty device Claude Code itself is running on**: hooks are spawned
  without a controlling terminal (`/dev/tty` fails) and their stdout is
  captured by Claude Code, so the only reliable transport is the parent's
  tty, found with `ps -o tty= -p $PPID` and written as `/dev/$t`.
- That tty *is* the clitab tab, so events reach the right tab without any
  tab id — and if Claude runs detached (no tty), the guard clause makes the
  hook a silent no-op.
- Events: `{"e":"prompt"}` turn start · `{"e":"tool","tool":"Bash"}` running a
  tool · `{"e":"stop"}` turn end (clitab computes the duration) ·
  `{"e":"notify","msg":"…"}` needs attention — flashes the tab and shows the
  message until you switch to it.
- Every command ends in `; true`: a failing hook must never block Claude
  Code. Missing jq produces an empty payload, which clitab ignores.

## Legacy notes

- The shell-integration `OSC 9;claude-done` title revert is unaffected and
  keeps working.
- The old `Notification` hook running `printf '\a'` (BEL flash) wrote to
  stdout — current Claude Code versions capture hook stdout instead of
  passing it to the terminal, so that setup is likely inert. The new
  `Notification` hook above replaces it.

## Troubleshooting

1. **Dashboard never appears** — test the protocol directly, without Claude:
   in a clitab tab run
   `printf '\033]7777;{"e":"tool","tool":"Test"}\033\\'`
   The tab's third line should read `⚙ Test · 0s`. If it does, the protocol
   works and the hook config is the problem — validate your settings JSON
   (`jq . ~/.claude/settings.json`).
2. **Timer/duration works but no tool names or notice text** — jq is missing;
   install it (`brew install jq`) or accept the degraded display.
3. **Manual printf works but hooks don't** — check `claude --version`
   (hooks need v1.0.24+), and make sure Claude runs directly in the tab:
   the transport needs Claude's process to have the tab's tty
   (`ps -o tty= -p <claude-pid>` must not show `??`).
