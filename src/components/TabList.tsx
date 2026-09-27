import React, { useCallback, useRef } from 'react';
import { Tab } from '../types';
import { TabItem } from './TabItem';

interface TabListProps {
  tabs: Tab[];
  activeTabId: string | null;
  onTabClick: (tabId: string) => void;
  onTabClose: (tabId: string) => void;
  onNewTab: () => void;
}

export const TabList: React.FC<TabListProps> = ({
  tabs,
  activeTabId,
  onTabClick,
  onTabClose,
  onNewTab,
}) => {
  const itemsRef = useRef<HTMLDivElement>(null);

  // Arrow navigation inside the tablist: activate and move focus in one step,
  // which is what a vertical tablist is expected to do.
  const focusTab = useCallback(
    (index: number) => {
      const count = tabs.length;
      if (count === 0) return;
      const tab = tabs[(index + count) % count];
      if (!tab) return;
      onTabClick(tab.id);
      itemsRef.current
        ?.querySelector<HTMLElement>(`[data-tab-id="${CSS.escape(tab.id)}"]`)
        ?.focus();
    },
    [tabs, onTabClick]
  );

  const onKeyDown = (event: React.KeyboardEvent) => {
    const active = tabs.findIndex((tab) => tab.id === activeTabId);
    const current = active < 0 ? 0 : active;
    switch (event.key) {
      case 'ArrowDown':
      case 'ArrowRight':
        event.preventDefault();
        focusTab(current + 1);
        break;
      case 'ArrowUp':
      case 'ArrowLeft':
        event.preventDefault();
        focusTab(current - 1);
        break;
      case 'Home':
        event.preventDefault();
        focusTab(0);
        break;
      case 'End':
        event.preventDefault();
        focusTab(tabs.length - 1);
        break;
      default:
        break;
    }
  };

  return (
    <nav className="tab-list" aria-label="Terminals">
      <div className="tab-list-header">
        <span>Terminal Tabs</span>
      </div>
      <div
        className="tab-list-items"
        ref={itemsRef}
        role="tablist"
        aria-orientation="vertical"
        onKeyDown={onKeyDown}
      >
        {tabs.map((tab, index) => (
          <TabItem
            key={tab.id}
            tab={tab}
            index={index}
            count={tabs.length}
            isActive={tab.id === activeTabId}
            onClick={() => onTabClick(tab.id)}
            onClose={() => onTabClose(tab.id)}
          />
        ))}
      </div>
      <button className="new-tab-button" onClick={onNewTab}>
        + New Tab
      </button>
    </nav>
  );
};
