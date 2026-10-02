import { SearchAddon } from '@xterm/addon-search';
import type { Terminal as XTerm } from '@xterm/xterm';

/**
 * Per-terminal search addon, created on first use. Cached on the terminal
 * instance itself (WeakMap): a disposed terminal takes its entry with it, so
 * a remount (StrictMode, webview reload, tab reopen) always gets a fresh
 * addon and no lifecycle bookkeeping is needed anywhere else. This keeps
 * Terminal.tsx — which owns the xterm instances — out of the search feature
 * entirely; the search bar reaches terminals through termRegistry, same as
 * the timeline does.
 */
const addons = new WeakMap<XTerm, SearchAddon>();

export function getSearchAddon(term: XTerm): SearchAddon {
  let addon = addons.get(term);
  if (!addon) {
    addon = new SearchAddon();
    term.loadAddon(addon);
    addons.set(term, addon);
    hookRenders(term);
  }
  return addon;
}

/* Ghostty renders search matches by recoloring cells: an opaque block under
   black glyphs (search-background / search-foreground). xterm's DOM renderer
   paints highlight decorations BELOW the glyph spans, and WKWebView gives
   their mix-blend-mode no backdrop to sample, so the block colors land as-is
   — SearchBar passes Ghostty's exact values — while the glyphs keep their
   original color on top. The black half of Ghostty's look is therefore done
   here, on the glyph spans themselves: recolor what each decoration covers,
   splitting spans that straddle a match boundary. */
const MATCH_TEXT_COLOR = '#000000';

interface TextPatch {
  span: HTMLElement;
  /** Previous inline color, when the span was recolored in place. */
  color?: string;
  /** Replacement clones, when the span had to be split at a boundary. */
  clones?: HTMLElement[];
  parent?: HTMLElement;
}

const patches = new WeakMap<XTerm, TextPatch[]>();
const hooked = new WeakSet<XTerm>();
const signatures = new WeakMap<XTerm, string>();

function hookRenders(term: XTerm): void {
  if (hooked.has(term)) return;
  hooked.add(term);
  // Scrolling repositions the decorations without a search call; re-apply
  // once the render pass has moved them. The signature check below keeps
  // this cheap while unrelated output streams.
  term.onRender(() => requestAnimationFrame(() => syncMatchTextColors(term)));
}

/** Put back every span this feature recolored or split. */
function restore(term: XTerm): void {
  const list = patches.get(term);
  if (!list) return;
  for (const patch of list) {
    if (patch.color !== undefined && patch.span.isConnected) {
      patch.span.style.color = patch.color;
    }
    const first = patch.clones?.[0];
    if (first && patch.parent?.isConnected) {
      patch.parent.insertBefore(patch.span, first);
      for (const clone of patch.clones ?? []) clone.remove();
    }
  }
  patches.delete(term);
}

/**
 * Paint the glyphs under the current search decorations in Ghostty's black.
 * Decoration positions (inline left/top/width) map to buffer row and cell
 * range; the row's spans are walked against buffer cell widths so wide CJK
 * glyphs — two cells each — line up. Spans fully inside a match are recolored
 * in place, boundary spans are split into clones carrying the original
 * style. `force` bypasses the position signature for callers that just ran
 * a search (the spans may have been rewritten without anything moving).
 */
export function syncMatchTextColors(term: XTerm, force = false): void {
  const container = term.element?.querySelector('.xterm-decoration-container');
  const rows = term.element?.querySelector('.xterm-rows') as HTMLElement | null;
  if (!container || !rows) return;
  const decorations = container.querySelectorAll<HTMLElement>('.xterm-find-result-decoration');
  const signature = `${term.buffer.active.viewportY}:${Array.from(
    decorations,
    (decoration) => decoration.style.top + decoration.style.left + decoration.style.width
  ).join(',')}`;
  if (!force && signatures.get(term) === signature) return;
  signatures.set(term, signature);
  restore(term);
  if (!decorations.length) return;

  const firstRow = rows.firstElementChild as HTMLElement | null;
  const cellHeight = firstRow?.offsetHeight || 1;
  const cellWidth = rows.clientWidth / term.cols || 1;
  const applied: TextPatch[] = [];

  for (const decoration of decorations) {
    const rowIndex = Math.round(parseFloat(decoration.style.top) / cellHeight);
    const col = Math.round(parseFloat(decoration.style.left) / cellWidth);
    const end = col + Math.round(parseFloat(decoration.style.width) / cellWidth);
    const rowDiv = rows.children[rowIndex] as HTMLElement | undefined;
    const line = term.buffer.active.getLine(term.buffer.active.viewportY + rowIndex);
    if (!rowDiv || !line) continue;

    let cursor = 0;
    for (const node of Array.from(rowDiv.children)) {
      const span = node as HTMLElement;
      const text = span.textContent ?? '';
      // Cell start of each character in this span; wide glyphs advance two.
      const starts: number[] = [];
      for (let i = 0; i < text.length; i++) {
        starts.push(cursor);
        const width = line.getCell(cursor)?.getWidth() ?? 1;
        cursor += width > 1 ? width : 1;
      }
      const from = starts.findIndex((cell) => cell >= col && cell < end);
      if (from === -1) continue;
      let to = from;
      while (to + 1 < starts.length) {
        const next = starts[to + 1];
        if (next === undefined || next >= end) break;
        to++;
      }

      if (from === 0 && to === starts.length - 1) {
        applied.push({ span, color: span.style.color });
        span.style.color = MATCH_TEXT_COLOR;
        continue;
      }
      const parent = span.parentNode as HTMLElement;
      const clones: HTMLElement[] = [];
      const parts: Array<[number, number]> = [];
      if (from > 0) parts.push([0, from]);
      parts.push([from, to + 1]);
      if (to + 1 < starts.length) parts.push([to + 1, starts.length]);
      for (const [start, stop] of parts) {
        const clone = span.cloneNode(false) as HTMLElement;
        clone.textContent = text.slice(start, stop);
        if (start === from) clone.style.color = MATCH_TEXT_COLOR;
        parent.insertBefore(clone, span);
        clones.push(clone);
      }
      span.remove();
      applied.push({ span, clones, parent });
    }
  }
  patches.set(term, applied);
}
