export interface Tab {
  id: string;
  title: string;
  cwd: string;
  flashing: boolean;
  hasClaudeTitle: boolean;
}

export interface PtyOutputPayload {
  tab_id: string;
  data: number[];
}

export interface TabTitlePayload {
  tab_id: string;
  title: string;
}

export interface TabCwdPayload {
  tab_id: string;
  cwd: string;
}

export interface TabFlashPayload {
  tab_id: string;
}

export interface TabExitPayload {
  tab_id: string;
  code: number;
}
