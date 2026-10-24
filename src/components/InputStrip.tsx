import React, { useEffect, useRef, useState } from 'react';
import { Terminal as XTerm } from '@xterm/xterm';
import { SerializeAddon } from '@xterm/addon-serialize';
import {
  attachImeRecovery,
  attachKeyEncoding,
  attachTrackpadFilter,
  TERM_FONT_FAMILY,
  TERM_THEME,
  type SendBytes,
} from '../lib/termInput';

/**
 * Imperative bridge to the parent (Terminal.tsx). The mirror only exists
 * while the strip is mounted; the parent feeds live PTY chunks through
 * `feed` and follows the primary's grid through `resize`.
 */
export interface InputStripHandle {
  /** Queue a live PTY chunk; buffered internally until the snapshot lands. */
  feed(chunk: Uint8Array): void;
  /** Move the caret into the mirror (strip click, ⌘⇧B). */
  focus(): void;
  /** Track the primary's cols/rows so the mirror stays line-for-line equal. */
  resize(cols: number, rows: number): void;
  /** Re-run the height scan; the parent pokes this at its re-evaluation
   * points (scroll etc.), where the viewport may have crossed the
   * scroll-bottom boundary that switches the height mode. */
  remeasure(): void;
}

interface InputStripProps {
  /** The terminal whose screen bottom this strip mirrors and feeds. */
  primary: XTerm;
  fontSize: number;
  /** Byte path into the PTY — the same one the primary's onData uses. */
  send: SendBytes;
  /** Mirror lifecycle handshake, called synchronously at mount (handle) and
   * in teardown (null). Chunks the parent buffered *before* the handshake are
   * dropped, not fed: they were written to the primary before this
   * component's snapshot fence, so the snapshot already contains them.
   * Chunks arriving after it go through `feed`, which buffers internally
   * until the snapshot lands. */
  onReady: (handle: InputStripHandle | null) => void;
  /** The mirror gained/lost keyboard focus. The parent pins the strip
   * visible while it is focused, even at the bottom of the scrollback. */
  onPinnedChange: (pinned: boolean) => void;
}

/* Automatic height: the strip shows the live input region — the contiguous
   non-blank block around the mirror's cursor — and nothing else, so it grows
   with a multi-line prompt or a "/"-completion menu and shrinks back to the
   single input line at rest. Two scan rules make that work across both agent
   TUIs and plain shells:
   - downward from the cursor, blank runs of up to SCAN_DOWN_GAP rows are
     stepped over (ink chrome can have a blank row between box and hints);
   - upward, the scan runs only when chrome was found below the cursor, and
     stops at the first blank: an agent input box always has border/hint rows
     under the caret, while a bare shell prompt sits flush against the output
     above it — scanning up there would swallow the output tail. Upward
     coverage is what picks up the box's top border; a "/" completion menu
     renders *below* the box, flush with it (probe-verified), and rides the
     downward scan.
   What the strip does with the block's extent depends on the primary's
   viewport (both probe-verified):
   - scrolled up (the strip's reason to exist) it shows exactly rows
     top..bottom, shifting the mirror down inside the clip to crop the blank
     tail: ink erases a filtered/closed "/" menu *in place* and leaves the
     block stranded over blank rows — it does not reflow down — so without
     the crop the strip would stay at menu height, mostly blank, long after
     the menu is gone;
   - at the scroll bottom (⌘⇧B-pinned) the crop is off: the strip extends
     to the screen's last row and pixel-matches the view underneath (a crop
     there would shift the block down and double its top row at the seam). */
const SCAN_DOWN_GAP = 1;
/** Content-scan debounce after each mirror write. */
const SCAN_DEBOUNCE_MS = 50;
/** Row ceiling, further bounded to 70% of the grid: a strip taller than that
    would cover the very history the user scrolled up to read. Sized against
    the real thing (PTY probe, Claude Code 2.1.308): a "/" box+menu block is
    ~23 rows on a 40-row grid, and every block is blank-delimited above and
    below, so the scan finds the true extent — the cap is only a fuse. */
const MAX_CONTENT_ROWS = 30;

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/** The strip's height in grid rows plus the mirror's downward shift inside
 * the clipped host (0 = rows sit exactly over the primary's). */
