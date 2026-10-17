/** Best-effort persistence for renderer-side UI preferences (panel widths,
 * terminal font size). localStorage lives in WKWebView's persistent data
 * store, so values survive app restarts; a missing, corrupt or out-of-range
 * read falls back to the caller's default, and a failed write degrades to
 * session-only. */

export function loadNumber(key: string, min: number, max: number, fallback: number): number {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return fallback;
    const value = Number(raw);
    if (!Number.isFinite(value)) return fallback;
    return Math.min(max, Math.max(min, Math.round(value)));
  } catch {
    return fallback;
  }
}

export function saveNumber(key: string, value: number): void {
  try {
    localStorage.setItem(key, String(value));
  } catch {
    // preferences are best-effort; nothing to recover from
  }
}
