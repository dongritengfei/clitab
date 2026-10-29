import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Terminal as XTerm } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { registerTerm, unregisterTerm } from '../lib/termRegistry';
import {
  attachImeRecovery,
  attachKeyEncoding,
  attachTrackpadFilter,
  TERM_FONT_FAMILY,
  TERM_THEME,
} from '../lib/termInput';
import { InputStrip, type InputStripHandle } from './InputStrip';

interface TerminalProps {
  tabId: string;
  isActive: boolean;
  /** xterm font size, shared by every terminal (View ▸ Zoom, persisted by
   * useTabManager). Changes apply live via `options.fontSize` + refit. */
  fontSize: number;
  /** View ▸ Pinned Input Strip: master switch for the bottom mirror strip.
   * While on, typing never snaps a scrolled-up viewport to the bottom (the
   * live input line is readable in the strip); while off, xterm's native
   * scroll-on-input is restored. */
  stripEnabled: boolean;
  /** Each increment (⌘⇧B, from useTabManager) is a fresh request to pin the
   * strip visible and move the caret into it. */
  stripFocusNonce: number;
  /** Subscribe to this tab's output; resolves once the live stream is flowing.
   * `done` (fence writes only) fires once xterm has parsed that write and
   * everything queued before it. */
  attach: (
    tabId: string,
    write: (chunk: Uint8Array, isReplay?: boolean, done?: () => void) => void
  ) => Promise<void>;
  detach: (tabId: string) => void;
  onInput: (tabId: string, data: Uint8Array) => void;
  onResize: (tabId: string, rows: number, cols: number) => void;
}

/** Below this the container is hidden (or mid-animation) and cannot be measured. */
const MIN_SIZE = 10;
/** Debounce so dragging the window does not spam the PTY with resize ioctls. */
const RESIZE_DEBOUNCE_MS = 60;

/** Cursor-ups in a replay above which the producer is a repaint-style TUI.
    Plain shell output emits essentially none; ink/Claude Code emits one per
    repaint, so a ring full of them means the replay starts mid-frame. */
const REDRAW_HEAVY_CURSOR_UPS = 8;

/** What a replayed ring buffer contains, which decides how it may be used. */
type ReplayKind =
  /** Ordinary sequential output: replaying it reconstructs the screen. */
  | 'plain'
  /** Repaint-style TUI output on the normal screen (ink/Claude Code). The
      replay starts mid-frame, so executing it leaves the *visible* screen
      misaligned — but the content it pushes into the scrollback is exactly
      the recent history the user wants to keep browsing. Replay it, then
      home + ED2: that clears the visible screen without touching the
      scrollback, and the TUI's next repaint resyncs on the empty screen. */
  | 'redraw'
  /** The ring ends inside the alternate screen (vim, less, …). A full-screen
      app has no meaningful scrollback history, its repaint assumes alt-screen
      state, and the mid-replay clear semantics differ — drop it and let the
      app repaint itself. */
  | 'alt-screen';

/**
 * Classify a replay by scanning its escape sequences. Repaint-style TUIs
 * redraw with cursor-relative moves (cursor-up N, overwrite); a replay that
 * starts mid-frame executes fewer cursor-ups than the TUI expects, so a ring
 * full of them cannot reconstruct the visible screen — but it still carries
 * the recent output as scrollback (see ReplayKind).
 */
function classifyReplay(bytes: Uint8Array): ReplayKind {
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
  if (inAlt) return 'alt-screen';
  return cursorUps > REDRAW_HEAVY_CURSOR_UPS ? 'redraw' : 'plain';
}

