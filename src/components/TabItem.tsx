import React, { useRef, useEffect, useState } from 'react';
import { Tab } from '../types';

interface TabItemProps {
  tab: Tab;
  isActive: boolean;
  onClick: () => void;
  onClose: () => void;
}

// Truncate path from the beginning, keeping the end visible
function truncatePath(fullPath: string, maxWidth: number): string {
  if (fullPath.length <= 30) return fullPath;

  // Simple approach: show last ~25 chars with "..." prefix
  const charsToShow = Math.floor(maxWidth / 8); // Approx chars based on width
  if (charsToShow >= fullPath.length) return fullPath;

  return '...' + fullPath.slice(-(charsToShow - 3));
}

export const TabItem: React.FC<TabItemProps> = ({ tab, isActive, onClick, onClose }) => {
  const isPath = !tab.hasClaudeTitle;
  const containerRef = useRef<HTMLDivElement>(null);
  const [displayTitle, setDisplayTitle] = useState(tab.title);

  useEffect(() => {
    if (isPath && containerRef.current) {
      const width = containerRef.current.offsetWidth;
      setDisplayTitle(truncatePath(tab.title, width - 30)); // Subtract close button width
    } else {
      setDisplayTitle(tab.title);
    }
  }, [tab.title, isPath]);

  return (
    <div
      ref={containerRef}
      className={`tab-item ${isActive ? 'active' : ''} ${tab.flashing ? 'flashing' : ''}`}
      onClick={onClick}
    >
      <span
        className="tab-title"
        title={tab.title}
      >
        {displayTitle}
      </span>
      <button
        className="tab-close"
        onClick={(e) => {
          e.stopPropagation();
          onClose();
        }}
      >
        ×
      </button>
    </div>
  );
};
