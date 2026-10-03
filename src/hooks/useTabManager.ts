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
import type { IDecoration, IMarker, Terminal as XTerm } from '@xterm/xterm';

/** `isReplay` marks the attach-time ring replay, as opposed to live output.
 * `done` rides on fence writes (see the tab-status listener): xterm invokes it
 * once that write — and everything queued before it — has been parsed. */
type OutputHandler = (chunk: Uint8Array, isReplay?: boolean, done?: () => void) => void;
/** Internal sink: a live chunk plus its absolute position in the byte stream.
 * An empty chunk with `done` set is a fence: no bytes, just a callback ordered
 * behind every write queued before it. */
type StreamSink = (chunk: Uint8Array, seq: number, done?: () => void) => void;

/** Zero-length chunk used to queue a marker-registration fence. */
const FENCE = new Uint8Array(0);

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
  /** ⌘F find bar for the active terminal: 0 = closed; each increment (from
   * the `find` menu shortcut) is a fresh open/refocus request. Renderer-only,
   * like the timeline. */
  searchNonce: number;
  closeSearch: () => void;
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
  const [searchNonce, setSearchNonce] = useState(0);
  const [timelines, setTimelines] = useState<Record<string, TimelineEvent[]>>({});
  const [staleEvents, setStaleEvents] = useState<ReadonlySet<string>>(() => new Set());

  // Timeline transition detection is per-tab stateful. Markers bind an event
  // to its terminal line; they are mutable xterm objects, so they live in a
  // ref (tabId → eventId → marker), never in state.
  const trackers = useRef(new Map<string, TimelineTracker>());
  const markers = useRef(new Map<string, Map<number, IMarker>>());
  // Mirror of the timelines state so navigateToEvent can look up sibling
  // events without joining the callback's dependency list.
  const timelinesRef = useRef(timelines);
  timelinesRef.current = timelines;
  // The one highlight decoration currently on screen, its expiry timer, and
  // the throwaway marker it rides on (disposed together with it).
  const highlight = useRef<{ decoration: IDecoration; owned: IMarker; timer: number } | null>(
    null
  );

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

  // ⌘F: every press is a fresh search — incrementing the nonce opens the bar
  // and, when it is already open, tells SearchBar to reset and refocus.
  const openSearch = useCallback(() => setSearchNonce((n) => n + 1), []);
  const closeSearch = useCallback(() => setSearchNonce(0), []);

  const attachTab = useCallback(
    async (tabId: string, handler: OutputHandler) => {
      // Anything arriving while we catch up is queued, so the replay is always
      // written before the live stream instead of being interleaved with it.
      let live = false;
      const queued: { chunk: Uint8Array; seq: number; done?: () => void }[] = [];
      const subscription: StreamSink = (chunk, seq, done) => {
        if (live) handler(chunk, false, done);
        else queued.push({ chunk, seq, done });
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

      for (const { chunk, seq, done } of queued) {
        if (done) {
          // Fence: carries no bytes, so there is nothing to dedup — forward
          // as-is to keep its callback ordered behind the writes queued
          // before it (the replay and any live chunks already drained).
          handler(chunk, false, done);
        } else if (seq >= replayEnd) {
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
    const markerMap = markers.current.get(tabId);
    const marker = markerMap?.get(eventId);
    // A trimmed line has no valid position anymore: no-op (the UI grays the
    // event out via staleEvents, but double-guard here).
    if (!term || !marker || marker.isDisposed) return;

    // Markers are registered when the event's OSC arrives, but ink can
    // rewrite the anchored line afterwards: a prompt queued mid-turn fires
    // its OSC at queue time, anchoring on the transient queue display (the
    // real transcript entry is committed only when the previous turn stops,
    // arbitrarily later), and a turn-end anchored on the spinner block can
    // shift when a queued next turn commits immediately. By click time
    // everything has settled, so relocate here: a turn-start jumps to where
    // its prompt text actually lives; a turn-end uses the settled layout
    // invariant [done summary][blank][next entry] and jumps two lines above
    // the next turn's (relocated) entry.
    const events = timelinesRef.current[tabId] ?? [];
    const ev = events.find((e) => e.id === eventId);
    let line = marker.line;
    if (ev?.kind === 'turn-start') {
      line = relocatePromptLine(term, marker.line, ev.msg) ?? line;
    } else if (ev?.kind === 'turn-end') {
      // FIFO pairing: the k-th turn-end ends the k-th turn-start's turn, so
      // the next turn's entry belongs to the (k+1)-th turn-start. A queued
      // prompt's turn-start fires *before* this turn-end arrives, so
      // scanning events after this one would miss it — index by rank.
      const idx = events.findIndex((e) => e.id === eventId);
      let rank = 0;
      for (let j = 0; j <= idx; j++) {
        if (events[j]?.kind === 'turn-end') rank++;
      }
      const starts = events.filter((e) => e.kind === 'turn-start');
      const next = starts[rank];
      const nextMarker = next ? markerMap?.get(next.id) : undefined;
      if (next && nextMarker && !nextMarker.isDisposed) {
        const nextLine = relocatePromptLine(term, nextMarker.line, next.msg) ?? nextMarker.line;
        line = Math.max(0, nextLine - TURN_END_DONE_GAP);
      }
    }
    // Center the target line in the viewport when the buffer allows, so the
    // user sees context above and below it (scrollToLine would pin it to the
    // top edge). Clamped at both ends: near the buffer start or end the
    // viewport just rests at the nearest valid position.
    const buffer = term.buffer.active;
    const centered = Math.min(
      Math.max(0, line - Math.floor(term.rows / 2)),
      Math.max(0, buffer.length - term.rows)
    );
    term.scrollLines(centered - buffer.viewportY);

    // Replace a still-showing highlight from a previous jump, so rapid clicks
    // never leave ghost decorations or timers behind.
    if (highlight.current) {
      window.clearTimeout(highlight.current.timer);
      highlight.current.decoration.dispose();
      highlight.current.owned.dispose();
      highlight.current = null;
    }
    // The decoration rides a throwaway marker so it lands on the relocated
    // line even when the event's own marker names a rewritten one.
    const owned = term.registerMarker(line - (buffer.baseY + buffer.cursorY));
    if (!owned) return;
    const decoration = term.registerDecoration({
      marker: owned,
      width: term.cols,
      height: 1,
      backgroundColor: HIGHLIGHT_COLOR,
    });
    if (!decoration) {
      owned.dispose();
      return;
    }
    highlight.current = {
      decoration,
      owned,
      timer: window.setTimeout(() => {
        decoration.dispose();
        owned.dispose();
        if (highlight.current?.decoration === decoration) highlight.current = null;
      }, HIGHLIGHT_MS),
    };
    // Focus deliberately stays in the timeline panel: consecutive jumps
    // should not each cost a click back into the panel.
  }, []);

  const dismissError = useCallback(() => setError(null), []);

  // The event listeners below are registered once for the lifetime of the app,
  // so they call through a ref that always points at the freshest closures.
  const actions = {
    createTab,
    closeActiveTab,
    selectTabByIndex,
    cycleTab,
    switchTab,
    jumpToNextWaiting,
    openSearch,
  };
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
        const newEvents = tracker.push({
          status: payload.status,
          notice: payload.notice,
          answer: payload.answer,
        });
        if (newEvents.length === 0) return;

        const existing = markers.current.get(tabId);
        const markerMap = existing ?? new Map<number, IMarker>();
        if (!existing) markers.current.set(tabId, markerMap);

        const term = getTerm(tabId);
        const sink = handlers.current.get(tabId);
        for (const ev of newEvents) {
          const key = `${tabId}:${ev.id}`;
          // No terminal mounted yet (mid-attach): record the event anyway,
          // but gray it out — there is no terminal line to jump to, so it
          // must not present itself as navigable.
          if (!term || !sink) {
            setStaleEvents((prev) => new Set(prev).add(key));
            continue;
          }
          // The backend emits this event *after* the output bytes preceding
          // its OSC sequence, but xterm parses writes asynchronously: reading
          // the cursor now would see a stale line. Queue an empty fence write
          // through the same sink the chunks travel, and register the marker
          // in its callback — by then the cursor sits exactly where the OSC
          // sequence arrived, so the marker lands on the event's own line.
          sink(FENCE, 0, () => {
            // The tab may have closed or its terminal remounted while the
            // fence was queued; both unregister before disposing, and the
            // public Terminal type exposes no isDisposed to check directly.
            if (getTerm(tabId) !== term || markers.current.get(tabId) !== markerMap) return;
            // The OSC lands while ink's cursor sits at the bottom of its UI,
            // but the lines users think of are a few rows higher: for a
            // turn-start, the transcript entry showing the submitted prompt;
            // for a turn-end, the spinner block above the input box. Anchor
            // each to its content line instead of the raw cursor line.
            let offset = 0;
            if (ev.kind === 'turn-start') offset = turnStartOffset(term, ev.msg) ?? 0;
            else if (ev.kind === 'turn-end') offset = turnEndOffset(term);
            const marker = term.registerMarker(offset);
            if (!marker) {
              setStaleEvents((prev) => new Set(prev).add(key));
              return;
            }
            markerMap.set(ev.id, marker);
            const bind = (m: IMarker) =>
              m.onDispose(() => {
                // Skip when the dispose was cleanup we initiated ourselves
                // (tab closed, event dropped by the cap): the map entry is
                // already gone in those cases, and the event is either no
                // longer rendered or still navigable.
                if (markers.current.get(tabId) !== markerMap) return;
                if (markerMap.get(ev.id) !== m) return;
                setStaleEvents((prev) => new Set(prev).add(key));
              });
            bind(marker);
            // The anchor can still end up stranded: ink may rewrite the line
            // afterwards (the OSC beating the commit repaint, or a queued
            // prompt's transient box display consumed at stop time).
            // navigateToEvent relocates at click time, when everything has
            // settled.
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
          case 'find':
            // The bar searches the active terminal; with no tab open there is
            // nothing to search and no caret to hand focus back to.
            if (activeTabRef.current) actions.openSearch();
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
    searchNonce,
    closeSearch,
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

/** How far above the cursor to look for a turn's content line. */
const ANCHOR_SCAN_LINES = 30;
/** Longest prompt prefix used as the search needle. */
const ANCHOR_PROBE_LEN = 40;
/** How far to climb to the top of an anchored block (spinner blocks are 2–3 lines). */
const ANCHOR_BLOCK_CLIMB = 10;

/** The prompt-text needle for `msg`: first line, capped, with a flag telling
 * `promptLineMatches` whether the probe is the whole line. */
function promptProbe(msg: string | null | undefined): { probe: string; whole: boolean } | null {
  const firstLine = msg?.split('\n')[0]?.trim();
  if (!firstLine) return null;
  const probe = firstLine.slice(0, ANCHOR_PROBE_LEN);
  return { probe, whole: probe === firstLine };
}

/**
 * The UserPromptSubmit hook's OSC reaches the PTY while ink's cursor is parked
 * at the bottom of its UI (input box, hints), but the submitted prompt is
 * already drawn a few lines up in the transcript — a marker on the raw cursor
 * line lands below the line the user means by "this turn started". Scan
 * upwards for the line showing the prompt text and return that line as a
 * (negative) registerMarker offset, or null when the text is not visible
 * (hook without jq, or the transcript repaint has not happened yet — the
 * caller falls back to the cursor line and may re-anchor later).
 */
function turnStartOffset(term: XTerm, msg: string | null | undefined): number | null {
  const needle = promptProbe(msg);
  if (!needle) return null;
  const buffer = term.buffer.active;
  const cursor = buffer.baseY + buffer.cursorY;
  let fallback: number | null = null;
  for (let line = cursor; line >= Math.max(0, cursor - ANCHOR_SCAN_LINES); line--) {
    const text = buffer.getLine(line)?.translateToString(true);
    if (!text || !promptLineMatches(text, needle.probe, needle.whole)) continue;
    // The transcript entry ("❯ prompt") wins over a bare text match: the
    // response body can quote the prompt's words back (common with short
    // CJK prompts like "再来一次"), and the nearest raw match would then sit
    // mid-response instead of on the entry the user means.
    if (text.trimStart().startsWith('❯')) return line - cursor;
    fallback ??= line - cursor;
  }
  return fallback;
}

/** How many lines the box-region skip may consume (multi-line queued text + borders). */
const ANCHOR_BOX_SKIP = 10;

/**
 * The stop hook's OSC lands while ink still shows its busy frame: spinner
 * block, a blank, the input-box border rules, and the cursor inside the box.
 * Everything down there is dynamic — the next turn's commit repaint erases
 * and rewrites it, stranding a raw-cursor marker inside the *next* turn's
 * content. The box may even carry text: Claude Code queues a prompt typed
 * mid-turn and submits it the instant this turn stops, so at stop time the
 * box holds the next prompt — content-looking text on a line that is about
 * to be rewritten. The turn's actual last line is the top of the spinner
 * block: ink pins the block's top row when it rewrites it in place into the
 * "✻ … done" summary, which then stays put as static transcript.
 *
 * So: skip everything from the cursor (parked inside the box) through the
 * box's top border rule — that whole region is the box, whatever it
 * contains — then blanks and stray chrome, then anchor on the first content
 * line and climb to the top of its contiguous block (the spinner can carry
 * sub-lines like "⎿  Tip: …" that the done frame erases).
 */
function turnEndOffset(term: XTerm): number {
  const buffer = term.buffer.active;
  const cursor = buffer.baseY + buffer.cursorY;
  const floor = Math.max(0, cursor - ANCHOR_SCAN_LINES);
  const textAt = (line: number): string =>
    buffer.getLine(line)?.translateToString(true)?.trim() ?? '';
  const isBorder = (t: string): boolean => /^[\u2500-\u257F\s]+$/.test(t);
  // ink's bottom-of-UI chrome: blanks, the empty box prompt, border rules.
  const isChrome = (t: string): boolean => !t || t === '\u276f' || isBorder(t);
  // Box region skip. Without a border in sight (unexpected layout), the
  // budget keeps the skip bounded and the phases below still anchor on the
  // nearest content line.
  let line = cursor;
  let budget = ANCHOR_BOX_SKIP;
  while (line >= floor && budget-- > 0) {
    const border = textAt(line) !== '' && isBorder(textAt(line));
    line--;
    if (border) break;
  }
  while (line >= floor && isChrome(textAt(line))) line--;
  if (line < floor) return 0;
  let top = line;
  while (top - 1 >= Math.max(floor, line - ANCHOR_BLOCK_CLIMB) && !isChrome(textAt(top - 1))) {
    top--;
  }
  return top - cursor;
}

/** Settled Claude Code layout: [✻ … done summary][blank][❯ next turn entry] —
 * a turn's done line lives this many lines above the next turn's entry. */
const TURN_END_DONE_GAP = 2;

/**
 * Click-time relocation for a stranded turn-start marker (see navigateToEvent).
 * A marker's line can stop showing its prompt text after ink rewrites it: the
 * OSC may beat the commit repaint, or a prompt queued mid-turn fires its OSC
 * at queue time, anchoring on the transient "❯ <queued text>" box display —
 * that display is consumed the moment the previous turn stops, and the real
 * transcript entry is committed later, arbitrarily far below. At click time
 * everything has settled, so if the marker's line no longer matches the probe,
 * rescan the whole buffer for ❯-prefixed matches: prefer the nearest one
 * *below* the stranded line (later-committed entries live below), else the
 * nearest above. Returns null when the line still matches (nothing stranded)
 * or the prompt text appears nowhere (unlocatable — the caller keeps the
 * marker's line).
 */
function relocatePromptLine(
  term: XTerm,
  line: number,
  msg: string | null | undefined
): number | null {
  const needle = promptProbe(msg);
  if (!needle) return null;
  const buffer = term.buffer.active;
  const at = buffer.getLine(line)?.translateToString(true) ?? '';
  if (promptLineMatches(at, needle.probe, needle.whole)) return null;
  let below: number | null = null;
  let above: number | null = null;
  for (let i = 0; i < buffer.length; i++) {
    const text = buffer.getLine(i)?.translateToString(true);
    if (!text || !text.trimStart().startsWith('❯')) continue;
    if (!promptLineMatches(text, needle.probe, needle.whole)) continue;
    if (i > line && below === null) below = i;
    else if (i < line) above = i;
  }
  return below ?? above;
}

/** Substring match with word-boundary guards, so an ink chrome line like
 * "hint" cannot anchor a "hi" prompt. Boundaries only apply to ASCII word
 * characters (CJK has none), and only at the probe's end when the probe is
 * the whole first line — a truncated probe is followed by more prompt text. */
function promptLineMatches(text: string, probe: string, whole: boolean): boolean {
  const at = text.indexOf(probe);
  if (at < 0) return false;
  const word = /[A-Za-z0-9_]/;
  if (at > 0 && word.test(probe.charAt(0)) && word.test(text.charAt(at - 1))) return false;
  const after = text.charAt(at + probe.length); // '' when the probe ends the line
  if (whole && after && word.test(probe.charAt(probe.length - 1)) && word.test(after)) return false;
  return true;
}
