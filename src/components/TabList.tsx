import React, { useCallback, useRef, useState } from 'react';
import { flushSync } from 'react-dom';
import { Tab } from '../types';
import { TabItem } from './TabItem';

/** Vertical travel before a press turns into a drag; under the threshold the
 * existing select-on-press behavior is the whole gesture. */
const DRAG_THRESHOLD_PX = 4;

/** Mutable state of the one in-flight press→drag gesture. */
interface DragState {
  tabId: string;
  pointerId: number;
  startY: number;
  /** Pointer offset inside the row at press time, so the row keeps the
   * grabbed point under the cursor instead of snapping to its top edge. */
  grabOffsetY: number;
  startIndex: number;
  dragging: boolean;
  /** The row's current inline translateY; needed to recover the untransformed
   * slot top from a bounding rect that includes the transform. */
  translateY: number;
}

interface TabListProps {
  tabs: Tab[];
  activeTabId: string | null;
  /** Activate without moving focus out of the list; used by arrow navigation. */
  onTabClick: (tabId: string) => void;
  /** Activate a tab from a deliberate gesture (click, Enter, Space): the
   * caret must end up in that tab's terminal. */
  onTabActivate: (tabId: string) => void;
  onTabClose: (tabId: string) => void;
  /** Move a tab to a new position (remove-then-insert). Called per hover
   * change during a drag (local order only) and once on drop with `persist`
   * to write the final arrangement to the backend. */
  onReorder: (tabId: string, toIndex: number, persist?: boolean) => void;
  onNewTab: () => void;
}

