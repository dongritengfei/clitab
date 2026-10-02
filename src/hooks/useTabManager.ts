import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  TAB_GONE,
  type AttachStreamResponse,
  type FocusTabPayload,
  type MenuShortcutPayload,
  type PtyOutputPayload,
  type Tab,
  type TabCwdPayload,
  type TabExitPayload,
  type TabFlashPayload,
  type TabResponse,
  type TabStatusPayload,
  type TabTitlePayload,
  type TabWaitingPayload,
} from '../types';
import { base64ToBytes, bytesToBase64 } from '../lib/base64';
import { getTerm } from '../lib/termRegistry';
import { MAX_TIMELINE_EVENTS, TimelineTracker, type TimelineEvent } from '../lib/timeline';
import type { IDecoration, IMarker } from '@xterm/xterm';

/** `isReplay` marks the attach-time ring replay, as opposed to live output. */
type OutputHandler = (chunk: Uint8Array, isReplay?: boolean) => void;
/** Internal sink: a live chunk plus its absolute position in the byte stream. */
type StreamSink = (chunk: Uint8Array, seq: number) => void;

/** How long the jumped-to terminal line stays highlighted. */
const HIGHLIGHT_MS = 1200;
/** Same color as xterm's selectionBackground (Catppuccin surface2, translucent). */
const HIGHLIGHT_COLOR = '#585b7066';

function toTab(response: TabResponse): Tab {
  return { ...response, flashing: false };
}

export interface TabManagerState {
  tabs: Tab[];
  activeTabId: string | null;
  error: string | null;
  dismissError: () => void;
  createTab: () => Promise<void>;
  closeTab: (tabId: string) => Promise<void>;
  closeActiveTab: () => Promise<void>;
  switchTab: (tabId: string) => void;
  selectTabByIndex: (index: number) => void;
  cycleTab: (direction: 1 | -1) => void;
  /** Subscribe `handler` to a tab's output, after replaying what it missed. */
  attachTab: (tabId: string, handler: OutputHandler) => Promise<void>;
  detachTab: (tabId: string) => void;
  writeInput: (tabId: string, data: Uint8Array) => Promise<void>;
  resizePty: (tabId: string, rows: number, cols: number) => Promise<void>;
  /** Per-tab timeline of key hook-protocol events (renderer-only history). */
  timelines: Record<string, TimelineEvent[]>;
  /** Keys `${tabId}:${eventId}` whose terminal line left the scrollback. */
  staleEvents: ReadonlySet<string>;
  /** Scroll the tab's terminal to a timeline event and highlight the line. */
  navigateToEvent: (tabId: string, eventId: number) => void;
}

