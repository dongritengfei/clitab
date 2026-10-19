# Session Restore — Design Spec

Date: 2026-10-19 · Status: agreed with the user (grilling session, Q1–Q8 + D1–D7 confirmed)

## Goal

The second launch of clitab looks like the moment the last one was closed: the
same tabs in the same order with the same titles, the tab that was active
comes back with its agent conversation already resuming, the others resume on
first click — and the timeline shows the restored session's earlier user
prompts as display-only history rows.

## Confirmed decisions

- **Q1 = A** — restore the tabs that were still open at last quit ("未关闭"),
  not a full `claude --resume`-style session browser. The transcripts hold no
  closed/exited marker, so "was open" can only come from clitab's own state.
- **Q2 = both agents** — Claude Code and Qoder CLI. Verified facts: Qoder
  mirrors Claude Code's storage (`~/.qoder/projects/<cwd-encoded>/<sid>.jsonl`)
  and its CLI takes `--resume <id>`.
- **Q3** — not a separate "history" sidebar section: restored tabs appear as
  ordinary tabs, visually identical to the previous run.
- **Q4 = B (hybrid)** — at startup only the previously *active* tab's session
  is auto-resumed; other restored tabs resume on first activation.
- **Q5 = accepted** — the hook protocol gains session fields
  (`sid`/`tp`/`a`) on the UserPromptSubmit payload. The wiki Agent-Setup
  install prompts must be updated and users must reinstall hooks; until then
  the feature degrades silently (tabs restore as plain shells).
- **Q6 = accepted** — snapshot covers all open tabs (including shell-only),
  original order, last active tab; written *continuously* on every
  snapshot-relevant change (crash-safe), not only at graceful quit;
  `tab-snapshot.json` in `app_config_dir`, following `last_cwd.rs`'s
  best-effort contract. Deleted transcript ⇒ plain shell in the original cwd
  (via existing `resolve_cwd`). The no-snapshot path keeps today's
  last-cwd startup behavior.
- **Q7 = accepted** — history loading: after a resume, the backend streams the
  session JSONL and extracts real user prompts (`type=user`, non-sidechain,
  plain-string content, not an injected wrapper); rows are visually distinct,
  **not** click-navigable in v1; no historical answers; history + live rows
  share the existing 500-row cap.
- **Q8** — consensus confirmed; develop in a worktree.

## Derived defaults (accepted via Q8)

- **D1** — resume by injecting `<agent> --resume <sid>\n` into the tab's
  normal shell (keeps OSC 7 cwd reporting, `claude-done` title revert and the
  preexec machinery working). **No** `--fork-session`: resume appends to the
  same `<sid>.jsonl`, keeping history continuous.
- **D2** — agent kind travels as a static `"a":"claude"|"qoder"` field in the
  hook payload (the two install JSONs already differ); `tp` (transcript_path)
  removes the `~/.qoder` vs `~/.qoderwork` directory ambiguity.
- **D3** — "agent alive at quit" is tracked from existing signals: any hook
  event ⇒ alive; shell-integration `claude-done` (PromptReady) ⇒ exited.
  Tabs whose agent had already exited restore as plain shells (no resume).
- **D4** — degraded paths: transcript deleted ⇒ resume cancelled silently,
  plain shell, title reverted; hooks not reinstalled (no `sid`) ⇒ no binding,
  plain shell restore; no jq ⇒ same (sid rides the jq-built payload).
- **D5** — a Finder "New clitab Tab Here" request during the startup grace
  becomes an *additional* tab on top of the restore, taking focus.
- **D6** — v1 history extraction is strict: plain-string `message.content`
  only; excluded prefixes: `<task-notification>`, `<local-command-caveat>`,
  `<local-command-stdout>`, `<command-name>`, `<command-message>`,
  `<cross-session-message`. Array content (tool_result, skill injection,
  attachments) is skipped.
- **D7** — one tab, many sessions: the snapshot records the *latest* binding
  (every prompt hook refreshes it); restore resumes only that last session.

## Non-goals (this iteration)

- Click-to-jump for historical rows (resumed transcripts repaint, but line
  alignment is fuzzy — display-only in v1).
- Historical `answer` rows (AskUserQuestion choices are unreliable to extract
  from tool_result records).
- A full session browser (Q1 option B).
- A restore on/off setting.
- Restoring terminal scrollback itself (physically gone; `--resume`'s repaint
  is the fidelity ceiling).

## Verified environment facts

- `claude --resume <session-id>` resumes non-interactively, appending to the
  same JSONL by default; `--fork-session` is the opt-in that creates a new id.
  Qoder CLI mirrors the flags (`-r/--resume [id]`, `--fork-session`,
  `--list-sessions`).
- `~/.claude/projects` dir naming: cwd with `/` → `-` (leading `-`). Session
  title records exist (`ai-title`/`custom-title`) but are not needed: the
  snapshot carries clitab's own tab titles.
- Transcripts record no closed/exited state.
- Scale on this machine: 170 JSONLs / 248 MB total, median 401 KB, max 51.6 MB
  ⇒ parse streaming, cap output (500 prompts).
- Hook stdin JSON carries `session_id` and `transcript_path` (Claude Code;
  Qoder mirrors stdin fields).
- The OSC 7777 wire decoder already ignores unknown fields (pinned by
  `status.rs::unknown_extra_fields_are_ignored`), so old and new payloads
  coexist in both directions.

## Obligations (project conventions)

- Wiki `Agent-Setup.md` / `.zh.md`: update the UserPromptSubmit hook command,
  JSON blocks byte-identical across languages (wiki is a separate repo —
  cloned at `/tmp/clitab-wiki` during planning).
- `AGENT_HOOKS.md`: document the extended payload.
- `README.md` / `README.zh-CN.md`: feature bullet, kept in sync.
- `CLAUDE.md`: architecture section sync.
- Rust unit tests for every new pure module (project rule: `npm test` =
  `cargo test --lib`; no frontend test runner — `npm run typecheck` is the
  frontend gate).