interface StripGeometry {
  rows: number;
  shiftCells: number;
}

/**
 * Geometry from the mirror's parsed buffer (scan rules above). With
 * `cropTail` (viewport scrolled up) the strip shows exactly the block
 * top..bottom — cap-truncated from the top, the block's last row staying
 * anchored at the strip's bottom edge — and `shiftCells` moves the mirror
 * down so the cropped blank tail falls out of the clip. Without it the count
 * extends to the bottom of the screen (pixel-matched overlay). It is valid
 * to read `cursorY` as an absolute row because the mirror has scrollback 0
 * (baseY is always 0).
 */
function stripGeometry(mirror: XTerm, cropTail: boolean): StripGeometry {
  const buffer = mirror.buffer.active;
  const gridRows = mirror.rows;
  const cap = Math.max(1, Math.min(MAX_CONTENT_ROWS, Math.ceil(gridRows * 0.7)));
  const cy = clamp(buffer.cursorY, 0, gridRows - 1);
  const isBlank = (y: number): boolean => {
    const line = buffer.getLine(y);
    return !line || line.translateToString(true).trim() === '';
  };
  let bottom = cy;
  let blanks = 0;
  for (let y = cy + 1; y < gridRows; y++) {
    if (isBlank(y)) {
      blanks += 1;
      if (blanks > SCAN_DOWN_GAP) break;
    } else {
      blanks = 0;
      bottom = y;
    }
  }
  let top = cy;
  if (bottom > cy) {
    // Stop early once the cap is reached; rows above it can never show.
    const topLimit = Math.max(0, gridRows - cap);
    for (let y = cy - 1; y >= topLimit; y--) {
      if (isBlank(y)) break;
      top = y;
    }
  }
  if (cropTail) {
    return {
      rows: clamp(bottom - top + 1, 1, cap),
      shiftCells: gridRows - 1 - bottom,
    };
  }
  return { rows: clamp(gridRows - top, 1, cap), shiftCells: 0 };
}

/**
 * The pinned input strip: a pixel-exact live mirror of the terminal's bottom
 * rows, so the agent's input line (and its Tab-completion dropdown, caret,
 * ink chrome) stays reachable while the main view is scrolled up reading
 * history. It is a second xterm instance with the primary's exact grid, fed
 * the same byte stream, anchored inside a clipped host — bottom-flush where
 * it pixel-matches the screen (at the scroll bottom), shifted to crop a
 * stranded blank tail where it doesn't (scrolled up; see the scan rules) —
 * no parsing or text extraction anywhere; whatever the TUI draws is what
 * shows. Its height is content-driven (`stripGeometry`), quantized to the
 * primary's row grid so the clip edge never cuts a text row in half.
 *
 * Interaction is native: the mirror is a full xterm, so focus, selection,
 * IME and key encodings work exactly as in the main terminal, and its
 * onData goes straight to the PTY. It never drives the PTY size (the primary
 * owns fit/resize) and never scrolls (scrollback: 0 — a pure screen view).
 */
