# Agent hooks ↔ clitab integration

clitab understands a private OSC protocol (**OSC 7777**) that agent CLI hooks
use to turn each tab into a small dashboard: the tool currently running with a
live timer, the last turn's duration, and notification messages waiting for
you. The same hooks also drive the attention flash. Supported agents:

- **Claude Code** — hooks live in `~/.claude/settings.json`.
- **Qoder CLI** — hooks live in `~/.qoder/settings.json` (verified with
  `qodercli` 1.1.65). Its hook system mirrors Claude Code's: same event
  names, same JSON config shape, same stdin fields (`prompt` / `tool_name` /
  `tool_response` / `message`), same `bash -c` execution — and a hook's
  `$PPID` is the `qodercli` process itself, so the tty transport below works
  unchanged.

Other terminals (iTerm2, Terminal.app) silently ignore these sequences, so this
config is safe to keep in your global settings.

## Setup

[jq](https://jqlang.github.io/jq/) is recommended: it powers tool names,
notification text, the submitted-prompt text and the recorded answers in the
timeline. Without jq everything degrades gracefully — turn start/stop and
flashing still work, only the detail text is missing.

### Claude Code

Add this to `~/.claude/settings.json` (or a project's `.claude/settings.json`):

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

### Qoder CLI

Add this to `~/.qoder/settings.json` (or a project's `.qoder/settings.json`).
It is the same block as above except for the `Notification` matcher: Qoder
also emits notifications that need nobody's attention (`auth_success`,
`elicitation_response`, `elicitation_complete`), so the matcher admits only
the types that mean "waiting for you".

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
        "matcher": "permission_prompt|idle_prompt|elicitation_dialog",
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

The wiki manuals embed copy-paste setup prompts carrying the same JSON
([Manual](https://github.com/dongritengfei/clitab/wiki/Manual#what-the-hooks-add) /
[手册](https://github.com/dongritengfei/clitab/wiki/%E6%89%8B%E5%86%8C#hooks-%E5%A2%9E%E5%BC%BA%E7%9A%84%E9%83%A8%E5%88%86))
— keep those copies in sync when this file changes (the Qoder block needs its
own wiki prompt).

## How it works

- Each hook `printf`s one JSON payload wrapped in `ESC ] 7777 ; … ESC \` to
  **the tty device the agent CLI itself is running on**: hooks are spawned
  without a controlling terminal (`/dev/tty` fails) and their stdout is
  captured by the agent, so the only reliable transport is the parent's tty,
  found with `ps -o tty= -p $PPID` and written as `/dev/$t`. For both Claude
  Code and Qoder CLI, `$PPID` is the agent process itself.
- That tty *is* the clitab tab, so events reach the right tab without any
  tab id — and if the agent runs detached (no tty), the guard clause makes the
  hook a silent no-op.
- Events: `{"e":"prompt","msg":"…"}` turn start, with the submitted prompt for
  the timeline (msg omitted without jq) · `{"e":"tool","tool":"Bash"}` running
  a tool · `{"e":"answer","msg":"…"}` the user's choice in an AskUserQuestion
  dialog (PostToolUse, other tools emit nothing) · `{"e":"stop"}` turn end
  (clitab computes the duration) · `{"e":"notify","msg":"…"}` needs attention —
  flashes the tab and shows the message until you switch to it or type into
  it (answering the dialog is typing — no hook fires at that moment).
- Every command ends in `; true`: a failing hook must never block the agent.
  Missing jq produces an empty payload, which clitab ignores.

## Qoder CLI notes

- Hook firing, stdin field names and `$PPID` = `qodercli` were verified live
  on 1.1.65; Qoder's `Notification` payload carries the same `message` field
  plus a `notification_type` (which the matcher above filters on).
- Qoder's `AskUserQuestion` tool wraps answers in the same text as Claude
  Code ("Your questions have been answered: … You can now continue with these
  answers in mind"), so the answer jq above works unchanged.
- Qoder-specific events (`StopFailure`, `PermissionRequest`, `SessionStart`,
  `SubagentStop`, …) are deliberately not wired up. A turn that dies on an
  agent error is backstopped by clitab's idle reconcile, same as an
  interrupted Claude turn.
- **Mid-turn prompts never reach the timeline.** Verified live on 1.1.65:
  Qoder shows a prompt typed while a turn runs as `Queued (press ↑ to edit)`,
  but it fires no `UserPromptSubmit` for it — not at queue time, not later.
  The text is injected into the *running* turn's context and answered within
  the same turn, instead of being auto-submitted as a new turn after the stop
  (Claude Code fires the hook at queue time, which is what clitab's queued
  timeline row and the dashboard's prompt-queue modeling key on). With no
  hook event there is nothing to model — a protocol limitation, not a
  misconfiguration.
- The shell-integration title revert recognizes `qoder*` commands alongside
  `claude*`; the `claude-done` OSC 9 event name is protocol-internal and
  fires for both agents.

## Legacy notes

- The shell-integration `OSC 9;claude-done` title revert is unaffected and
  keeps working.
- The old `Notification` hook running `printf '\a'` (BEL flash) wrote to
  stdout — current Claude Code versions capture hook stdout instead of
  passing it to the terminal, so that setup is likely inert. The new
  `Notification` hook above replaces it.

## Troubleshooting

1. **Dashboard never appears** — test the protocol directly, without the
   agent: in a clitab tab run
   `printf '\033]7777;{"e":"tool","tool":"Test"}\033\\'`
   The tab's status line should read `⚙ Test · 0s`. If it does, the protocol
   works and the hook config is the problem — validate your settings JSON
   (`jq . ~/.claude/settings.json` / `jq . ~/.qoder/settings.json`).
2. **Timer/duration works but no tool names or notice text** — jq is missing;
   install it (`brew install jq`) or accept the degraded display.
3. **Manual printf works but hooks don't** — check the agent supports hooks
   (`claude --version` needs v1.0.24+; Qoder CLI verified on 1.1.65), and
   make sure the agent runs directly in the tab: the transport needs the
   agent's process to have the tab's tty
   (`ps -o tty= -p <agent-pid>` must not show `??`).
