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

Other terminals (iTerm2, Terminal.app) silently ignore these sequences, so the
config is safe to keep in your global settings.

**This file is the developer-facing protocol reference.** The copy-paste
installation prompts (both agents) and the user-facing troubleshooting live on
the wiki:
[Agent Setup](https://github.com/dongritengfei/clitab/wiki/Agent-Setup) ·
[Troubleshooting](https://github.com/dongritengfei/clitab/wiki/Troubleshooting).
When a hook command changes, update the wiki's `Agent-Setup.md` and
`Agent-Setup.zh.md` — the two JSON blocks must stay byte-identical.

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
- [jq](https://jqlang.github.io/jq/) powers tool names, notification text,
  submitted-prompt text and recorded answers. Without jq everything degrades
  gracefully — turn start/stop and flashing still work, only the detail text
  is missing.
- Every command ends in `; true`: a failing hook must never block the agent.
  Missing jq produces an empty payload, which clitab ignores.

## Qoder CLI notes

- Hook firing, stdin field names and `$PPID` = `qodercli` were verified live
  on 1.1.65; Qoder's `Notification` payload carries the same `message` field
  plus a `notification_type` (the Qoder install prompt's matcher admits only
  the types that mean "waiting for you": `permission_prompt` / `idle_prompt` /
  `elicitation_dialog`).
- Qoder's `AskUserQuestion` tool wraps answers in the same text as Claude
  Code ("Your questions have been answered: … You can now continue with these
  answers in mind"), so the answer jq works unchanged.
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
  passing it to the terminal, so that setup is likely inert. The current
  `Notification` hook replaces it.
