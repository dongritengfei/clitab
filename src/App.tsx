import React, { useCallback, useRef, useState } from 'react';
import { SearchBar } from './components/SearchBar';
import { TabList } from './components/TabList';
import { Terminal } from './components/Terminal';
import { TimelinePanel } from './components/TimelinePanel';
import { useTabManager } from './hooks/useTabManager';
import { loadNumber, saveNumber } from './lib/prefs';
import type { TimelineEvent } from './lib/timeline';
import './App.css';

/** Stable empty array, so an eventless panel does not re-render every tick. */
const NO_EVENTS: TimelineEvent[] = [];

/* Draggable column widths. The defaults are a step up from the old fixed CSS
   widths (240/280) to match the enlarged UI fonts; the clamps keep neither
   panel from eating the terminal. Persisted per column — a saved width wins
   over the default, and double-clicking a splitter resets to it. */
const SIDEBAR_KEY = 'clitab.sidebarWidth';
const SIDEBAR_DEFAULT = 280;
const SIDEBAR_MIN = 160;
const SIDEBAR_MAX = 480;
const TIMELINE_KEY = 'clitab.timelineWidth';
const TIMELINE_DEFAULT = 320;
const TIMELINE_MIN = 200;
const TIMELINE_MAX = 480;

/** Which column a splitter drag resizes. */
type Splitter = 'sidebar' | 'timeline';

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function App() {
  const {
    tabs,
    panes,
    activeTabId,
    error,
    dismissError,
    createTab,
    closeTab,
    switchTab,
    reorderTab,
    attachTab,
    detachTab,
    writeInput,
    resizePty,
    searchNonce,
    closeSearch,
    terminalFontSize,
    inputStripEnabled,
    inputStripFocusNonce,
    timelines,
    staleEvents,
    navigateToEvent,
  } = useTabManager();

  const [sidebarWidth, setSidebarWidth] = useState(() =>
    loadNumber(SIDEBAR_KEY, SIDEBAR_MIN, SIDEBAR_MAX, SIDEBAR_DEFAULT)
  );
  const [timelineWidth, setTimelineWidth] = useState(() =>
    loadNumber(TIMELINE_KEY, TIMELINE_MIN, TIMELINE_MAX, TIMELINE_DEFAULT)
  );
  const [dragging, setDragging] = useState<Splitter | null>(null);
  // The in-flight splitter gesture. Widths stream through the state above (the
  // columns re-render per move), but the gesture's anchor and latest value
  // live in a ref: moves never read a stale render snapshot, and release
  // persists exactly what is on screen — once.
  const drag = useRef<{
    which: Splitter;
    startX: number;
    startWidth: number;
    current: number;
  } | null>(null);

  const startSplitterDrag =
    (which: Splitter) => (event: React.PointerEvent<HTMLDivElement>) => {
      if (event.button !== 0) return;
      // Same trackpad tap-to-click filter as TabList's reorder: only a
      // physical press may grab the splitter (synthesized downs carry
      // buttons=0 — see Terminal.tsx's gesture notes).
      if ((event.buttons & 1) === 0) return;
      event.preventDefault();
      // Capture keeps the gesture fed while the pointer is over the terminal
      // (or outside the window); it releases implicitly on pointerup.
      event.currentTarget.setPointerCapture(event.pointerId);
      const startWidth = which === 'sidebar' ? sidebarWidth : timelineWidth;
      drag.current = { which, startX: event.clientX, startWidth, current: startWidth };
      setDragging(which);
      document.body.classList.add('splitting');
    };

  const onSplitterMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const gesture = drag.current;
    if (!gesture) return;
    // The sidebar's right edge follows the pointer; the timeline's left edge
    // moves the other way.
    const delta =
      gesture.which === 'sidebar'
        ? event.clientX - gesture.startX
        : gesture.startX - event.clientX;
    const next =
      gesture.which === 'sidebar'
        ? clamp(gesture.startWidth + delta, SIDEBAR_MIN, SIDEBAR_MAX)
        : clamp(gesture.startWidth + delta, TIMELINE_MIN, TIMELINE_MAX);
    if (next === gesture.current) return;
    gesture.current = next;
    if (gesture.which === 'sidebar') setSidebarWidth(next);
    else setTimelineWidth(next);
  };

  // pointercancel commits too: whatever width is on screen is what persists.
  const endSplitterDrag = () => {
    const gesture = drag.current;
    drag.current = null;
    setDragging(null);
    document.body.classList.remove('splitting');
    if (gesture) {
      saveNumber(gesture.which === 'sidebar' ? SIDEBAR_KEY : TIMELINE_KEY, gesture.current);
    }
  };

  const resetSplitter = (which: Splitter) => () => {
    const key = which === 'sidebar' ? SIDEBAR_KEY : TIMELINE_KEY;
    const fallback = which === 'sidebar' ? SIDEBAR_DEFAULT : TIMELINE_DEFAULT;
    if (which === 'sidebar') setSidebarWidth(fallback);
    else setTimelineWidth(fallback);
    saveNumber(key, fallback);
  };

  const splitter = (which: Splitter) => (
    <div
      className={`splitter${dragging === which ? ' dragging' : ''}`}
      role="separator"
      aria-orientation="vertical"
      title="Drag to resize · double-click to reset"
      onPointerDown={startSplitterDrag(which)}
      onPointerMove={onSplitterMove}
      onPointerUp={endSplitterDrag}
      onPointerCancel={endSplitterDrag}
      onDoubleClick={resetSplitter(which)}
    />
  );

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
        width={sidebarWidth}
        tabs={tabs}
        activeTabId={activeTabId}
        onTabClick={switchTab}
        onTabActivate={activateTab}
        onTabClose={closeTab}
        onReorder={reorderTab}
        onNewTab={createTab}
      />
      {splitter('sidebar')}
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
        {/* Panes render in mount order (`panes`), never in sidebar order:
            reordering their DOM would reset each xterm viewport's scrollTop
            (see useTabManager's paneIds). They are an invisible-order stack. */}
        {panes.length === 0 ? (
          <div className="empty-state">
            <p>No tabs open</p>
            <button onClick={createTab}>Create a new tab</button>
          </div>
        ) : (
          panes.map((tab) => (
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
                fontSize={terminalFontSize}
                stripEnabled={inputStripEnabled}
                stripFocusNonce={inputStripFocusNonce}
                attach={attachTab}
                detach={detachTab}
                onInput={writeInput}
                onResize={resizePty}
              />
            </div>
          ))
        )}
      </div>
      {splitter('timeline')}
      <TimelinePanel
        width={timelineWidth}
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
