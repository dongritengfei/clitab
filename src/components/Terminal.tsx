import React, { useEffect, useRef } from 'react';
import { Terminal as XTerm } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { registerTerm, unregisterTerm } from '../lib/termRegistry';

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

/* macOS "tap to click" reaches WKWebView as fully synthesized mouse events,
   and the tap-to-drag gesture ("tap, then tap again and swipe") produces an
   accidental text selection in xterm. Measured on a real Force Touch trackpad,
   the gestures are separable by per-event physical state — no timing windows:

     - A deliberate tap / double-tap: the synthesized click's mousedown
       carries buttons=1 (the system committed a real click).
     - A real press-drag: mousedown buttons=1, every mousemove buttons=1, and
       webkitmouseforcechanged fires continuously as finger pressure ramps.
     - Tap-to-drag: a stream of buttons=0 mousemoves (the swipe, delivered
       with no button held at all) bracketed by synthesized clicks whose
       mousedown also carries buttons=0 — no physical press exists at any
       point. When the two taps land close together, macOS scores such a
       click as detail>=2, and xterm turns that double-click into a word
       selection: this is the accidental highlight. (The swipe's moves are
       inert: xterm's drag window is the 7ms between the synthesized
       down/up, and the moves never fall inside it.)

   So multi-click selection semantics are only honored when the click carries
   physical button state; a buttonless multi-click is stopped before xterm's
   selection service. WebKit also varies where the swipe sits: the moves can
   follow the synthesized 7ms click (inert — xterm's drag window is closed),
   or the down/up can bracket the entire swipe (dangerous — xterm extends the
   selection on every move inside its own drag without checking buttons), so
   moves inside a buttonless gesture are held back too. Single clicks always
   pass (cursor placement), and a real press-drag is untouched end to end. */

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
      // registerDecoration (the timeline jump highlight) is proposed API;
      // without this flag the call throws instead of returning a decoration.
      allowProposedApi: true,
      cursorBlink: true,
      fontSize: 14,
      // JetBrains Mono and the Nerd-patched fallback are bundled with the app
      // (see main.tsx / App.css), so the stack resolves the same everywhere —
      // Ghostty-like metrics, and powerline/devicon PUA glyphs always render
      // inside a single cell.
      fontFamily: '"JetBrains Mono", "JetBrainsMono Nerd Font Mono", Menlo, monospace',
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

    term.onData((data) => {
      callbacks.current.onInput(tabId, new TextEncoder().encode(data));
    });

    /* WKWebView delivers IME-committed punctuation ( “ 《 『 …) as a plain
       `input` event *before* the keystroke's own keydown (WebKit #25119), while
       xterm.js gates its input-event fallback on "no keydown seen yet"
       (`!e.composed || !this._keyDownSeen` in CompositionHelper._inputEvent) —
       the still-held Shift's keydown already set that flag, so the commit is
       dropped (the "type twice to see it" symptom, xterm.js #6144/#5887).
       Recover exactly those dropped commits, and nothing else:

       - Mirror xterm's `_keyDownSeen` (true on any keydown, false on any
         keyup). When it is false at an `input`, xterm's gate passed and it
         already sent `ev.data` — stay out.
       - Send `ev.data` alone, never the textarea's value: xterm never clears
         the hidden textarea after a composition commit, so its content is
         stale residue (earlier pinyin commits) and forwarding it wholesale
         duplicates already-delivered text.
       - An `input` whose data equals the preceding keydown's key is normal
         ordered typing (ABC): xterm's keydown/keypress path owns it. */
    let lastKeyDownKey: string | undefined;
    let keyDownSeen = false;
    let composing = false;

    const onKeyDownSeen = (ev: Event) => {
      keyDownSeen = true;
      lastKeyDownKey = (ev as KeyboardEvent).key;
    };
    const onKeyUpSeen = () => {
      keyDownSeen = false;
    };
    const onCompositionStart = () => {
      composing = true;
    };
    const onCompositionEnd = () => {
      composing = false;
    };
    /* xterm keeps committed text in the hidden textarea (it only clears it on
       Enter / Ctrl+C / blur / paste) and derives the next commit as
       value.substring(value.length at compositionstart) — i.e. it assumes the
       caret stays parked at the end of that residue. An unhandled key
       (Cmd+Left) lets WebKit move the caret to offset 0, so the next
       composition inserts its preedit *before* the residue and the commit
       re-sends the stale character instead of the new one. Restoring xterm's
       own "empty while not composing" invariant after every commit makes any
       caret-moving key harmless. Queued from the bubble phase so this timeout
       runs after xterm's own setTimeout(0) textarea read, which its
       target-phase compositionend listener queues first. */
    const onCompositionEndCleanup = () => {
      setTimeout(() => {
        if (composing) return; // a new composition already began
        const ta = container.querySelector('textarea');
        if (ta) ta.value = '';
      }, 0);
    };
    const onInputAfterXterm = (ev: Event) => {
      const ie = ev as InputEvent;
      if (ie.inputType !== 'insertText' || ie.isComposing || composing || !ie.data) return;
      if (!keyDownSeen) return; // xterm's gate passed: it sent ev.data itself
      if (lastKeyDownKey === ie.data) return; // keydown path already sent it
      callbacks.current.onInput(tabId, new TextEncoder().encode(ie.data));
      const ta = container.querySelector('textarea');
      if (ta) ta.value = ''; // hygiene only: nothing reads the textarea again
    };
    container.addEventListener('keydown', onKeyDownSeen, true);
    container.addEventListener('keyup', onKeyUpSeen, true);
    container.addEventListener('compositionstart', onCompositionStart, true);
    container.addEventListener('compositionend', onCompositionEnd, true);
    container.addEventListener('compositionend', onCompositionEndCleanup);
    // Bubble phase on the container: runs after xterm's own textarea listener,
    // so the mirror state observed here matches what xterm's gate saw.
    container.addEventListener('input', onInputAfterXterm);

    // xterm.js implements neither modifyOtherKeys nor the kitty keyboard
    // protocol, so Shift+Enter would reach the shell as a plain CR — Claude
    // Code (which enables the kitty keyboard protocol at startup) would
    // submit the prompt instead of inserting a newline. Send the kitty CSI-u
    // encoding of Shift+Enter ourselves, exactly what a kitty-protocol
    // terminal would deliver.
    term.attachCustomKeyEventHandler((event) => {
      if (
        event.key === 'Enter' &&
        event.shiftKey &&
        !event.altKey &&
        !event.ctrlKey &&
        !event.metaKey
      ) {
        if (event.type === 'keydown' && !event.repeat) {
          callbacks.current.onInput(tabId, new TextEncoder().encode('\x1b[13;2u'));
        }
        return false;
      }
      return true;
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
    // How the replay is used depends on what it contains (see classifyReplay):
    // plain output reconstructs the screen as-is; a repaint-style TUI ring is
    // executed for its scrollback (the recent conversation stays browsable)
    // and the misaligned visible screen is then cleared with home + ED2,
    // which leaves the scrollback untouched — the TUI's next repaint (spinner
    // tick, keypress, window resize) lands on the empty screen, where its
    // cursor-relative moves clamp to the top and resync; an alt-screen ring
    // is dropped entirely and the app repaints itself.
    void callbacks.current
      .attach(tabId, (chunk, isReplay) => {
        if (disposed) return;
        if (!isReplay) {
          term.write(chunk);
          return;
        }
        switch (classifyReplay(chunk)) {
          case 'alt-screen':
            return;
          case 'redraw':
            term.write(chunk);
            // Home + ED2: clear the visible screen, keep the replayed
            // history in the scrollback.
            term.write('\x1b[H\x1b[2J');
            return;
          case 'plain':
            term.write(chunk);
            return;
        }
      });

    // Synthesized-gesture filtering (see the block comment above for the
    // measured-data rationale). Every event macOS synthesizes for a tap or
    // tap-to-drag carries buttons=0 — no physical press exists — while a real
    // press-drag carries buttons=1 on its mousedown and every mousemove.
    // Two state-based filters, no timing windows:
    //   1. A buttonless multi-click is tap machinery, not a deliberate
    //      double-click: it never reaches xterm's selection service.
    //   2. While a buttonless gesture is down, its moves never reach xterm's
    //      document-level drag listener either — WebKit sometimes brackets
    //      the whole swipe between the synthesized down/up, and xterm
    //      extends the selection on any move inside its own drag without
    //      checking buttons. A detail=1 buttonless down still passes (tap to
    //      place the cursor); with its moves held back, the gesture ends as
    //      the plain click it physically was.
    let synthesizedGesture = false;

    const onMouseDown = (event: MouseEvent) => {
      if (event.button !== 0) return;
      synthesizedGesture = event.buttons === 0;
      if (synthesizedGesture && event.detail >= 2) {
        event.stopPropagation();
      }
    };

    const onMouseUp = (event: MouseEvent) => {
      if (event.button === 0) synthesizedGesture = false;
    };

    const onMouseMove = (event: MouseEvent) => {
      if (event.buttons !== 0) {
        // Physical button state: a real drag, never suppress (and heal a
        // gesture window left open by a missed mouseup).
        synthesizedGesture = false;
        return;
      }
      if (synthesizedGesture) event.stopPropagation();
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
      container.removeEventListener('keydown', onKeyDownSeen, true);
      container.removeEventListener('keyup', onKeyUpSeen, true);
      container.removeEventListener('compositionstart', onCompositionStart, true);
      container.removeEventListener('compositionend', onCompositionEnd, true);
      container.removeEventListener('compositionend', onCompositionEndCleanup);
      container.removeEventListener('input', onInputAfterXterm);
      callbacks.current.detach(tabId);
      if (fitRef.current === fitAddon) fitRef.current = null;
      if (termRef.current === term) termRef.current = null;
      unregisterTerm(tabId, term);
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
