import React, { useLayoutEffect, useRef, useState } from 'react';
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