export const InputStrip: React.FC<InputStripProps> = ({
  primary,
  fontSize,
  send,
  onReady,
  onPinnedChange,
}) => {
  const hostRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  const mirrorRef = useRef<XTerm | null>(null);
  // Chunks fed before the snapshot landed; flushed into the mirror after it.
  const pending = useRef<Uint8Array[]>([]);
  const snapshotLanded = useRef(false);
  // This component mounts once per strip appearance; props are read through a
  // ref so the creation effect never re-runs (same pattern as Terminal.tsx).
  const props = useRef({ fontSize, send, onReady, onPinnedChange });
  props.current = { fontSize, send, onReady, onPinnedChange };

  // Rendered height in px: content rows quantized to the row grid (see the
  // lifecycle effect's autoSize). 0 until the first measurement right after
  // mount — one frame of zero-height beats a wrong-size flash.
  const [height, setHeight] = useState(0);
  // Latest autoSize, for callers outside the lifecycle effect (font zoom).
  const autoSizeRef = useRef<() => void>(() => {});

  // Mirror lifecycle: one creation per primary instance, torn down on unmount
  // (the parent unmounts the strip when it hides). StrictMode-safe: the
  // fence callback checks `disposed`, and cleanup disposes both mirror and
  // addon, so the double invoke leaves exactly one live mirror.
  useEffect(() => {
    const host = hostRef.current;
    const inner = innerRef.current;
    if (!host || !inner) return;

    let disposed = false;
    let opened = false;
    // A ⌘⇧B focus request that beat the snapshot (the mirror can only take
    // the caret once it is open); honored at the end of the fence callback.
    let focusRequested = false;
    let scanTimer: number | undefined;
    const detachers: (() => void)[] = [];
    snapshotLanded.current = false;
    pending.current = [];

    const mirror = new XTerm({
      cursorBlink: true,
      fontSize: props.current.fontSize,
      // Shared with the primary (lib/termInput.ts) — identical cell metrics
      // are what make the overlay pixel-align with the screen underneath.
      fontFamily: TERM_FONT_FAMILY,
      // Pure screen view: the primary owns the scrollback.
      scrollback: 0,
      theme: TERM_THEME,
    });
    mirrorRef.current = mirror;
    // Grid parity with the primary from the start; the snapshot is serialized
    // against exactly this geometry. The mirror never drives the PTY size.
    mirror.resize(primary.cols, primary.rows);

    // Row-grid geometry, measured from the DOM on every pass: `offset` is the
    // gap between the strip's bottom edge (= the wrapper's) and the primary's
    // screen bottom — fit() leaves slack there when the container height is
    // not a multiple of the row height — and cellH is one rendered row. The
    // mirror is anchored `offset` px up so its rows sit exactly over the
    // primary's, and the host height is quantized to whole rows (+1 px for
    // the border-top; global box-sizing is border-box) so the clip edge
    // lands on a row boundary instead of slicing a text row in half.
    const autoSize = () => {
      if (disposed) return;
      const screen = primary.element?.querySelector('.xterm-screen') as HTMLElement | null;
      if (!screen || primary.rows < 1) return;
      const hostRect = host.getBoundingClientRect();
      const screenRect = screen.getBoundingClientRect();
      const cellH = screenRect.height / primary.rows;
      if (!Number.isFinite(cellH) || cellH <= 0) return;
      const offset = Math.max(0, hostRect.bottom - screenRect.bottom);
      // Before the snapshot lands the mirror buffer is empty; sizing to it
      // would flash a cap-height strip. One row until the fence callback.
      // Scroll-bottom state is read from the *primary's* viewport (the same
      // expression as the parent's awayFromBottom): the mirror never scrolls.
      const primaryBuf = primary.buffer.active;
      const atBottom = primaryBuf.viewportY + primary.rows >= primaryBuf.length;
      const geo = snapshotLanded.current
        ? stripGeometry(mirror, !atBottom)
        : { rows: 1, shiftCells: 0 };
      // A positive shiftCells slides the mirror below the host's bottom edge,
      // cropping the blank tail out of the clip; 0 keeps its rows exactly
      // over the primary's (pixel-matched overlay).
      inner.style.bottom = `${offset - geo.shiftCells * cellH}px`;
      inner.style.left = `${Math.max(0, screenRect.left - hostRect.left)}px`;
      setHeight(offset + 1 + geo.rows * cellH);
    };
    autoSizeRef.current = autoSize;
    const scheduleScan = () => {
      window.clearTimeout(scanTimer);
      scanTimer = window.setTimeout(autoSize, SCAN_DEBOUNCE_MS);
    };

    const serialize = new SerializeAddon();
    primary.loadAddon(serialize);

    // Focus in/out of the mirror pins/unpins the strip's visibility.
    const onFocusIn = () => props.current.onPinnedChange(true);
    const onFocusOut = () => props.current.onPinnedChange(false);
    host.addEventListener('focusin', onFocusIn);
    host.addEventListener('focusout', onFocusOut);

    // Wheel over the strip keeps scrolling the primary's history: re-dispatch
    // onto the primary's viewport so xterm's own wheel math (deltaMode,
    // multiplier) applies and the primary's onScroll drives re-evaluation.
    // The capture phase + stopPropagation are the point: left to bubble up
    // from the mirror's DOM, the event would first reach the *mirror's* own
    // xterm wheel handler, which — the mirror having no scrollback — converts
    // it into literal up/down cursor keys sent to the PTY (xterm's convenience
    // for alt-buffer apps like vim/tmux), so a scroll gesture over the strip
    // would page through the agent's prompt history instead.
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      event.stopPropagation();
      const viewport = primary.element?.querySelector('.xterm-viewport');
      viewport?.dispatchEvent(
        new WheelEvent('wheel', {
          deltaY: event.deltaY,
          deltaX: event.deltaX,
          deltaMode: event.deltaMode,
          bubbles: true,
          cancelable: true,
        })
      );
    };
    host.addEventListener('wheel', onWheel, { capture: true, passive: false });

    const sendBytes: SendBytes = (data) => props.current.send(data);

    // Handshake *before* queueing the snapshot fence, synchronously: chunks
    // the parent buffered between the visibility decision and this mount are
    // covered by the fence's snapshot (they were written to the primary
    // before the fence), so the parent drops them; everything from here on
    // arrives through feed() and is buffered internally until the snapshot
    // lands. Queuing the fence in the same synchronous block leaves no gap
    // for a chunk to slip between the two handoff regimes.
    props.current.onReady({
      feed(chunk) {
        if (disposed) return;
        if (snapshotLanded.current) {
          mirror.write(chunk);
          scheduleScan();
        } else {
          pending.current.push(chunk);
        }
      },
      focus() {
        if (disposed) return;
        if (opened) mirror.focus();
        else focusRequested = true;
      },
      resize(cols, rows) {
        if (disposed) return;
        mirror.resize(cols, rows);
        // The primary's screen element reaches its new size on its next
        // render tick; re-measure after both DOMs have settled.
        requestAnimationFrame(() => requestAnimationFrame(autoSize));
      },
      remeasure() {
        if (disposed) return;
        scheduleScan();
      },
    });

    // First geometry pass: anchor the mirror and show a minimal strip
    // immediately; the fence callback below sizes it to real content.
    autoSize();

    // Snapshot behind an empty fence write on the primary: the callback runs
    // once every byte queued before it has been parsed, so serialize() sees a
    // settled screen. Internally-buffered chunks arrived after the fence was
    // queued, so the primary parses them after it too — flushing them behind
    // the snapshot loses nothing and applies nothing twice. Written before
    // open, as the serialize addon recommends: no half-painted frames.
    primary.write('', () => {
      if (disposed) return;
      const snapshot = serialize.serialize({ scrollback: 0, excludeAltBuffer: true });
      mirror.write(snapshot);
      for (const chunk of pending.current) mirror.write(chunk);
      pending.current = [];
      snapshotLanded.current = true;

      mirror.open(inner);
      opened = true;
      mirror.onData((data) => sendBytes(new TextEncoder().encode(data)));
      // Same WKWebView input patches as the main terminal: Shift+Enter's
      // kitty encoding, IME commit recovery, tap-to-drag filtering.
      attachKeyEncoding(mirror, sendBytes);
      detachers.push(attachImeRecovery(inner, sendBytes));
      detachers.push(attachTrackpadFilter(host));
      // Size to the snapshot now; the trailing scan covers the flushed
      // chunks, which are still being parsed.
      autoSize();
      scheduleScan();
      if (focusRequested) mirror.focus();
    });

    return () => {
      disposed = true;
      window.clearTimeout(scanTimer);
      snapshotLanded.current = false;
      pending.current = [];
      props.current.onReady(null);
      host.removeEventListener('focusin', onFocusIn);
      host.removeEventListener('focusout', onFocusOut);
      host.removeEventListener('wheel', onWheel, { capture: true });
      for (const detach of detachers) detach();
      serialize.dispose();
      mirror.dispose();
      if (mirrorRef.current === mirror) mirrorRef.current = null;
    };
  }, [primary]);

  // Font zoom applies live, same as the primary's. The cell height changes
  // with the font, so re-quantize after the primary refits (Terminal.tsx)
  // and both DOMs have settled.
  useEffect(() => {
    const mirror = mirrorRef.current;
    if (mirror && mirror.options.fontSize !== fontSize) {
      mirror.options.fontSize = fontSize;
      requestAnimationFrame(() => requestAnimationFrame(() => autoSizeRef.current()));
    }
  }, [fontSize]);

  return (
    <div ref={hostRef} className="input-strip" style={{ height }}>
      <div ref={innerRef} className="input-strip-inner" />
    </div>
  );
};
