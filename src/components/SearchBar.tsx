import React, { useCallback, useEffect, useRef, useState } from 'react';
import type { ISearchOptions } from '@xterm/addon-search';
import { getTerm } from '../lib/termRegistry';
import { getSearchAddon, syncMatchTextColors } from '../lib/searchAddon';

interface SearchBarProps {
  /** The tab being searched: the bar always targets the active terminal. */
  tabId: string;
  /** Each increment is a fresh ⌘F: reset the query and refocus the input. */
  nonce: number;
  onClose: () => void;
}

/* Ghostty's search colors, sampled from a live Ghostty screenshot: every
   match a #ffe082 amber block, the active match a white one, both with
   black text. The decorations paint below the glyph layer exactly as given
   (no blending — WKWebView gives mix-blend-mode nothing to sample there),
   so these are the on-screen block colors verbatim; the black glyphs are
   syncMatchTextColors' job (searchAddon.ts). */
const DECORATIONS: ISearchOptions['decorations'] = {
  matchBackground: '#ffe082',
  matchOverviewRuler: '#ffe082',
  activeMatchBackground: '#ffffff',
  activeMatchColorOverviewRuler: '#ffffff',
};

/* The addon also SELECTS the active match, and the theme's translucent
   selection gray over the white block is what turned it into a gray slab.
   While the bar is open the selection background becomes the same opaque
   white, so selection and active decoration agree on Ghostty's color;
   close() puts the theme value back. */
const THEME_SELECTION_BACKGROUND = '#585b7066';

/**
 * ⌘F find bar floating over the active terminal (mounted only while open).
 * Plain substring search over the visible screen plus scrollback, via
 * SearchAddon; Enter / Shift+Enter cycle matches, Esc closes and returns
 * the caret to the shell.
 */
export const SearchBar: React.FC<SearchBarProps> = ({ tabId, nonce, onClose }) => {
  const [query, setQuery] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);

  const addonFor = useCallback((id: string | null) => {
    const term = id ? getTerm(id) : undefined;
    return term ? getSearchAddon(term) : undefined;
  }, []);

  const search = useCallback(
    (direction: 'next' | 'previous', term: string, incremental = false) => {
      if (!term) return;
      const addon = addonFor(tabId);
      if (!addon) return;
      const options: ISearchOptions = { incremental, decorations: DECORATIONS };
      if (direction === 'next') addon.findNext(term, options);
      else addon.findPrevious(term, options);
      // Decorations render on the terminal's next frame; recolor the glyphs
      // they cover once their positions exist.
      const xterm = getTerm(tabId);
      if (xterm) {
        requestAnimationFrame(() =>
          requestAnimationFrame(() => syncMatchTextColors(xterm, true))
        );
      }
    },
    [addonFor, tabId]
  );

  /* Opaque-white selection for the active match while the bar is open,
     restored on close or tab switch (see THEME_SELECTION_BACKGROUND). */
  useEffect(() => {
    const term = getTerm(tabId);
    if (!term) return;
    term.options.theme = { ...term.options.theme, selectionBackground: '#ffffff' };
    return () => {
      term.options.theme = {
        ...term.options.theme,
        selectionBackground: THEME_SELECTION_BACKGROUND,
      };
    };
  }, [tabId]);

  /* Every ⌘F is a fresh search: drop the old query's highlights, empty the
     input, take focus. Deliberately keyed on the nonce alone — `tabId` must
     NOT be a dependency here, or a tab switch would wipe the query that the
     effect below is carrying over. */
  useEffect(() => {
    addonFor(tabId)?.clearDecorations();
    setQuery('');
    inputRef.current?.focus();
  }, [nonce]);

  /* Switching tabs while the bar is open: the old tab's highlights belong to
     a terminal nobody is looking at — drop them — and the query carries over
     to the new tab. Focus is deliberately not stolen back: activating a tab
     moves the caret into its terminal (App.tsx), and fighting that would
     make ⌃Tab-then-type surprising. */
  const prevTabId = useRef(tabId);
  useEffect(() => {
    if (prevTabId.current === tabId) return;
    addonFor(prevTabId.current)?.clearDecorations();
    prevTabId.current = tabId;
    if (query) search('next', query);
  }, [tabId]);

  const close = useCallback(() => {
    addonFor(tabId)?.clearDecorations();
    onClose();
    // Hand the caret back so typing continues without a click.
    getTerm(tabId)?.focus();
  }, [addonFor, tabId, onClose]);

  const onQueryChange = (value: string) => {
    setQuery(value);
    if (!value) {
      addonFor(tabId)?.clearDecorations();
      return;
    }
    // Incremental: extending the current match keeps the view still while
    // typing instead of jumping to the next occurrence on every keystroke.
    search('next', value, true);
  };

  const onKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    // An IME composition is confirmed with Enter and cancelled with Esc;
    // neither must run a search or close the bar. The committed text arrives
    // as a plain change event and searches like any other input.
    if (event.nativeEvent.isComposing) return;
    if (event.key === 'Enter') {
      event.preventDefault();
      search(event.shiftKey ? 'previous' : 'next', query);
    } else if (event.key === 'Escape') {
      event.preventDefault();
      close();
    }
  };

  // Clicking a button must not pull focus out of the input: the flow is
  // type → click ↓ → keep typing.
  const keepFocus = (event: React.MouseEvent) => event.preventDefault();

  return (
    <div className="search-bar" role="search">
      <input
        ref={inputRef}
        type="text"
        className="search-input"
        value={query}
        placeholder="Search"
        aria-label="Search terminal output"
        spellCheck={false}
        onChange={(event) => onQueryChange(event.target.value)}
        onKeyDown={onKeyDown}
      />
      <button
        aria-label="Previous match"
        disabled={!query}
        onMouseDown={keepFocus}
        onClick={() => search('previous', query)}
      >
        ↑
      </button>
      <button
        aria-label="Next match"
        disabled={!query}
        onMouseDown={keepFocus}
        onClick={() => search('next', query)}
      >
        ↓
      </button>
      <button aria-label="Close search" onMouseDown={keepFocus} onClick={close}>
        ×
      </button>
    </div>
  );
};
