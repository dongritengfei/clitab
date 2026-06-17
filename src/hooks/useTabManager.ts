import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Tab, PtyOutputPayload, TabTitlePayload, TabCwdPayload, TabFlashPayload, TabExitPayload } from '../types';

export function useTabManager() {
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);
  const [outputHandlers, setOutputHandlers] = useState<Map<string, (data: Uint8Array) => void>>(new Map());

  // Initialize: load tabs from backend
  useEffect(() => {
    invoke<Tab[]>('list_tabs').then((loadedTabs) => {
      setTabs(loadedTabs);
      if (loadedTabs.length > 0 && loadedTabs[0]) {
        setActiveTabId(loadedTabs[0].id);
      }
    });
  }, []);

  // Listen to PTY output events
  useEffect(() => {
    const unlisten = listen<PtyOutputPayload>('pty-output', (event) => {
      const { tab_id, data } = event.payload;
      const handler = outputHandlers.get(tab_id);
      if (handler) {
        handler(new Uint8Array(data));
      }
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [outputHandlers]);

  // Listen to tab title changes
  useEffect(() => {
    const unlisten = listen<TabTitlePayload>('tab-title', (event) => {
      const { tab_id, title } = event.payload;
      setTabs((prev) =>
        prev.map((tab) => (tab.id === tab_id ? { ...tab, title, hasClaudeTitle: true } : tab))
      );
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Listen to cwd changes (OSC 7)
  useEffect(() => {
    const unlisten = listen<TabCwdPayload>('tab-cwd', (event) => {
      const { tab_id, cwd } = event.payload;
      setTabs((prev) =>
        prev.map((tab) => {
          if (tab.id !== tab_id) return tab;
          // If Claude has set a title, keep it; otherwise use cwd
          return {
            ...tab,
            cwd,
            title: tab.hasClaudeTitle ? tab.title : cwd,
          };
        })
      );
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Listen to tab flash events
  useEffect(() => {
    const unlisten = listen<TabFlashPayload>('tab-flash', (event) => {
      const { tab_id } = event.payload;
      setTabs((prev) =>
        prev.map((tab) =>
          tab.id === tab_id && tab.id !== activeTabId ? { ...tab, flashing: true } : tab
        )
      );
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [activeTabId]);

  // Listen to prompt ready events (reset Claude title flag)
  useEffect(() => {
    const unlisten = listen<TabFlashPayload>('prompt-ready', (event) => {
      const { tab_id } = event.payload;
      setTabs((prev) =>
        prev.map((tab) =>
          tab.id === tab_id
            ? { ...tab, hasClaudeTitle: false, title: tab.cwd }
            : tab
        )
      );
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Listen to tab exit events
  useEffect(() => {
    const unlisten = listen<TabExitPayload>('tab-exit', (event) => {
      const { tab_id } = event.payload;
      // When process exits, fall back to cwd
      setTabs((prev) =>
        prev.map((tab) =>
          tab.id === tab_id ? { ...tab, title: tab.cwd } : tab
        )
      );
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const createTab = useCallback(async () => {
    const newTab = await invoke<Tab>('create_tab');
    setTabs((prev) => [...prev, newTab]);
    setActiveTabId(newTab.id);
    return newTab;
  }, []);

  const closeTab = useCallback(async (tabId: string) => {
    const hasProcess = await invoke<boolean>('has_active_process', { tabId });

    if (hasProcess) {
      const { ask } = await import('@tauri-apps/plugin-dialog');
      const confirmed = await ask('Terminal has an active process. Close anyway?', {
        title: 'Confirm Close',
        kind: 'warning',
      });

      if (!confirmed) {
        return false;
      }
    }

    await invoke('close_tab', { tabId });
    setTabs((prev) => {
      const filtered = prev.filter((t) => t.id !== tabId);
      if (activeTabId === tabId && filtered.length > 0 && filtered[0]) {
        setActiveTabId(filtered[0].id);
      } else if (filtered.length === 0) {
        setActiveTabId(null);
      }
      return filtered;
    });
    return true;
  }, [activeTabId]);

  const switchTab = useCallback(async (tabId: string) => {
    await invoke('switch_tab', { tabId });
    setActiveTabId(tabId);
    setTabs((prev) =>
      prev.map((tab) => (tab.id === tabId ? { ...tab, flashing: false } : tab))
    );
  }, []);

  const registerOutputHandler = useCallback((tabId: string, handler: (data: Uint8Array) => void) => {
    setOutputHandlers((prev) => {
      const next = new Map(prev);
      next.set(tabId, handler);
      return next;
    });
  }, []);

  const unregisterOutputHandler = useCallback((tabId: string) => {
    setOutputHandlers((prev) => {
      const next = new Map(prev);
      next.delete(tabId);
      return next;
    });
  }, []);

  const writeInput = useCallback(async (tabId: string, data: Uint8Array) => {
    await invoke('pty_input', { tabId, data: Array.from(data) });
  }, []);

  const resizePty = useCallback(async (tabId: string, rows: number, cols: number) => {
    await invoke('resize_pty', { tabId, rows, cols });
  }, []);

  return {
    tabs,
    activeTabId,
    createTab,
    closeTab,
    switchTab,
    registerOutputHandler,
    unregisterOutputHandler,
    writeInput,
    resizePty,
  };
}
