import React, { useEffect, useRef } from 'react';
import { Terminal as XTerm } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';

interface TerminalProps {
  tabId: string;
  isActive: boolean;
  /** Subscribe to this tab's output; resolves once the live stream is flowing. */
  attach: (tabId: string, write: (chunk: Uint8Array, isReplay?: boolean) => void) => Promise<void>;
  detach: (tabId: string) => void;
  onInput: (tabId: string, data: Uint8Array) => void;
  onResize: (tabId: string, rows: number, cols: number) => void;
}

/** Below this the container is hidden (or mid-animation) and cannot be measured. */
const MIN_SIZE = 10;
/** Debounce so dragging the window does not spam the PTY with resize ioctls. */
const RESIZE_DEBOUNCE_MS = 60;

/* macOS "tap to click" synthesizes the same mouse events for a trackpad tap
   as for a physical click, so "tap, then tap again and swipe" (tap-to-drag)
   reaches the web layer looking exactly like a deliberate drag — its moves
   even carry buttons=0, so event state cannot identify them. The one usable
   difference is rhythm: the swipe's mousedown lands right after the first
   tap's mouseup at the same spot, and its first real movement follows within
   a few hundred ms, while a deliberate press-hold-drag pauses longer before
   moving. A drag matching that signature has its mousemoves stopped before
   xterm's selection listener sees them, so nothing gets selected. */
/** Max time between the tap's mouseup and the swipe's mousedown. */
const TAP_SWIPE_GAP_MS = 300;
/** Max distance between those two points. */
const TAP_SWIPE_POS_PX = 24;
/** A drag that starts moving sooner than this after mousedown is a swipe.
    Measured on real trackpad data: a tap-to-drag swipe's first >3px move lands
    ~250-290ms after the synthesized mousedown (second-tap dwell plus swipe
    ramp-up), while a deliberate press-hold pauses longer before moving. */
const TAP_SWIPE_HOLD_MS = 400;

/** Cursor-ups in a replay above which the producer is a repaint-style TUI.
    Plain shell output emits essentially none; ink/Claude Code emits one per
    repaint, so a ring full of them means the replay starts mid-frame. */
const REDRAW_HEAVY_CURSOR_UPS = 8;

/**
 * Whether a replay must be dropped in favour of a clean start + forced
 * repaint. Repaint-style TUIs (ink/Claude Code on the normal screen, vim &
 * friends on the alternate screen) redraw with cursor-relative sequences
 * (cursor-up N, overwrite); a replay that starts mid-frame executes fewer
 * cursor-ups than the TUI expects, leaving xterm's cursor below the TUI's
 * assumed position, so every later repaint lands misaligned and ghost lines
 * accumulate. The replay cannot be repaired — only skipped.
 */
function replayNeedsCleanStart(bytes: Uint8Array): boolean {
  let inAlt = false;
  let cursorUps = 0;
  for (let i = 0; i + 3 <= bytes.length; i++) {
    if (bytes[i] !== 0x1b || bytes[i + 1] !== 0x5b) continue;
    let j = i + 2;
    const privateMode = bytes[j] === 0x3f;
    if (privateMode) j++;
    let mode = 0;
    for (;;) {
      const digit = bytes[j];
      if (digit === undefined || digit < 0x30 || digit > 0x39) break;
      mode = mode * 10 + (digit - 0x30);
      j++;
    }
    const finalByte = bytes[j];
    if (privateMode && (mode === 1049 || mode === 1047) && (finalByte === 0x68 || finalByte === 0x6c)) {
      inAlt = finalByte === 0x68; // 'h' enters, 'l' leaves
    } else if (!privateMode && finalByte === 0x41) {
      cursorUps += 1; // CSI n A
    }
    i = j;
  }
  return inAlt || cursorUps > REDRAW_HEAVY_CURSOR_UPS;
}

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
    // A replay of repaint-style TUI output is dropped (see
    // replayNeedsCleanStart): the misaligned cursor it leaves behind ghosts
    // every later repaint. The next repaint the TUI emits on its own (spinner
    // tick, keypress, window resize) then lands on the empty screen, where
    // its cursor-relative moves clamp to the top and resync — a forced
    // extra repaint would only risk two frames of differing heights
    // overwriting each other partially, which is its own ghost source.
    let replaySkipped = false;
    void callbacks.current
      .attach(tabId, (chunk, isReplay) => {
        if (disposed) return;
        if (isReplay && replayNeedsCleanStart(chunk)) {
          replaySkipped = true;
          return;
        }
        term.write(chunk);
      })
      .then(() => {
        if (!replaySkipped || disposed) return;
        // The program enabled modifyOtherKeys back at session start, long
        // before the ring window; re-assert it locally so shift+enter keeps
        // reporting as its own key instead of a plain CR in this new view.
        term.write('\x1b[>4;2m');
      });

    // Tap-swipe suppression (see the constants above for the rationale).
    let lastUp: { time: number; x: number; y: number } | null = null;
    let drag: {
      downTime: number;
      downX: number;
      downY: number;
      undecided: boolean;
      accidental: boolean;
    } | null = null;

    const onMouseDown = (event: MouseEvent) => {
      if (event.button !== 0) return;
      const nearLastUp =
        lastUp !== null &&
        event.timeStamp - lastUp.time < TAP_SWIPE_GAP_MS &&
        Math.abs(event.clientX - lastUp.x) < TAP_SWIPE_POS_PX &&
        Math.abs(event.clientY - lastUp.y) < TAP_SWIPE_POS_PX;
      drag = {
        downTime: event.timeStamp,
        downX: event.clientX,
        downY: event.clientY,
        undecided: nearLastUp,
        accidental: false,
      };
    };

    // Captured on window so a swipe that leaves the container is still held
    // back; stopping propagation here keeps the move from ever reaching
    // xterm's own document-level selection listener. `drag` alone gates this:
    // the synthesized tap-to-drag moves arrive with buttons=0, and xterm
    // extends its selection on any move between its own down and up.
    const onMouseMove = (event: MouseEvent) => {
      if (!drag) return;
      if (drag.undecided) {
        // Sub-3px motion is hand jitter, not a swipe; a double-click must keep
        // its word selection, so only real movement starts the hold clock.
        const dx = event.clientX - drag.downX;
        const dy = event.clientY - drag.downY;
        if (dx * dx + dy * dy < 9) return;
        const hold = event.timeStamp - drag.downTime;
        drag.accidental = hold < TAP_SWIPE_HOLD_MS;
        drag.undecided = false;
        // The double-tap half of the gesture may already have made xterm
        // select the word under the cursor; an accidental drag keeps nothing.
        if (drag.accidental) term.clearSelection();
      }
      if (drag.accidental) event.stopPropagation();
    };

    const onMouseUp = (event: MouseEvent) => {
      if (event.button === 0) {
        lastUp = { time: event.timeStamp, x: event.clientX, y: event.clientY };
      }
      drag = null;
    };

    container.addEventListener('mousedown', onMouseDown, true);
    window.addEventListener('mousemove', onMouseMove, true);
    window.addEventListener('mouseup', onMouseUp, true);

    return () => {
      disposed = true;
      window.clearTimeout(resizeTimer);
      observer.disconnect();
      container.removeEventListener('mousedown', onMouseDown, true);
      window.removeEventListener('mousemove', onMouseMove, true);
      window.removeEventListener('mouseup', onMouseUp, true);
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
