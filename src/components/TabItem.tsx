import React, { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Tab } from '../types';

interface TabItemProps {
  tab: Tab;
  /** Zero-based position; shown as the ⌘<number> switch hint. */
  index: number;
  isActive: boolean;
  onClick: () => void;
  onClose: () => void;
}

/** Approximate advance width of one character at the tab title's font size. */
const CHAR_WIDTH = 7.2;
/** Same, at the smaller cwd line's font size. */
const CWD_CHAR_WIDTH = 6.1;
/** Space reserved for the number badge, close button, and padding. */
const CHROME_WIDTH = 54;

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

/** Delay before the full-name tooltip appears, so sweeping the cursor across
 *  the tab list does not strobe a tooltip for every row. */
const TOOLTIP_DELAY_MS = 350;

export const TabItem: React.FC<TabItemProps> = ({ tab, index, isActive, onClick, onClose }) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const titleRef = useRef<HTMLSpanElement>(null);
  const hoverTimer = useRef<number | undefined>(undefined);
  const [displayTitle, setDisplayTitle] = useState(tab.title);
  const [displayCwd, setDisplayCwd] = useState(tab.cwd);
  // Full title plus where to anchor the tooltip; null keeps it hidden.
  const [tooltip, setTooltip] = useState<{ text: string; x: number; y: number } | null>(null);

  // A path title is the working directory, so shorten it; a program title
  // (Claude Code naming its session) is shown as the author intended.
  const isPath = !tab.hasClaudeTitle;
  // The cwd gets its own de-emphasized line only when it differs from the
  // title; when the title already is the cwd, a second line is pure noise.
  const showCwd = tab.cwd !== '' && tab.cwd !== tab.title;

  useLayoutEffect(() => {
    const container = containerRef.current;
    const available = container
      ? Math.floor((container.offsetWidth - CHROME_WIDTH) / CHAR_WIDTH)
      : 0;
    setDisplayTitle(isPath && container ? shortenPath(tab.title, available) : tab.title);

    const cwdAvailable = container
      ? Math.floor((container.offsetWidth - CHROME_WIDTH) / CWD_CHAR_WIDTH)
      : 0;
    setDisplayCwd(container ? shortenPath(tab.cwd, cwdAvailable) : tab.cwd);
  }, [tab.title, tab.cwd, isPath]);

  // Clear any pending tooltip timer on unmount so it can't fire on a dead node.
  useEffect(() => () => window.clearTimeout(hoverTimer.current), []);

  const handleTitleEnter = () => {
    const title = titleRef.current;
    const container = containerRef.current;
    if (!title || !container) return;
    window.clearTimeout(hoverTimer.current);
    hoverTimer.current = window.setTimeout(() => {
      // Only worth showing when the name is clipped: either shortened by
      // shortenPath (displayTitle differs) or cut by CSS ellipsis (overflow).
      if (displayTitle === tab.title && title.scrollWidth <= title.clientWidth) return;
      const rect = container.getBoundingClientRect();
      setTooltip({ text: tab.title, x: rect.right + 8, y: rect.top + rect.height / 2 });
    }, TOOLTIP_DELAY_MS);
  };

  const handleTitleLeave = () => {
    window.clearTimeout(hoverTimer.current);
    setTooltip(null);
  };

  return (
    <div
      ref={containerRef}
      className={`tab-item ${isActive ? 'active' : ''} ${tab.flashing ? 'flashing' : ''}`}
      onClick={onClick}
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
      onMouseDown={(event) => {
        if (event.button === 0) onClick();
      }}
      onKeyDown={(event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          onClick();
        }
      }}
    >
      {/* ⌘1–⌘9 switch tabs by position; showing the number makes the
          shortcut discoverable. Decorative: ATs get it via aria-posinset. */}
      <span className="tab-index" aria-hidden="true">
        {index + 1}
      </span>
      <div className="tab-text">
        <span
          ref={titleRef}
          className="tab-title"
          onMouseEnter={handleTitleEnter}
          onMouseLeave={handleTitleLeave}
        >
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
        onClick={(event) => {
          event.stopPropagation();
          onClose();
        }}
      >
        ×
      </button>
      {/* Portaled so the sidebar's overflow cannot clip it. */}
      {tooltip &&
        createPortal(
          <div
            className="tab-tooltip"
            style={{ left: tooltip.x, top: tooltip.y }}
            role="tooltip"
          >
            {tooltip.text}
          </div>,
          document.body
        )}
    </div>
  );
};
