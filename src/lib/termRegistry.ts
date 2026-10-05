import type { Terminal as XTerm } from '@xterm/xterm';

/**
 * tabId → live xterm instance. React state cannot hold the terminal (it is a
 * mutable native-ish object owned by Terminal.tsx's mount effect), and the
 * timeline needs the instance to bind events to buffer lines. This module is
 * the single place that maps a tab id back to its terminal.
 */
const terms = new Map<string, XTerm>();

export function registerTerm(tabId: string, term: XTerm): void {
  terms.set(tabId, term);
}

/**
 * Unregister only if `term` is still the registered instance: React StrictMode
 * remounts register the new instance before the old cleanup runs, and the old
 * cleanup must not delete the new entry.
 */
export function unregisterTerm(tabId: string, term: XTerm): void {
  if (terms.get(tabId) === term) terms.delete(tabId);
}

export function getTerm(tabId: string): XTerm | undefined {
  return terms.get(tabId);
}
