import { useCallback } from 'react';
import { SearchBar } from './components/SearchBar';
import { TabList } from './components/TabList';
import { Terminal } from './components/Terminal';
import { TimelinePanel } from './components/TimelinePanel';
import { useTabManager } from './hooks/useTabManager';
import type { TimelineEvent } from './lib/timeline';
import './App.css';

/** Stable empty array, so an eventless panel does not re-render every tick. */
const NO_EVENTS: TimelineEvent[] = [];

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
    searchNonce,
    closeSearch,
    timelines,
    staleEvents,
    navigateToEvent,
  } = useTabManager();

  // Clicking (or pressing Enter/Space on) a tab must land the caret in its
  // terminal — also when it is already the active one, where no `isActive`
  // transition runs. rAF: React has flushed by then, so the panel is visible
  // and its xterm helper textarea (the real input target) can take focus.
  const activateTab = useCallback(
    (tabId: string) => {
      switchTab(tabId);
      requestAnimationFrame(() => {
        document
          .getElementById(`panel-${tabId}`)
          ?.querySelector<HTMLElement>('.xterm-helper-textarea')
          ?.focus();
      });
    },
    [switchTab]
  );

  return (
    <div className="app-container">
      <TabList
        tabs={tabs}
        activeTabId={activeTabId}
        onTabClick={switchTab}
        onTabActivate={activateTab}
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
        {searchNonce > 0 && activeTabId && (
          <SearchBar tabId={activeTabId} nonce={searchNonce} onClose={closeSearch} />
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
      <TimelinePanel
        tabId={activeTabId}
        events={activeTabId ? timelines[activeTabId] ?? NO_EVENTS : NO_EVENTS}
        isStale={(eventId) => activeTabId !== null && staleEvents.has(`${activeTabId}:${eventId}`)}
        onNavigate={(eventId) => {
          if (activeTabId) navigateToEvent(activeTabId, eventId);
        }}
      />
    </div>
  );
}

export default App;
