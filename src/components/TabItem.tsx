import React, { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { Tab } from '../types';

interface TabItemProps {
  tab: Tab;
  /** Zero-based position in the tab list. */
  index: number;
  /** Total number of tabs; with index decides the ⌘<number> badge. */
  count: number;
  isActive: boolean;
  onClick: () => void;
  onClose: () => void;
}

/** Approximate advance width of one character at the tab title's font size. */
const CHAR_WIDTH = 7.2;
/** Same, at the smaller cwd line's font size. */
const CWD_CHAR_WIDTH = 6.1;
/** Space reserved for the non-text parts of a row: 3px border-left +
 *  16px/4px padding + 16px badge + 2×4px gaps + 20px close button. */
const CHROME_WIDTH = 67;
/** Extra width of the waiting dot when present: 6px dot + 4px gap. */
const WAITING_DOT_WIDTH = 10;

/** The done line lingers this long, then fades (see .tab-status.done-hidden). */
const DONE_FADE_MS = 6000;

/**
 * Shorten a path from the left: the tail is what identifies a tab, so
 * `/Users/yiyi/workspace/github/clitab` becomes `…/workspace/github/clitab`.
 * Breaking on a path separator reads better than cutting mid-directory.
 */
export function shortenPath(path: string, availableChars: number): string {
  if (availableChars <= 1 || path.length <= availableChars) return path;

  const absolute = path.startsWith('/');
  const pathBudget = availableChars - 1; // room for the path after the ellipsis
  const segBudget = pathBudget - (absolute ? 1 : 0); // ...and the leading slash
  const segments = path.split('/').filter(Boolean);
  const tail: string[] = [];
  let length = 0;

  for (let i = segments.length - 1; i >= 0; i--) {
    const segment = segments[i];
    if (segment === undefined) continue;
    const added = segment.length + (tail.length > 0 ? 1 : 0);
    if (length + added > segBudget) break;
    tail.unshift(segment);
    length += added;
  }

  if (tail.length === 0) {
    // Even a single segment does not fit: keep the end of the raw path.
    // pathBudget >= 1 here, so slice(-pathBudget) never degrades to slice(0).
    return `…${path.slice(-pathBudget)}`;
  }

  // Restore the leading slash for absolute paths (already budgeted above).
  const joined = (absolute ? '/' : '') + tail.join('/');
  return `…${joined}`;
}

/**
 * Human duration for the status line: `42s`, `1m 05s`, `1h 02m`. Negative
 * input (clock skew between the backend timestamp and Date.now) clamps to 0.
 */
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (hours > 0) return `${hours}h ${String(minutes).padStart(2, '0')}m`;
  if (minutes > 0) return `${minutes}m ${String(seconds).padStart(2, '0')}s`;
  return `${seconds}s`;
}