export function useTabManager(): TabManagerState {
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [timelines, setTimelines] = useState<Record<string, TimelineEvent[]>>({});
  const [staleEvents, setStaleEvents] = useState<ReadonlySet<string>>(() => new Set());

  // Timeline transition detection is per-tab stateful. Markers bind an event
  // to its terminal line; they are mutable xterm objects, so they live in a
  // ref (tabId → eventId → marker), never in state.
  const trackers = useRef(new Map<string, TimelineTracker>());
  const markers = useRef(new Map<string, Map<number, IMarker>>());
  // The one highlight decoration currently on screen, plus its expiry timer.
  const highlight = useRef<{ decoration: IDecoration; timer: number } | null>(null);

  // Output handlers live in a ref: putting them in state would re-subscribe the
  // PTY listener on every tab mount, and the gap between unsubscribing the old
  // listener and subscribing the new one would silently drop terminal output.
  const handlers = useRef(new Map<string, StreamSink>());

  // Mirrors for use inside async callbacks and event listeners, which otherwise
  // see the values captured when they were created.
  const tabsRef = useRef<Tab[]>([]);
  const activeTabRef = useRef<string | null>(null);
  tabsRef.current = tabs;
  activeTabRef.current = activeTabId;

  // Resolves once the `pty-output` listener is live: a tab may only ask for its
  // replay after that, or the replay and the live stream would interleave.
  const outputReady = useMemo(() => {
    let resolve!: () => void;
    const promise = new Promise<void>((r) => {
      resolve = r;
    });
    return { promise, resolve };
  }, []);

  const abandonTab = useCallback((tabId: string) => {
    handlers.current.delete(tabId);
    trackers.current.delete(tabId);
    // The tab's markers die with its terminal instance; dropping the map
    // first also disarms their onDispose guards (see the tab-status
    // listener), so a closing tab never re-adds stale keys.
    markers.current.delete(tabId);
    setTimelines((prev) => {
      const { [tabId]: _dropped, ...rest } = prev;
      return rest;
    });
    setStaleEvents((prev) => {
      let next: Set<string> | null = null;
      for (const key of prev) {
        if (key.startsWith(`${tabId}:`)) (next ??= new Set(prev)).delete(key);
      }
      return next ?? prev;
    });
    const remaining = tabsRef.current.filter((tab) => tab.id !== tabId);
    const closedIndex = tabsRef.current.findIndex((tab) => tab.id === tabId);
    setTabs(remaining);
    setActiveTabId((current) => {
      if (current !== tabId) return current;
      // Focus the tab that took the closed one's slot.
      const next = remaining[Math.min(closedIndex, remaining.length - 1)];
      return next?.id ?? null;
    });
  }, []);

  const createTab = useCallback(async () => {
    try {
      // Inherit the working directory of whichever tab is active *at this
      // moment* (after a close, that is the tab which took its place); the
      // backend validates the path and falls back to home if it is gone.
      const active = tabsRef.current.find((tab) => tab.id === activeTabRef.current);
      const created = toTab(await invoke<TabResponse>('create_tab', { cwd: active?.cwd ?? null }));
      setTabs((prev) =>
        // A `list_tabs` snapshot taken after this tab existed can already
        // contain it; appending again would mount two views for one PTY.
        prev.some((tab) => tab.id === created.id)
          ? prev.map((tab) => (tab.id === created.id ? created : tab))
          : [...prev, created]
      );
      setActiveTabId(created.id);
      setError(null);
    } catch (err) {
      report(setError, 'Could not open a new tab', err);
    }
  }, []);

  const closeTab = useCallback(
    async (tabId: string) => {
      try {
        // Only ask when something is actually still running in the tab.
        if (await invoke<boolean>('has_active_process', { tabId })) {
          const { ask } = await import('@tauri-apps/plugin-dialog');
          const confirmed = await ask('This terminal is still running a process. Close it anyway?', {
            title: 'Close terminal',
            kind: 'warning',
          });
          if (!confirmed) return;
        }
        await invoke('close_tab', { tabId });
        abandonTab(tabId);
      } catch (err) {
        const message = messageOf(err);
        if (message.startsWith(TAB_GONE)) {
          // The shell exited on its own a moment ago; the tab is already gone
          // on the backend, so drop it locally instead of showing an error.
          abandonTab(tabId);
        } else {
          report(setError, 'Could not close the tab', err);
        }
      }
    },
    [abandonTab]
  );

  const closeActiveTab = useCallback(async () => {
    const tabId = activeTabRef.current;
    if (tabId) await closeTab(tabId);
  }, [closeTab]);

  const switchTab = useCallback((tabId: string) => {
    setActiveTabId(tabId);
    // Seeing the tab acknowledges its notification: clear it here and in the
    // registry, so a webview reload cannot resurrect an already-seen notice.
    if (tabsRef.current.some((tab) => tab.id === tabId && tab.notice)) {
      void invoke('ack_tab_notice', { tabId }).catch(() => {
        /* the tab may already be gone */
      });
    }
    setTabs((prev) =>
      prev.map((tab) => (tab.id === tabId ? { ...tab, flashing: false, notice: null } : tab))
    );
  }, []);

  const selectTabByIndex = useCallback(
    (index: number) => {
      const tab = tabsRef.current[index];
      if (tab) switchTab(tab.id);
    },
    [switchTab]
  );

  const cycleTab = useCallback(
    (direction: 1 | -1) => {
      const list = tabsRef.current;
      if (list.length < 2) return;
      const current = list.findIndex((tab) => tab.id === activeTabRef.current);
      const tab = list[(current + direction + list.length) % list.length];
      if (tab) switchTab(tab.id);
    },
    [switchTab]
  );

  // ⌘J: triage. Round-robin from the tab *after* the active one; the active
  // tab is never a target (you are already looking at it). An empty queue
  // does nothing at all — no focus change, no toast.
  const jumpToNextWaiting = useCallback(() => {
    const list = tabsRef.current;
    const active = list.findIndex((tab) => tab.id === activeTabRef.current);
    for (let step = 1; step < list.length; step++) {
      const candidate = list[(active + step + list.length) % list.length];
      if (candidate?.waiting) {
        switchTab(candidate.id);
        return;
      }
    }
  }, [switchTab]);

  const attachTab = useCallback(
    async (tabId: string, handler: OutputHandler) => {
      // Anything arriving while we catch up is queued, so the replay is always
      // written before the live stream instead of being interleaved with it.
      let live = false;
      const queued: { chunk: Uint8Array; seq: number }[] = [];
      const subscription: StreamSink = (chunk, seq) => {
        if (live) handler(chunk);
        else queued.push({ chunk, seq });
      };
      handlers.current.set(tabId, subscription);

      // Stream position the replay covers up to; queued chunks below it were
      // already written by the replay and must not be written twice.
      let replayEnd = 0;
      try {
        await outputReady.promise;
        // If the view was remounted while we were waiting (React does this in
        // dev), the newer attach must take the replay; consuming it here would
        // hand the shell's first screen to a disposed terminal.
        if (handlers.current.get(tabId) !== subscription) return;

        // Everything the PTY produced so far, oldest first; live chunks follow.
        const response = await invoke<AttachStreamResponse>('attach_stream', { tabId });
        replayEnd = response.replayEnd;
        const bytes = base64ToBytes(response.data);
        if (bytes.length > 0) handler(bytes, true);
      } catch (err) {
        // The tab can legitimately disappear while we are attaching to it
        // (open then immediately close); anything else is worth showing.
        const message = messageOf(err);
        if (message.startsWith(TAB_GONE)) {
          console.debug('attach skipped, tab already closed:', message);
        } else {
          report(setError, 'Could not attach to the terminal', err);
        }
      } finally {
        live = true;
      }

      for (const { chunk, seq } of queued) {
        if (seq >= replayEnd) {
          handler(chunk);
        } else if (seq + chunk.length > replayEnd) {
          // Straddles the boundary: the replay already showed the head.
          handler(chunk.subarray(replayEnd - seq));
        }
        // Else the replay covered the whole chunk; drop it.
      }
      queued.length = 0;
    },
    [outputReady]
  );

  const detachTab = useCallback((tabId: string) => {
    handlers.current.delete(tabId);
    // Tell the backend to stop emitting into a handler that no longer exists.
    void invoke('detach_tab', { tabId }).catch(() => {
      /* the tab may already be gone */
    });
  }, []);

  const writeInput = useCallback(async (tabId: string, data: Uint8Array) => {
    try {
      await invoke('pty_input', { tabId, data: bytesToBase64(data) });
    } catch (err) {
      // Typing into a tab that just closed is normal; keep it quiet.
      console.debug('pty_input failed:', messageOf(err));
    }
  }, []);

  const resizePty = useCallback(async (tabId: string, rows: number, cols: number) => {
    try {
      await invoke('resize_pty', { tabId, rows, cols });
    } catch (err) {
      console.debug('resize_pty failed:', messageOf(err));
    }
  }, []);

  const navigateToEvent = useCallback((tabId: string, eventId: number) => {
    const term = getTerm(tabId);
    const marker = markers.current.get(tabId)?.get(eventId);
    // A trimmed line has no valid position anymore: no-op (the UI grays the
    // event out via staleEvents, but double-guard here).
    if (!term || !marker || marker.isDisposed) return;

    term.scrollToLine(marker.line);

    // Replace a still-showing highlight from a previous jump, so rapid clicks
    // never leave ghost decorations or timers behind.
    if (highlight.current) {
      window.clearTimeout(highlight.current.timer);
      highlight.current.decoration.dispose();
      highlight.current = null;
    }
    const decoration = term.registerDecoration({
      marker,
      width: term.cols,
      height: 1,
      backgroundColor: HIGHLIGHT_COLOR,
    });
    if (!decoration) return;
    highlight.current = {
      decoration,
      timer: window.setTimeout(() => {
        decoration.dispose();
        if (highlight.current?.decoration === decoration) highlight.current = null;
      }, HIGHLIGHT_MS),
    };
    // Focus deliberately stays in the timeline panel: consecutive jumps
    // should not each cost a click back into the panel.
  }, []);

  const dismissError = useCallback(() => setError(null), []);

  // The event listeners below are registered once for the lifetime of the app,
  // so they call through a ref that always points at the freshest closures.
  const actions = { createTab, closeActiveTab, selectTabByIndex, cycleTab, switchTab, jumpToNextWaiting };
  const actionsRef = useRef(actions);
  actionsRef.current = actions;

  useEffect(() => {
    let disposed = false;
    let unlistenOutput: UnlistenFn | undefined;
    const listeners: Promise<UnlistenFn>[] = [];

    invoke<TabResponse[]>('list_tabs')
      .then((loaded) => {
        if (disposed) return;
        const snapshot = loaded.map(toTab);
        const known = new Set(snapshot.map((tab) => tab.id));
        setTabs((prev) => [
          ...snapshot,
          // A tab created while this snapshot was in flight must not disappear
          // just because the backend computed its answer beforehand.
          ...prev.filter((tab) => !known.has(tab.id)),
        ]);
        setActiveTabId((current) => current ?? snapshot[0]?.id ?? null);
      })
      .catch((err) => report(setError, 'Could not load existing tabs', err));

    listen<PtyOutputPayload>('pty-output', (event) => {
      const handler = handlers.current.get(event.payload.tab_id);
      if (handler) handler(base64ToBytes(event.payload.data), event.payload.seq);
    })
      .then((fn) => {
        if (disposed) {
          fn();
          return;
        }
        unlistenOutput = fn;
      })
      .catch((err) => report(setError, 'Could not subscribe to terminal output', err))
      // Resolve even when the subscription failed: otherwise every attachTab
      // awaits outputReady forever and the terminals stay blank with no clue.
      .finally(() => outputReady.resolve());

    listeners.push(
      listen<TabTitlePayload>('tab-title', ({ payload }) => {
        setTabs((prev) =>
          prev.map((tab) =>
            tab.id === payload.tab_id
              ? {
                  ...tab,
                  title: payload.title,
                  // The backend already classified this; trusting it keeps the
                  // renderer from drifting from the backend's own heuristic.
                  hasClaudeTitle: payload.program_title,
                }
              : tab
          )
        );
      }),
      listen<TabCwdPayload>('tab-cwd', ({ payload }) => {
        setTabs((prev) =>
          prev.map((tab) =>
            tab.id === payload.tab_id
              ? {
                  ...tab,
                  cwd: payload.cwd,
                  // Keep a program's title; otherwise the path is the title.
                  title: tab.hasClaudeTitle ? tab.title : payload.cwd,
                }
              : tab
          )
        );
      }),
      listen<TabStatusPayload>('tab-status', ({ payload }) => {
        // The backend sends the tab's complete protocol state; replace both
        // fields rather than merging, so a cleared notice really disappears.
        setTabs((prev) =>
          prev.map((tab) =>
            tab.id === payload.tab_id
              ? { ...tab, status: payload.status, notice: payload.notice }
              : tab
          )
        );

        // Timeline: derive events from the state transition, then bind each
        // new event to the terminal line it arrived at.
        const tabId = payload.tab_id;
        let tracker = trackers.current.get(tabId);
        if (!tracker) {
          tracker = new TimelineTracker();
          trackers.current.set(tabId, tracker);
        }
        const newEvents = tracker.push({ status: payload.status, notice: payload.notice });
        if (newEvents.length === 0) return;

        const existing = markers.current.get(tabId);
        const markerMap = existing ?? new Map<number, IMarker>();
        if (!existing) markers.current.set(tabId, markerMap);

        const term = getTerm(tabId);
        for (const ev of newEvents) {
          // No terminal mounted yet (mid-attach): record the event anyway,
          // but gray it out — there is no terminal line to jump to, so it
          // must not present itself as navigable.
          const marker = term?.registerMarker(0);
          if (!marker) {
            setStaleEvents((prev) => new Set(prev).add(`${tabId}:${ev.id}`));
            continue;
          }
          markerMap.set(ev.id, marker);
          const key = `${tabId}:${ev.id}`;
          marker.onDispose(() => {
            // Skip when the dispose was cleanup we initiated ourselves (tab
            // closed, event dropped by the cap): the map entry is already
            // gone in those cases, and the event is no longer rendered.
            if (!markers.current.get(tabId)?.has(ev.id)) return;
            setStaleEvents((prev) => new Set(prev).add(key));
          });
        }

        setTimelines((prev) => {
          const merged = [...(prev[tabId] ?? []), ...newEvents];
          if (merged.length <= MAX_TIMELINE_EVENTS) return { ...prev, [tabId]: merged };
          // Cap reached: drop the oldest events and release their markers.
          // Delete from the map *before* dispose so the onDispose guard above
          // sees them as cleaned-up, not as trimmed lines.
          for (const ev of merged.slice(0, merged.length - MAX_TIMELINE_EVENTS)) {
            const marker = markerMap.get(ev.id);
            markerMap.delete(ev.id);
            marker?.dispose();
          }
          return { ...prev, [tabId]: merged.slice(-MAX_TIMELINE_EVENTS) };
        });
      }),
      listen<TabFlashPayload>('tab-flash', ({ payload }) => {
        // Flashing the tab the user is already looking at is pure noise.
        if (payload.tab_id === activeTabRef.current) return;
        setTabs((prev) =>
          prev.map((tab) => (tab.id === payload.tab_id ? { ...tab, flashing: true } : tab))
        );
      }),
      listen<TabFlashPayload>('prompt-ready', ({ payload }) => {
        setTabs((prev) =>
          prev.map((tab) =>
            tab.id === payload.tab_id
              ? { ...tab, hasClaudeTitle: false, title: tab.cwd }
              : tab
          )
        );
      }),
      listen<TabWaitingPayload>('tab-waiting', ({ payload }) => {
        setTabs((prev) =>
          prev.map((tab) =>
            tab.id === payload.tab_id ? { ...tab, waiting: payload.waiting } : tab
          )
        );
      }),
      listen<FocusTabPayload>('focus-tab', ({ payload }) => {
        // The tab may have been closed while its notification sat in the
        // notification center; switching to a ghost would blank the UI.
        if (tabsRef.current.some((tab) => tab.id === payload.tab_id)) {
          actionsRef.current.switchTab(payload.tab_id);
        }
      }),
      listen<TabExitPayload>('tab-exit', ({ payload }) => {
        // The shell is gone; the backend has already dropped the session.
        abandonTab(payload.tab_id);
      }),
      listen<MenuShortcutPayload>('menu-shortcut', ({ payload }) => {
        const { current: actions } = actionsRef;
        const selectTab = /^select-tab-(\d)$/.exec(payload.id);
        if (selectTab) {
          const number = Number(selectTab[1]);
          const list = tabsRef.current;
          // ⌘9 means "the last tab", like every other macOS app.
          actions.selectTabByIndex(number === 9 ? list.length - 1 : number - 1);
          return;
        }
        switch (payload.id) {
          case 'new-tab':
            void actions.createTab();
            break;
          case 'close-tab':
            void actions.closeActiveTab();
            break;
          case 'next-tab':
            actions.cycleTab(1);
            break;
          case 'prev-tab':
            actions.cycleTab(-1);
            break;
          case 'next-waiting':
            actions.jumpToNextWaiting();
            break;
        }
      }),
      // A tab the backend created itself: the delayed startup tab, or the
      // Finder "New clitab Tab Here" service. Same id-dedup as createTab —
      // the mount-time list_tabs snapshot may already contain it.
      listen<TabResponse>('tab-created', ({ payload }) => {
        const created = toTab(payload);
        setTabs((prev) =>
          prev.some((tab) => tab.id === created.id)
            ? prev.map((tab) => (tab.id === created.id ? created : tab))
            : [...prev, created]
        );
        setActiveTabId(created.id);
      })
    );

    return () => {
      disposed = true;
      unlistenOutput?.();
      // Unsubscribing races with `listen()` resolving: hand each promise to the
      // cleanup so the listener is removed as soon as it exists.
      listeners.forEach((promise) => promise.then((fn) => fn()));
    };
  }, [abandonTab, outputReady]);

  return {
    tabs,
    activeTabId,
    error,
    dismissError,
    createTab,
    closeTab,
    closeActiveTab,
    switchTab,
    selectTabByIndex,
    cycleTab,
    attachTab,
    detachTab,
    writeInput,
    resizePty,
    timelines,
    staleEvents,
    navigateToEvent,
  };
}

function report(setError: (message: string | null) => void, prefix: string, err: unknown) {
  setError(`${prefix}: ${messageOf(err)}`);
}

function messageOf(err: unknown): string {
  if (typeof err === 'string') return err;
  if (err instanceof Error) return err.message;
  return String(err);
}