export const Terminal: React.FC<TerminalProps> = ({
  tabId,
  isActive,
  fontSize,
  stripEnabled,
  stripFocusNonce,
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
  // Same for the font size: the creation effect reads the current value
  // through a ref; live changes go through the dedicated effect below.
  const fontSizeRef = useRef(fontSize);
  fontSizeRef.current = fontSize;

  /* Pinned input strip. Visibility is derived, never commanded: the strip
     shows while the active tab's viewport is away from the bottom (reading
     history — the agent's live input line would otherwise be off-screen) or
     while the mirror holds the caret (⌘⇧B pins it even at the bottom, where
     the overlay pixel-matches the screen underneath). Alt-screen apps are
     exempt: their bottom rows are their own UI. Every re-derivation point
     (scroll, parsed write, resize, enable toggle, activation) funnels into
     evaluateStrip. */
  const stripEnabledRef = useRef(stripEnabled);
  stripEnabledRef.current = stripEnabled;
  const isActiveRef = useRef(isActive);
  isActiveRef.current = isActive;
  const [stripVisible, setStripVisible] = useState(false);
  const stripVisibleRef = useRef(false);
  // The mirror has keyboard focus; pins the strip visible.
  const stripPinnedRef = useRef(false);
  // Mirror handle, plus the buffer catching live chunks between the
  // visibility decision and the mirror handshake. The buffer is *dropped* at
  // the handshake, not fed: those chunks were written to the primary before
  // InputStrip's snapshot fence, so the snapshot already contains them.
  const stripHandleRef = useRef<InputStripHandle | null>(null);
  const stripPendingRef = useRef<Uint8Array[] | null>(null);
  // A ⌘⇧B focus request that arrived before the mirror existed.
  const stripFocusPendingRef = useRef(false);

  const evaluateStrip = useCallback(() => {
    const term = termRef.current;
    let visible = false;
    if (term && stripEnabledRef.current && isActiveRef.current) {
      const buffer = term.buffer.active;
      const altScreen = buffer.type === 'alternate';
      const awayFromBottom = buffer.viewportY + term.rows < buffer.length;
      visible = !altScreen && (awayFromBottom || stripPinnedRef.current);
    }
    if (visible === stripVisibleRef.current) {
      // Visibility unchanged, but the viewport may have crossed the
      // scroll-bottom boundary — the strip's crop/pixel-match height-mode
      // switch — without any chunk arriving to re-run its scan.
      if (visible) stripHandleRef.current?.remeasure();
      return;
    }
    stripVisibleRef.current = visible;
    // Start buffering synchronously with the decision, so no chunk arriving
    // before React mounts InputStrip is lost.
    stripPendingRef.current = visible ? (stripPendingRef.current ?? []) : null;
    setStripVisible(visible);
  }, []);
  const evaluateStripRef = useRef(evaluateStrip);
  evaluateStripRef.current = evaluateStrip;

  const onStripReady = useCallback((handle: InputStripHandle | null) => {
    stripHandleRef.current = handle;
    if (handle) {
      stripPendingRef.current = null; // covered by the snapshot — see above
      if (stripFocusPendingRef.current) {
        stripFocusPendingRef.current = false;
        handle.focus();
      }
    } else {
      // The mirror is gone (hidden, tab switch, StrictMode remount): its focus
      // went with it. Resume buffering if the strip is about to come back.
      stripPinnedRef.current = false;
      if (stripVisibleRef.current) stripPendingRef.current = [];
    }
  }, []);

  const onStripPinnedChange = useCallback((pinned: boolean) => {
    stripPinnedRef.current = pinned;
    evaluateStripRef.current();
  }, []);

  const sendToPty = useCallback(
    (data: Uint8Array) => callbacks.current.onInput(tabId, data),
    [tabId]
  );

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    let disposed = false;
    let resizeTimer: number | undefined;

    const term = new XTerm({
      // registerDecoration (the timeline jump highlight) is proposed API;
      // without this flag the call throws instead of returning a decoration.
      allowProposedApi: true,
      cursorBlink: true,
      fontSize: fontSizeRef.current,
      // Shared with the input strip's mirror (lib/termInput.ts): identical
      // cell metrics are what make the overlay pixel-align with this screen.
      fontFamily: TERM_FONT_FAMILY,
      scrollback: 5000,
      // With the pinned input strip enabled, typing while scrolled up must
      // not snap the viewport to the bottom — the live input line is readable
      // (and typeable) in the strip, and the whole point is staying where you
      // are reading. The [stripEnabled] effect keeps this in sync live.
      scrollOnUserInput: !stripEnabledRef.current,
      theme: TERM_THEME,
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(container);
    fitRef.current = fitAddon;
    termRef.current = term;
    registerTerm(tabId, term);

    // No WebGL renderer, on purpose. Every tab keeps its terminal mounted (hidden
    // ones use `visibility: hidden` so they stay measurable), so one GL context
    // per tab means WKWebView eventually reclaims the oldest and fires
    // `webglcontextlost` in a live tab. Recovering means swapping renderers, and
    // xterm's swap disposes the old renderer before the fallback is re-attached:
    // `RenderService.dimensions` is an unguarded `this._renderer.value.dimensions`,
    // so one scroll tick landing after a teardown throws `undefined is not an
    // object`. The DOM renderer has no context budget and nothing to swap; reach
    // for @xterm/addon-canvas (2D canvas, no GL context) if output ever gets slow.

    // Byte path shared by every input patch below (and by the strip mirror).
    const send = (data: Uint8Array) => callbacks.current.onInput(tabId, data);

    term.onData((data) => {
      send(new TextEncoder().encode(data));
    });

    // WKWebView IME recovery, Shift+Enter's kitty encoding + the Cmd-229
    // drop, and trackpad tap-to-drag filtering — shared with the input
    // strip's mirror, rationale in lib/termInput.ts.
    const detachIme = attachImeRecovery(container, send);
    attachKeyEncoding(term, send);
    const detachTrackpad = attachTrackpadFilter(container);

    term.onResize(({ cols, rows }) => {
      if (cols < 1 || rows < 1) return;
      // The mirror follows the grid immediately (both screens must stay
      // line-for-line equal); only the PTY ioctl is debounced.
      stripHandleRef.current?.resize(cols, rows);
      window.clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(() => {
        if (!disposed) callbacks.current.onResize(tabId, rows, cols);
      }, RESIZE_DEBOUNCE_MS);
    });

    // Scroll position is the strip's primary visibility input.
    term.onScroll(() => evaluateStripRef.current());

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

    // Live chunks feed the strip mirror too (when one exists or is being
    // created); replay chunks never do — a mirror snapshots the primary's
    // *parsed* screen behind a fence, which already includes every replay
    // byte written before it.
    const feedStrip = (chunk: Uint8Array) => {
      const handle = stripHandleRef.current;
      if (handle) handle.feed(chunk);
      else stripPendingRef.current?.push(chunk);
    };

    // Replay whatever the PTY produced before we were listening, then stream.
    // How the replay is used depends on what it contains (see classifyReplay):
    // plain output reconstructs the screen as-is; a repaint-style TUI ring is
    // executed for its scrollback (the recent conversation stays browsable)
    // and the misaligned visible screen is then cleared with home + ED2,
    // which leaves the scrollback untouched — the TUI's next repaint (spinner
    // tick, keypress, window resize) lands on the empty screen, where its
    // cursor-relative moves clamp to the top and resync; an alt-screen ring
    // is dropped entirely and the app repaints itself.
    void callbacks.current
      .attach(tabId, (chunk, isReplay, done) => {
        if (disposed) return;
        if (!isReplay) {
          // Live chunks and fences; xterm runs the callback after this write
          // and everything queued before it has been parsed. The strip
          // re-evaluation piggybacks on the same ordering guarantee: buffer
          // transitions (alt-screen entry/exit) are settled when it runs.
          term.write(chunk, () => {
            done?.();
            evaluateStripRef.current();
          });
          feedStrip(chunk);
          return;
        }
        switch (classifyReplay(chunk)) {
          case 'alt-screen':
            return;
          case 'redraw':
            term.write(chunk);
            // Home + ED2: clear the visible screen, keep the replayed
            // history in the scrollback.
            term.write('\x1b[H\x1b[2J', () => evaluateStripRef.current());
            return;
          case 'plain':
            term.write(chunk, () => evaluateStripRef.current());
            return;
        }
      });

    return () => {
      disposed = true;
      window.clearTimeout(resizeTimer);
      observer.disconnect();
      detachTrackpad();
      detachIme();
      callbacks.current.detach(tabId);
      if (fitRef.current === fitAddon) fitRef.current = null;
      if (termRef.current === term) termRef.current = null;
      unregisterTerm(tabId, term);
      term.dispose();
    };
  }, [tabId]);

  // A font zoom (View menu) applies to every mounted terminal live. The cell
  // size changed but the container's box did not, so the ResizeObserver stays
  // silent — refit explicitly; fit feeds the debounced PTY resize via the
  // onResize path above. An unmeasurable container is skipped (same guard as
  // the observer's fit); the option is set either way, so the next fit picks
  // it up.
  useEffect(() => {
    const term = termRef.current;
    if (!term || term.options.fontSize === fontSize) return;
    term.options.fontSize = fontSize;
    const container = containerRef.current;
    if (!container || container.offsetWidth < MIN_SIZE || container.offsetHeight < MIN_SIZE) {
      return;
    }
    fitRef.current?.fit();
  }, [fontSize]);

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

  // Strip master switch (View ▸ Pinned Input Strip): rebind xterm's
  // scroll-on-input and re-derive visibility. With the strip off, native
  // snapping must come back — typed text would otherwise be invisible while
  // scrolled up, with no strip to read it in.
  useEffect(() => {
    const term = termRef.current;
    if (term) term.options.scrollOnUserInput = !stripEnabled;
    evaluateStripRef.current();
  }, [stripEnabled]);

  // The strip only exists for the active tab; a switch tears it down (its
  // mirror disposes) and the next scroll-away on the new tab recreates it.
  useEffect(() => {
    evaluateStripRef.current();
  }, [isActive]);

  // ⌘⇧B (View ▸ Focus Input Strip; useTabManager owns the cycle semantics):
  // a nonce bump means "the strip should have the caret now". Pin it visible
  // — even at the bottom, where the bottom-anchored overlay pixel-matches the
  // screen underneath — and focus it, deferring through the handshake when
  // the mirror must be created first.
  useEffect(() => {
    if (stripFocusNonce === 0) return;
    // The nonce is app-wide; every mounted pane sees the bump. Only the
    // active one may act on it — pinning a hidden pane's strip would surface
    // a stuck-visible strip the next time that tab is activated. (The
    // manager enables the feature in the same batch when it was off, so
    // stripEnabledRef is already true here when the cycle means "focus".)
    if (!isActiveRef.current || !stripEnabledRef.current) return;
    stripPinnedRef.current = true;
    evaluateStripRef.current();
    if (stripHandleRef.current) stripHandleRef.current.focus();
    else stripFocusPendingRef.current = true;
  }, [stripFocusNonce]);

  return (
    <>
      <div ref={containerRef} className="terminal-container" />
      {stripVisible && termRef.current && (
        <InputStrip
          primary={termRef.current}
          fontSize={fontSize}
          send={sendToPty}
          onReady={onStripReady}
          onPinnedChange={onStripPinnedChange}
        />
      )}
    </>
  );
};
