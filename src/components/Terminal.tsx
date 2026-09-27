import React, { useEffect, useRef } from 'react';
import { Terminal as XTerm } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';

interface TerminalProps {
  tabId: string;
  isActive: boolean;
  /** Subscribe to this tab's output; resolves once the live stream is flowing. */
  attach: (tabId: string, write: (chunk: Uint8Array) => void) => Promise<void>;
  detach: (tabId: string) => void;
  onInput: (tabId: string, data: Uint8Array) => void;
  onResize: (tabId: string, rows: number, cols: number) => void;
}

/** Below this the container is hidden (or mid-animation) and cannot be measured. */
const MIN_SIZE = 10;
/** Debounce so dragging the window does not spam the PTY with resize ioctls. */
const RESIZE_DEBOUNCE_MS = 60;

export const Terminal: React.FC<TerminalProps> = ({
  tabId,
  isActive,
  attach,
  detach,
  onInput,
  onResize,
}) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const termRef = useRef<XTerm | null>(null);
  // The terminal is created once per tab, so the callbacks it calls must not be
  // baked into the effect's dependency list (that would tear down the PTY view
  // on every parent render).
  const callbacks = useRef({ attach, detach, onInput, onResize });
  callbacks.current = { attach, detach, onInput, onResize };

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    let disposed = false;
    let resizeTimer: number | undefined;

    const term = new XTerm({
      cursorBlink: true,
      fontSize: 14,
      fontFamily: '"JetBrains Mono", "Fira Code", "Cascadia Code", Menlo, monospace',
      scrollback: 5000,
      theme: {
        background: '#1e1e2e',
        foreground: '#cdd6f4',
        cursor: '#f5e0dc',
        selectionBackground: '#585b7066',
      },
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(container);
    fitRef.current = fitAddon;
    termRef.current = term;

    // No WebGL renderer, on purpose. Every tab keeps its terminal mounted (hidden
    // ones use `visibility: hidden` so they stay measurable), so one GL context
    // per tab means WKWebView eventually reclaims the oldest and fires
    // `webglcontextlost` in a live tab. Recovering means swapping renderers, and
    // xterm's swap disposes the old renderer before the fallback is re-attached:
    // `RenderService.dimensions` is an unguarded `this._renderer.value.dimensions`,
    // so one scroll tick landing after a teardown throws `undefined is not an
    // object`. The DOM renderer has no context budget and nothing to swap; reach
    // for @xterm/addon-canvas (2D canvas, no GL context) if output ever gets slow.

    term.onData((data) => {
      callbacks.current.onInput(tabId, new TextEncoder().encode(data));
    });

    term.onResize(({ cols, rows }) => {
      if (cols < 1 || rows < 1) return;
      window.clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(() => {
        if (!disposed) callbacks.current.onResize(tabId, rows, cols);
      }, RESIZE_DEBOUNCE_MS);
    });

    const fit = () => {
      // A hidden container reports 0x0; fitting it would shrink the PTY and
      // leave the shell wrapping lines for a size nobody can see.
      if (container.offsetWidth < MIN_SIZE || container.offsetHeight < MIN_SIZE) return;
      try {
        fitAddon.fit();
      } catch (err) {
        console.debug('fit failed', err);
      }
    };

    // Covers window resizes, tab switches and sidebar changes — no need for a
    // `window.addEventListener('resize')` that would also hit hidden tabs.
    const observer = new ResizeObserver(fit);
    observer.observe(container);
    fit();

    // Replay whatever the PTY produced before we were listening, then stream.
    void callbacks.current.attach(tabId, (chunk) => {
      if (!disposed) term.write(chunk);
    });

    return () => {
      disposed = true;
      window.clearTimeout(resizeTimer);
      observer.disconnect();
      callbacks.current.detach(tabId);
      if (fitRef.current === fitAddon) fitRef.current = null;
      if (termRef.current === term) termRef.current = null;
      term.dispose();
    };
  }, [tabId]);

  // A tab that was hidden gets its box back on activation; ResizeObserver is
  // not guaranteed to fire for a display:none -> block change, so fit directly.
  // Switching tabs must also move the caret, or keystrokes go to the sidebar.
  useEffect(() => {
    const container = containerRef.current;
    if (!isActive || !container) return;
    if (container.offsetWidth < MIN_SIZE || container.offsetHeight < MIN_SIZE) return;
    fitRef.current?.fit();
    termRef.current?.focus();
  }, [isActive]);

  return <div ref={containerRef} className="terminal-container" />;
};
