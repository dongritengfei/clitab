import { TabList } from './components/TabList';
import { Terminal } from './components/Terminal';
import { useTabManager } from './hooks/useTabManager';
import './App.css';

function App() {
  const {
    tabs,
    activeTabId,
    error,
    dismissError,
    createTab,
    closeTab,
    switchTab,
    attachTab,
    detachTab,
    writeInput,
    resizePty,
  } = useTabManager();

  return (
    <div className="app-container">
      <TabList
        tabs={tabs}
        activeTabId={activeTabId}
        onTabClick={switchTab}
        onTabClose={closeTab}
        onNewTab={createTab}
      />
      <div className="terminal-area">
        {error && (
          <div className="error-banner" role="alert">
            <span>{error}</span>
            <button onClick={dismissError} aria-label="Dismiss">
              ×
            </button>
          </div>
        )}
        {tabs.length === 0 ? (
          <div className="empty-state">
            <p>No tabs open</p>
            <button onClick={createTab}>Create a new tab</button>
          </div>
        ) : (
          tabs.map((tab) => (
            <div
              key={tab.id}
              id={`panel-${tab.id}`}
              role="tabpanel"
              aria-labelledby={`tab-${tab.id}`}
              className={`terminal-wrapper ${tab.id === activeTabId ? 'active' : ''}`}
            >
              <Terminal
                tabId={tab.id}
                isActive={tab.id === activeTabId}
                attach={attachTab}
                detach={detachTab}
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
