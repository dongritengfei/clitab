import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  TAB_GONE,
  type AttachStreamResponse,
  type MenuShortcutPayload,
  type PtyOutputPayload,
  type Tab,
  type TabCwdPayload,
  type TabExitPayload,
  type TabFlashPayload,
  type TabResponse,
  type TabTitlePayload,
} from '../types';
import { base64ToBytes, bytesToBase64 } from '../lib/base64';

type OutputHandler = (chunk: Uint8Array) => void;
/** Internal sink: a live chunk plus its absolute position in the byte stream. */
type StreamSink = (chunk: Uint8Array, seq: number) => void;

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
}

export function useTabManager(): TabManagerState {
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

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
      const created = toTab(await invoke<TabResponse>('create_tab'));
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
    setTabs((prev) => prev.map((tab) => (tab.id === tabId ? { ...tab, flashing: false } : tab)));
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
        if (bytes.length > 0) handler(bytes);
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

  const dismissError = useCallback(() => setError(null), []);

  // The event listeners below are registered once for the lifetime of the app,
  // so they call through a ref that always points at the freshest closures.
  const actions = { createTab, closeActiveTab, selectTabByIndex, cycleTab };
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
        }
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
