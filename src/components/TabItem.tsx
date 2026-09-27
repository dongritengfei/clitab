import React, { useLayoutEffect, useRef, useState } from 'react';
import { Tab } from '../types';

interface TabItemProps {
  tab: Tab;
  isActive: boolean;
  onClick: () => void;
  onClose: () => void;
}

/** Approximate advance width of one character at the tab title's font size. */
const CHAR_WIDTH = 7.2;
/** Space reserved for the close button and padding. */
const CHROME_WIDTH = 34;

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

export const TabItem: React.FC<TabItemProps> = ({ tab, isActive, onClick, onClose }) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const [displayTitle, setDisplayTitle] = useState(tab.title);

  // A path title is the working directory, so shorten it; a program title
  // (Claude Code naming its session) is shown as the author intended.
  const isPath = !tab.hasClaudeTitle;

  useLayoutEffect(() => {
    const container = containerRef.current;
    if (!isPath || !container) {
      setDisplayTitle(tab.title);
      return;
    }
    const available = Math.floor((container.offsetWidth - CHROME_WIDTH) / CHAR_WIDTH);
    setDisplayTitle(shortenPath(tab.title, available));
  }, [tab.title, isPath]);

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
      aria-controls={`panel-${tab.id}`}
      tabIndex={0}
      onKeyDown={(event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          onClick();
        }
      }}
    >
      <span className="tab-title" title={tab.title}>
        {displayTitle}
      </span>
      <button
        className="tab-close"
        aria-label={`Close ${tab.title}`}
        onClick={(event) => {
          event.stopPropagation();
          onClose();
        }}
      >
        ×
      </button>
    </div>
  );
};
