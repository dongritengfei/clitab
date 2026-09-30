/**
 * Turn state reported via the OSC 7777 hook protocol. `kind` discriminates;
 * all timestamps are epoch milliseconds from the backend's clock. Mirrors
 * `TabStatus` in `src-tauri/src/status.rs`.
 */
export type TabStatus =
  | { kind: 'thinking'; since: number }
  | { kind: 'tool'; name: string; since: number }
  | { kind: 'done'; duration: number | null; at: number };

/** A Notification-hook message awaiting the user. Mirrors `Notice` in Rust. */
export interface TabNotice {
  msg: string | null;
  at: number;
}

export interface Tab {
  id: string;
  title: string;
  cwd: string;
  /** True when a program (e.g. Claude Code) owns the title, not the cwd. */
  hasClaudeTitle: boolean;
  /** Hook-protocol turn state; null until this tab's session speaks it. */
  status: TabStatus | null;
  /** Notification awaiting the user; null once seen or superseded. */
  notice: TabNotice | null;
  /** Renderer-only: the tab is asking for attention. */
  flashing: boolean;
}

/** Shape returned by the `create_tab` / `list_tabs` commands. */
export interface TabResponse {
  id: string;
  title: string;
  cwd: string;
  hasClaudeTitle: boolean;
  /** Hook-protocol turn state; null until this tab's session speaks it. */
  status: TabStatus | null;
  /** Notification awaiting the user; null once acknowledged. */
  notice: TabNotice | null;
}

export interface PtyOutputPayload {
  tab_id: string;
  /** Raw PTY bytes, base64 encoded (Tauri event payloads are JSON). */
  data: string;
  /**
   * Absolute stream position of `data`'s first byte. Compared against an
   * attach's `replayEnd` so a re-attach drops bytes the replay already covered.
   */
  seq: number;
}

export interface TabTitlePayload {
  tab_id: string;
  title: string;
  /** True when a program (not the shell prompt) set the title. */
  program_title: boolean;
}

/** Shape returned by the `attach_stream` command. */
export interface AttachStreamResponse {
  /** Replay ring contents, base64 encoded, oldest byte first. */
  data: string;
  /** Stream position the replay ends at; live chunks at or past it are new. */
  replayEnd: number;
}

export interface TabCwdPayload {
  tab_id: string;
  cwd: string;
}

export interface TabFlashPayload {
  tab_id: string;
}

/** Full replacement state for one tab's protocol fields. */
export interface TabStatusPayload {
  tab_id: string;
  status: TabStatus | null;
  notice: TabNotice | null;
}

export interface TabExitPayload {
  tab_id: string;
  code: number;
}

/** Emitted by the native menu when a tab accelerator is pressed. */
export interface MenuShortcutPayload {
  id: string;
}

/**
 * Prefix of the error a command returns when the tab it names no longer
 * exists. Mirrors `TAB_GONE` in `src-tauri/src/lib.rs`.
 */
export const TAB_GONE = 'clitab:tab-gone:';