export const TabList: React.FC<TabListProps> = ({
  tabs,
  activeTabId,
  onTabClick,
  onTabActivate,
  onTabClose,
  onReorder,
  onNewTab,
}) => {
  const itemsRef = useRef<HTMLDivElement>(null);
  // The drag gesture lives in per-press window listeners, so the values it
  // needs fresh (tab order, reorder callback) are read through refs instead
  // of closing over a render's snapshot.
  const tabsRef = useRef(tabs);
  tabsRef.current = tabs;
  const onReorderRef = useRef(onReorder);
  onReorderRef.current = onReorder;
  const [draggingId, setDraggingId] = useState<string | null>(null);

  // Arrow navigation inside the tablist: activate and move focus in one step,
  // which is what a vertical tablist is expected to do.
  const focusTab = useCallback(
    (index: number) => {
      const count = tabs.length;
      if (count === 0) return;
      const tab = tabs[(index + count) % count];
      if (!tab) return;
      onTabClick(tab.id);
      itemsRef.current
        ?.querySelector<HTMLElement>(`[data-tab-id="${CSS.escape(tab.id)}"]`)
        ?.focus();
    },
    [tabs, onTabClick]
  );

  const onKeyDown = (event: React.KeyboardEvent) => {
    const active = tabs.findIndex((tab) => tab.id === activeTabId);
    const current = active < 0 ? 0 : active;
    switch (event.key) {
      case 'ArrowDown':
      case 'ArrowRight':
        event.preventDefault();
        focusTab(current + 1);
        break;
      case 'ArrowUp':
      case 'ArrowLeft':
        event.preventDefault();
        focusTab(current - 1);
        break;
      case 'Home':
        event.preventDefault();
        focusTab(0);
        break;
      case 'End':
        event.preventDefault();
        focusTab(tabs.length - 1);
        break;
      default:
        break;
    }
  };

  // Press→drag gesture. Selection stays on mousedown (TabItem), so this only
  // adds the reorder: under the movement threshold the press is a plain
  // click; past it the row follows the pointer and the others give way live.
  const onPointerDown = (event: React.PointerEvent) => {
    if (event.button !== 0) return;
    // macOS tap-to-click reaches WKWebView as synthesized buttonless events:
    // a trackpad touch scores as a tap whose pointerdown carries buttons=0,
    // and the following swipe is a stream of buttons=0 moves (measured data —
    // see Terminal.tsx's synthesized-gesture filter). A reorder must take a
    // *physical* press, so a buttonless down never arms the gesture; the tap
    // itself still selects the tab via TabItem's mousedown, untouched.
    if ((event.buttons & 1) === 0) return;
    const source = event.target as HTMLElement;
    // The close button is its own gesture; grabbing it must not start a drag.
    if (source.closest('.tab-close')) return;
    const row = source.closest<HTMLElement>('[data-tab-id]');
    const tabId = row?.dataset.tabId;
    if (!row || !tabId) return;
    const startIndex = tabsRef.current.findIndex((tab) => tab.id === tabId);
    if (startIndex === -1) return;

    const rect = row.getBoundingClientRect();
    const drag: DragState = {
      tabId,
      pointerId: event.pointerId,
      startY: event.clientY,
      grabOffsetY: event.clientY - rect.top,
      startIndex,
      dragging: false,
      translateY: 0,
    };
    // Capture keeps the gesture fed while the pointer is outside the list (or
    // the window): a release we never see would leave the drag armed and
    // reordering rows on hover alone. Released implicitly on pointerup, and
    // defensively in finish() for the cancel path.
    row.setPointerCapture(event.pointerId);

    const draggedRow = () =>
      itemsRef.current?.querySelector<HTMLElement>(
        `[data-tab-id="${CSS.escape(drag.tabId)}"]`
      ) ?? null;

    const onMove = (moveEvent: PointerEvent) => {
      if (!drag.dragging) {
        // Defense in depth for the same trackpad gesture: a buttons=1 down
        // (deliberate tap) can still be followed by buttonless swipe moves —
        // only moves with the left button physically held may start the drag.
        // Once dragging, every move is honored: a live drag is a real press
        // by definition, and suppressing it mid-gesture would strand the row.
        if ((moveEvent.buttons & 1) === 0) return;
        if (Math.abs(moveEvent.clientY - drag.startY) < DRAG_THRESHOLD_PX) return;
        drag.dragging = true;
        setDraggingId(drag.tabId);
      }
      // Suppress selection-drag and the compat mouse events for the rest of
      // the gesture.
      moveEvent.preventDefault();
      const items = itemsRef.current;
      if (!items) return;

      // Rows have variable heights (status line, cwd line), so the insertion
      // index comes from live geometry: how many *other* rows has the pointer
      // moved past the midpoint of? That count is exactly the dragged tab's
      // remove-then-insert index.
      let target = 0;
      for (const other of items.querySelectorAll<HTMLElement>('[data-tab-id]')) {
        if (other.dataset.tabId === drag.tabId) continue;
        const bounds = other.getBoundingClientRect();
        if (moveEvent.clientY > bounds.top + bounds.height / 2) target += 1;
      }
      const current = tabsRef.current.findIndex((tab) => tab.id === drag.tabId);
      if (current === -1) return; // tab exited mid-drag; release ends it
      if (target !== current) {
        // Flush the reorder so the slot math below reads the reordered DOM —
        // a deferred commit paints the row in its new slot with the old
        // transform for a frame (a visible jump on every row crossing).
        flushSync(() => onReorderRef.current(drag.tabId, target));
      }

      // Follow the pointer: translate against the untransformed layout slot
      // (the bounding rect includes the previous move's transform).
      const dragged = draggedRow();
      if (!dragged) return;
      const slotTop = dragged.getBoundingClientRect().top - drag.translateY;
      drag.translateY = moveEvent.clientY - drag.grabOffsetY - slotTop;
      dragged.style.transform = `translateY(${drag.translateY}px)`;
    };

    // Pointerup commits; pointercancel (the system took the pointer, or the
    // row left the DOM) commits what is on screen too — the live order is
    // already what the user sees.
    const finish = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', finish);
      window.removeEventListener('pointercancel', finish);
      const dragged = draggedRow();
      if (dragged?.hasPointerCapture(drag.pointerId)) {
        dragged.releasePointerCapture(drag.pointerId);
      }
      if (dragged) dragged.style.transform = '';
      if (!drag.dragging) return;
      setDraggingId(null);
      const current = tabsRef.current.findIndex((tab) => tab.id === drag.tabId);
      // Dropped back where it started (or the tab is gone): display and
      // backend already agree, so no IPC.
      if (current === -1 || current === drag.startIndex) return;
      onReorderRef.current(drag.tabId, current, true);
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', finish);
    window.addEventListener('pointercancel', finish);
  };

  return (
    <nav className="tab-list" aria-label="Terminals">
      <div className="tab-list-header">
        <span>CLI Tabs</span>
      </div>
      <div
        className={`tab-list-items${draggingId ? ' dragging' : ''}`}
        ref={itemsRef}
        role="tablist"
        aria-orientation="vertical"
        onKeyDown={onKeyDown}
        onPointerDown={onPointerDown}
      >
        {tabs.map((tab, index) => (
          <TabItem
            key={tab.id}
            tab={tab}
            index={index}
            count={tabs.length}
            isActive={tab.id === activeTabId}
            isDragging={tab.id === draggingId}
            onClick={() => onTabActivate(tab.id)}
            onClose={() => onTabClose(tab.id)}
          />
        ))}
      </div>
      <button className="new-tab-button" onClick={onNewTab}>
        + New Tab
      </button>
    </nav>
  );
};
