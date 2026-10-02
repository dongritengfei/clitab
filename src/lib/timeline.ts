import type { TabNotice, TabStatus } from '../types';

/** The key events the timeline tracks. Tool calls are deliberately excluded:
 *  a single turn can fire dozens, and the timeline is a summary. */
export type TimelineEventKind = 'turn-start' | 'notice' | 'turn-end';

/** One key event in a tab's session history. */
export interface TimelineEvent {
  /** Unique within the tab (timelines are stored per tab). */
  id: number;
  kind: TimelineEventKind;
  /** Epoch ms taken from the backend payload — authoritative, never Date.now(). */
  at: number;
  /** notice only: message text; may be absent when the hook lacks jq. */
  msg?: string | null;
  /** turn-end only: turn duration in ms; null when the start was never seen. */
  duration?: number | null;
}

/** A tab's full protocol state as carried by one `tab-status` payload. */
export interface StatusSnapshot {
  status: TabStatus | null;
  notice: TabNotice | null;
}

/** Per-tab event cap; the oldest events are dropped beyond this. */
export const MAX_TIMELINE_EVENTS = 500;

/**
 * Derives timeline events from successive `tab-status` snapshots. Payloads are
 * full replacements, so an event is a *transition*:
 *
 * - status becomes `thinking`            → turn-start (at = status.since)
 * - notice appears or its `at` changes   → notice     (at = notice.at)
 * - status becomes `done`                → turn-end   (at = status.at)
 *
 * Tool-only changes never produce events. The notice rule keys on `at`, not on
 * a null→non-null transition: every payload re-sends an unchanged notice, and
 * acknowledging one only clears renderer-local state — a fresh backend
 * timestamp is the reliable signal for "new notification". Deliberately
 * tolerant of partially installed hooks: events never need to pair up (a
 * turn-end without a turn-start is recorded as-is).
 */
export class TimelineTracker {
  private prev: StatusSnapshot = { status: null, notice: null };
  private nextId = 1;

  /** Feed one payload's snapshot; returns the newly detected events (oldest first). */
  push(snapshot: StatusSnapshot): TimelineEvent[] {
    const events: TimelineEvent[] = [];
    const { status, notice } = snapshot;

    if (notice && notice.at !== this.prev.notice?.at) {
      events.push({ id: this.nextId++, kind: 'notice', at: notice.at, msg: notice.msg });
    }
    if (status?.kind === 'thinking' && this.prev.status?.kind !== 'thinking') {
      events.push({ id: this.nextId++, kind: 'turn-start', at: status.since });
    } else if (status?.kind === 'done' && this.prev.status?.kind !== 'done') {
      events.push({ id: this.nextId++, kind: 'turn-end', at: status.at, duration: status.duration });
    }

    this.prev = snapshot;
    // One payload can carry both a new notice and a turn transition;
    // backend timestamps decide the order.
    return events.sort((a, b) => a.at - b.at);
  }
}
