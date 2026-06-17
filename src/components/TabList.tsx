import React from 'react';
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
  return (
    <div className="tab-list">
      <div className="tab-list-header">
        <span>Terminal Tabs</span>
      </div>
      <div className="tab-list-items">
        {tabs.map((tab) => (
          <TabItem
            key={tab.id}
            tab={tab}
            isActive={tab.id === activeTabId}
            onClick={() => onTabClick(tab.id)}
            onClose={() => onTabClose(tab.id)}
          />
        ))}
      </div>
      <button className="new-tab-button" onClick={onNewTab}>
        + New Tab
      </button>
    </div>
  );
};
