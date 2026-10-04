import type { TabAnswer, TabNotice, TabStatus } from '../types';

/** The key events the timeline tracks. Tool calls are deliberately excluded:
 *  a single turn can fire dozens, and the timeline is a summary. `answer` is
 *  the exception — the user's choice in an AskUserQuestion dialog. */
export type TimelineEventKind = 'turn-start' | 'notice' | 'answer' | 'turn-end';

/** One key event in a tab's session history. */
export interface TimelineEvent {
  /** Unique within the tab (timelines are stored per tab). */
  id: number;
  kind: TimelineEventKind;
  /** Epoch ms taken from the backend payload — authoritative, never Date.now(). */
  at: number;
  /** notice / turn-start / answer: message text (the notification, the
   *  user's submitted prompt, or the chosen answer); may be absent when the
   *  hook lacks jq. */
  msg?: string | null;
  /** turn-end only: turn duration in ms; null when the start was never seen. */
  duration?: number | null;
  /** turn-start only: the prompt arrived while a previous turn was still in
   *  flight — Claude Code queued it and auto-submits it the instant the
   *  previous turn stops (the hook fires at queue time and NOT again at
   *  submit). */
  queued?: boolean;
  /** turn-start only: execution start (epoch ms), stamped by
   *  `startQueuedTurn` when the backend's auto-submission payload arrives
   *  (exact time, also clears `queued`), or as a fallback by
   *  `stampQueuedStarts` at the consuming turn-end (turn-end's time, keeps
   *  `queued`). Display prefers this over `at` (the queue time). */
  startedAt?: number | null;
}

/** A tab's full protocol state as carried by one `tab-status` payload. */
export interface StatusSnapshot {
  status: TabStatus | null;
  notice: TabNotice | null;
  answer: TabAnswer | null;
}

/** Per-tab event cap; the oldest events are dropped beyond this. */
export const MAX_TIMELINE_EVENTS = 500;

/**
 * Derives timeline events from successive `tab-status` snapshots. Payloads are
 * full replacements, so an event is a *transition*:
 *
 * - status is `thinking` with a new `since` → turn-start (at = status.since),
 *   except when `auto` — a backend-modeled auto-submission whose row already
 *   exists (the queued prompt); `startQueuedTurn` flips that row instead
 * - notice appears or its `at` changes   → notice     (at = notice.at)
 * - answer appears or its `at` changes   → answer     (at = answer.at)
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
  private prev: StatusSnapshot = { status: null, notice: null, answer: null };
  private nextId = 1;

  /** Feed one payload's snapshot; returns the newly detected events (oldest first). */
  push(snapshot: StatusSnapshot): TimelineEvent[] {
    const events: TimelineEvent[] = [];
    const { status, notice, answer } = snapshot;

    if (notice && notice.at !== this.prev.notice?.at) {
      events.push({ id: this.nextId++, kind: 'notice', at: notice.at, msg: notice.msg });
    }
    if (answer && answer.at !== this.prev.answer?.at) {
      events.push({ id: this.nextId++, kind: 'answer', at: answer.at, msg: answer.msg });
    }
    // A prompt queued mid-turn re-enters `thinking` from `thinking` (its hook
    // fires at queue time), so a kind-transition check would drop it: key on
    // `since` instead — the backend stamps a fresh one per prompt
    // (begin_turn), and re-sent payloads carry the same one (dedup by
    // timestamp, like notice and answer above).
    const prevThinking =
      this.prev.status?.kind === 'thinking' ? this.prev.status.since : undefined;
    if (status?.kind === 'thinking' && status.since !== prevThinking) {
      // An `auto` thinking is the backend modeling Claude Code's
      // auto-submission of the queue head (no hook re-fires): the queued
      // prompt's turn-start row already exists, so no new row — the caller
      // flips the existing one via `startQueuedTurn`.
      if (!status.auto) {
        const turnStart: TimelineEvent = {
          id: this.nextId++,
          kind: 'turn-start',
          at: status.since,
          msg: status.msg,
        };
        // A prompt arriving while the previous turn is still in flight
        // (thinking or tool) is one Claude Code queued: tag it so the panel
        // can explain why its timestamp precedes the previous turn-end.
        const prevKind = this.prev.status?.kind;
        if (prevKind === 'thinking' || prevKind === 'tool') turnStart.queued = true;
        events.push(turnStart);
      }
    } else if (status?.kind === 'done' && this.prev.status?.kind !== 'done') {
      events.push({ id: this.nextId++, kind: 'turn-end', at: status.at, duration: status.duration });
    }

    this.prev = snapshot;
    // One payload can carry both a new notice and a turn transition;
    // backend timestamps decide the order.
    return events.sort((a, b) => a.at - b.at);
  }
}

/**
 * A turn-end means the head of the prompt queue just executed: Claude Code
 * auto-submits the oldest queued prompt the instant the previous turn stops
 * (the hook does NOT fire again), so the turn-end's timestamp is the best
 * available execution time. Stamps the oldest unstamped queued turn-start —
 * FIFO, the same pairing navigateToEvent uses. Returns a new array when
 * something was stamped (React state); the input array is never mutated, and
 * is returned as-is when there is nothing to stamp.
 */
export function stampQueuedStarts(events: TimelineEvent[], executedAt: number): TimelineEvent[] {
  const idx = events.findIndex(
    (e) => e.kind === 'turn-start' && e.queued === true && e.startedAt == null
  );
  if (idx < 0) return events;
  return events.map((e, i) => (i === idx ? { ...e, startedAt: executedAt } : e));
}

/**
 * The backend modeled an auto-submission (`thinking` with `auto: true`):
 * Claude Code just started executing the oldest still-queued prompt, at
 * `since`. Flips that row — exact execution time replaces any
 * `stampQueuedStarts` fallback, and the Queued badge goes away because the
 * prompt is no longer waiting. Targets `queued === true` regardless of
 * `startedAt` (the fallback stamp runs first and keeps the badge). Same
 * non-mutating / identity contract as `stampQueuedStarts`.
 */
export function startQueuedTurn(events: TimelineEvent[], since: number): TimelineEvent[] {
  const idx = events.findIndex((e) => e.kind === 'turn-start' && e.queued === true);
  if (idx < 0) return events;
  return events.map((e, i) =>
    i === idx ? { ...e, startedAt: since, queued: false } : e
  );
}

/**
 * Panel display order: chronological by execution time (`startedAt ?? at`).
 * The events array itself stays in *arrival* order — a queued prompt's row
 * arrives at queue time, before the consuming turn-end, and the FIFO
 * turn-start/turn-end pairing in `navigateToEvent` relies on that — so the
 * panel renders this sorted copy instead. A flip (`startQueuedTurn`) therefore
 * moves the row below the turn-end that consumed it. Ties put turn-end first:
 * an auto-submitted turn shares the exact millisecond with the stop that
 * spawned it, and visually the turn must end before the next one starts.
 */
export function displayOrder(events: TimelineEvent[]): TimelineEvent[] {
  return [...events].sort((a, b) => {
    const dt = (a.startedAt ?? a.at) - (b.startedAt ?? b.at);
    if (dt !== 0) return dt;
    return (a.kind === 'turn-end' ? 0 : 1) - (b.kind === 'turn-end' ? 0 : 1);
  });
}