export const TabItem: React.FC<TabItemProps> = ({
  tab,
  index,
  count,
  isActive,
  onClick,
  onClose,
}) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const [displayTitle, setDisplayTitle] = useState(tab.title);
  const [displayCwd, setDisplayCwd] = useState(tab.cwd);

  // A path title is the working directory, so shorten it; a program title
  // (Claude Code naming its session) is shown as the author intended.
  const isPath = !tab.hasClaudeTitle;
  // The cwd gets its own de-emphasized line only when it differs from the
  // title; when the title already is the cwd, a second line is pure noise.
  const showCwd = tab.cwd !== '' && tab.cwd !== tab.title;

  // The dot only costs width while it is there; folding it into the chrome
  // budget keeps the title ellipsis accurate either way.
  const chromeWidth = CHROME_WIDTH + (tab.waiting ? WAITING_DOT_WIDTH : 0);

  useLayoutEffect(() => {
    const container = containerRef.current;
    const available = container
      ? Math.floor((container.offsetWidth - chromeWidth) / CHAR_WIDTH)
      : 0;
    setDisplayTitle(isPath && container ? shortenPath(tab.title, available) : tab.title);

    const cwdAvailable = container
      ? Math.floor((container.offsetWidth - chromeWidth) / CWD_CHAR_WIDTH)
      : 0;
    setDisplayCwd(container ? shortenPath(tab.cwd, cwdAvailable) : tab.cwd);
  }, [tab.title, tab.cwd, isPath, chromeWidth]);

  // Live elapsed timer: only mounted while a turn is actually ticking, so
  // idle tabs cost nothing.
  const ticking = tab.status?.kind === 'thinking' || tab.status?.kind === 'tool';
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!ticking) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [ticking]);

  // The done line fades out a few seconds after the turn ended; any newer
  // status (or a new done with a different `at`) cancels the fade.
  const doneAt = tab.status?.kind === 'done' ? tab.status.at : null;
  const [doneFaded, setDoneFaded] = useState(false);
  useEffect(() => {
    setDoneFaded(false);
    if (doneAt === null) return;
    const timer = setTimeout(() => setDoneFaded(true), DONE_FADE_MS);
    return () => clearTimeout(timer);
  }, [doneAt]);

  // Status-line content. A pending notification outranks the turn state; the
  // row itself exists only once this tab has spoken the protocol, so plain
  // shell tabs keep their exact current layout.
  const hasStatusRow = tab.status !== null || tab.notice !== null;
  let statusText = '';
  if (tab.notice) {
    statusText = `⚠ ${tab.notice.msg ?? 'Agent needs attention'}`;
  } else if (tab.status?.kind === 'tool') {
    statusText = `⚙ ${tab.status.name} · ${formatDuration(now - tab.status.since)}`;
  } else if (tab.status?.kind === 'thinking') {
    statusText = `⏳ ${formatDuration(now - tab.status.since)}`;
  } else if (tab.status?.kind === 'done') {
    statusText = `✓ ${tab.status.duration != null ? formatDuration(tab.status.duration) : 'done'}`;
  }

  // ⌘9 targets the last tab (macOS convention), so with more than 9 tabs
  // positions 9..n-1 have no shortcut of their own and get no badge.
  const badge = count > 9 && index >= 8 ? (index === count - 1 ? 9 : null) : index + 1;

  return (
    <div
      ref={containerRef}
      className={`tab-item ${isActive ? 'active' : ''} ${tab.flashing ? 'flashing' : ''} ${tab.waiting ? 'waiting' : ''}`}
      // Tabs are switched with the keyboard too, so make them reachable.
      role="tab"
      id={`tab-${tab.id}`}
      data-tab-id={tab.id}
      aria-selected={isActive}
      aria-posinset={index + 1}
      aria-controls={`panel-${tab.id}`}
      tabIndex={0}
      // Select on press, like browser tab strips: WKWebView's tap-to-click
      // synthesis intermittently drops the synthesized `click`, which made
      // trackpad taps need a second try. mousedown always arrives.
      // preventDefault keeps the browser from focusing the row: focus must
      // land in the terminal (App.activateTab does that), or the next arrow
      // keys would navigate the tab list instead of reaching the shell.
      onMouseDown={(event) => {
        if (event.button === 0) {
          event.preventDefault();
          onClick();
        }
      }}
      onKeyDown={(event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          onClick();
        }
      }}
    >
      {/* The badge doubles as the ⌘<number> hint, and always matches what
          the shortcut actually does. Decorative: ATs get position via
          aria-posinset. */}
      <span className="tab-index" aria-hidden="true">
        {badge ?? ''}
      </span>
      {tab.waiting && <span className="waiting-dot" aria-hidden="true" />}
      <div className="tab-text">
        <span className="tab-title" title={tab.title}>
          {displayTitle}
        </span>
        {hasStatusRow && (
          <span
            className={`tab-status${tab.notice ? ' notice' : ''}${doneFaded && !tab.notice ? ' done-hidden' : ''}`}
          >
            {statusText}
          </span>
        )}
        {showCwd && (
          <span className="tab-cwd" title={tab.cwd}>
            {displayCwd}
          </span>
        )}
      </div>
      <button
        className="tab-close"
        aria-label={`Close ${tab.title}`}
        onMouseDown={(event) => event.stopPropagation()}
        onClick={onClose}
      >
        ×
      </button>
    </div>
  );
};
