/**
 * Input plumbing shared by every interactive xterm instance in a tab: the
 * main terminal and the pinned input strip's mirror (components/InputStrip.tsx).
 * Each patch exists because WKWebView and/or xterm.js drops or mangles a
 * specific class of inputs; the measured rationale travels with the code.
 */
import type { Terminal as XTerm, ITheme } from '@xterm/xterm';

/** Deliver raw bytes to the tab's PTY (the `pty_input` path). */
export type SendBytes = (data: Uint8Array) => void;

/** Font stack for every xterm instance in a tab. JetBrains Mono and the
 * Nerd-patched fallback are bundled with the app (see main.tsx / App.css), so
 * the stack resolves the same everywhere — Ghostty-like metrics, and
 * powerline/devicon PUA glyphs always render inside a single cell. The input
 * strip's mirror must share the primary's exact metrics to pixel-align with
 * the screen underneath; both instances take these constants. */
export const TERM_FONT_FAMILY =
  '"JetBrains Mono", "JetBrainsMono Nerd Font Mono", Menlo, monospace';

/** Shared terminal theme (Catppuccin Mocha), same rationale as the font. */
export const TERM_THEME: ITheme = {
  background: '#1e1e2e',
  foreground: '#cdd6f4',
  cursor: '#f5e0dc',
  selectionBackground: '#585b7066',
};

/**
 * Key encodings xterm.js does not produce itself. Returns nothing; the
 * handler lives as long as the terminal instance.
 */
export function attachKeyEncoding(term: XTerm, send: SendBytes): void {
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
        send(new TextEncoder().encode('\x1b[13;2u'));
      }
      return false;
    }
    /* With a CJK input source active, WebKit reports Cmd presses as
       keyCode-229 "Process" keydowns — the IME marked the key handled, even
       when nothing is composing (xterm.js #5887 documents exactly this).
       xterm's composition path treats *every* 229 keydown as user input and,
       via scrollOnUserInput, snaps a scrolled-up viewport to the bottom — so
       ⌘C while reading history threw away the scroll position. A Cmd combo
       never sends data to the terminal, so dropping it before xterm's
       handler is safe: real composition keys (229 without Cmd) still take
       the normal path, and native copy/paste runs through the Edit menu
       (menu.rs), not through this handler. */
    if (event.type === 'keydown' && event.metaKey && event.keyCode === 229) {
      return false;
    }
    return true;
  });
}

/**
 * WKWebView IME recovery. Returns a detach function.
 *
 * WKWebView delivers IME-committed punctuation ( “ 《 『 …) as a plain
 * `input` event *before* the keystroke's own keydown (WebKit #25119), while
 * xterm.js gates its input-event fallback on "no keydown seen yet"
 * (`!e.composed || !this._keyDownSeen` in CompositionHelper._inputEvent) —
 * the still-held Shift's keydown already set that flag, so the commit is
 * dropped (the "type twice to see it" symptom, xterm.js #6144/#5887).
 * Recover exactly those dropped commits, and nothing else:
 *
 * - Mirror xterm's `_keyDownSeen` (true on any keydown, false on any
 *   keyup). When it is false at an `input`, xterm's gate passed and it
 *   already sent `ev.data` — stay out.
 * - Send `ev.data` alone, never the textarea's value: xterm never clears
 *   the hidden textarea after a composition commit, so its content is
 *   stale residue (earlier pinyin commits) and forwarding it wholesale
 *   duplicates already-delivered text.
 * - An `input` whose data equals the preceding keydown's key is normal
 *   ordered typing (ABC): xterm's keydown/keypress path owns it.
 */
export function attachImeRecovery(container: HTMLElement, send: SendBytes): () => void {
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
    send(new TextEncoder().encode(ie.data));
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

  return () => {
    container.removeEventListener('keydown', onKeyDownSeen, true);
    container.removeEventListener('keyup', onKeyUpSeen, true);
    container.removeEventListener('compositionstart', onCompositionStart, true);
    container.removeEventListener('compositionend', onCompositionEnd, true);
    container.removeEventListener('compositionend', onCompositionEndCleanup);
    container.removeEventListener('input', onInputAfterXterm);
  };
}

/**
 * Trackpad tap-to-drag filtering. Returns a detach function.
 *
 * macOS "tap to click" reaches WKWebView as fully synthesized mouse events,
 * and the tap-to-drag gesture ("tap, then tap again and swipe") produces an
 * accidental text selection in xterm. Measured on a real Force Touch trackpad,
 * the gestures are separable by per-event physical state — no timing windows:
 *
 *   - A deliberate tap / double-tap: the synthesized click's mousedown
 *     carries buttons=1 (the system committed a real click).
 *   - A real press-drag: mousedown buttons=1, every mousemove buttons=1, and
 *     webkitmouseforcechanged fires continuously as finger pressure ramps.
 *   - Tap-to-drag: a stream of buttons=0 mousemoves (the swipe, delivered
 *     with no button held at all) bracketed by synthesized clicks whose
 *     mousedown also carries buttons=0 — no physical press exists at any
 *     point. When the two taps land close together, macOS scores such a
 *     click as detail>=2, and xterm turns that double-click into a word
 *     selection: this is the accidental highlight. (The swipe's moves are
 *     inert: xterm's drag window is the 7ms between the synthesized
 *     down/up, and the moves never fall inside it.)
 *
 * So multi-click selection semantics are only honored when the click carries
 * physical button state; a buttonless multi-click is stopped before xterm's
 * selection service. WebKit also varies where the swipe sits: the moves can
 * follow the synthesized 7ms click (inert — xterm's drag window is closed),
 * or the down/up can bracket the entire swipe (dangerous — xterm extends the
 * selection on every move inside its own drag without checking buttons), so
 * moves inside a buttonless gesture are held back too. Single clicks always
 * pass (cursor placement), and a real press-drag is untouched end to end.
 */
export function attachTrackpadFilter(container: HTMLElement): () => void {
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
    container.removeEventListener('mousedown', onMouseDown, true);
    window.removeEventListener('mousemove', onMouseMove, true);
    window.removeEventListener('mouseup', onMouseUp, true);
  };
}
