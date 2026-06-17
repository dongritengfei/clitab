import { useCallback } from 'react';
import { TabList } from './components/TabList';
import { Terminal } from './components/Terminal';
import { useTabManager } from './hooks/useTabManager';
import './App.css';

function App() {
  const {
    tabs,
    activeTabId,
    createTab,
    closeTab,
    switchTab,
    registerOutputHandler,
    unregisterOutputHandler,
    writeInput,
    resizePty,
  } = useTabManager();

  const handleTabClick = useCallback(
    (tabId: string) => {
      switchTab(tabId);
    },
    [switchTab]
  );

  const handleTabClose = useCallback(
    (tabId: string) => {
      closeTab(tabId);
    },
    [closeTab]
  );

  const handleNewTab = useCallback(() => {
    createTab();
  }, [createTab]);

  const activeTab = tabs.find((t) => t.id === activeTabId);

  return (
    <div className="app-container">
      <TabList
        tabs={tabs}
        activeTabId={activeTabId}
        onTabClick={handleTabClick}
        onTabClose={handleTabClose}
        onNewTab={handleNewTab}
      />
      <div className="terminal-area">
        {tabs.length === 0 ? (
          <div className="empty-state">
            <p>No tabs open</p>
            <button onClick={handleNewTab}>Create a new tab</button>
          </div>
        ) : (
          tabs.map((tab) => (
            <div
              key={tab.id}
              className={`terminal-wrapper ${tab.id === activeTabId ? 'active' : ''}`}
            >
              <Terminal
                tabId={tab.id}
                isActive={tab.id === activeTabId}
                onOutput={registerOutputHandler}
                onUnmount={unregisterOutputHandler}
                onInput={writeInput}
                onResize={resizePty}
              />
            </div>
          ))
        )}
      </div>
    </div>
  );
}

export default App;
