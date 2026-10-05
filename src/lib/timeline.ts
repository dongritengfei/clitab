import type { TabAnswer, TabNotice, TabStatus } from '../types';

/** The events the timeline tracks: a log of the *user's* own inputs — the
 *  prompts they submitted and the answers they picked. Everything Claude
 *  reports (tool calls, notifications, turn completion) is deliberately
 *  excluded: the tab dashboard already surfaces that state, and the timeline
 *  is a navigation aid for "what did I send, and where did it land". */
export type TimelineEventKind = 'turn-start' | 'answer';

/** One key event in a tab's session history. */
export interface TimelineEvent {
  /** Unique within the tab (timelines are stored per tab). */
  id: number;
  kind: TimelineEventKind;
  /** Epoch ms taken from the backend payload — authoritative, never Date.now(). */
  at: number;
  /** turn-start / answer: message text (the user's submitted prompt or the
   *  chosen answer); may be absent when the hook lacks jq. */
  msg?: string | null;
  /** turn-start only: the prompt arrived while a previous turn was still in
   *  flight — Claude Code queued it and auto-submits it the instant the
   *  previous turn stops (the hook fires at queue time and NOT again at
   *  submit). */
  queued?: boolean;
  /** turn-start only: execution start (epoch ms), stamped by
   *  `startQueuedTurn` when the backend's auto-submission payload arrives
   *  (exact time, also clears `queued`), or as a fallback by
   *  `stampQueuedStarts` when the Done payload arrives (the stop's time,
   *  keeps `queued`). Display prefers this over `at` (the queue time). */
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
 * - answer appears or its `at` changes      → answer     (at = answer.at)
 *
 * The answer rule keys on `at`, not on a null→non-null transition: every
 * payload re-sends an unchanged answer, and a fresh backend timestamp is the
 * reliable signal for "new answer". Notices, tool changes and the done
 * transition deliberately produce no rows (see TimelineEventKind); the done
 * timestamp still matters to the caller as the queued prompt's fallback
 * execution time (`stampQueuedStarts`).
 */
export class TimelineTracker {
  private prev: StatusSnapshot = { status: null, notice: null, answer: null };
  private nextId = 1;

  /** Feed one payload's snapshot; returns the newly detected events (oldest first). */
  push(snapshot: StatusSnapshot): TimelineEvent[] {
    const events: TimelineEvent[] = [];
    const { status, answer } = snapshot;

    if (answer && answer.at !== this.prev.answer?.at) {
      events.push({ id: this.nextId++, kind: 'answer', at: answer.at, msg: answer.msg });
    }
    // A prompt queued mid-turn re-enters `thinking` from `thinking` (its hook
    // fires at queue time), so a kind-transition check would drop it: key on
    // `since` instead — the backend stamps a fresh one per prompt
    // (begin_turn), and re-sent payloads carry the same one (dedup by
    // timestamp, like answer above).
    const prevThinking =
      this.prev.status?.kind === 'thinking' ? this.prev.status.since : undefined;
    if (status?.kind === 'thinking' && status.since !== prevThinking) {
      // An `auto` thinking is the backend modeling Claude Code's
      // auto-submission of the queue head (no hook re-fires): the queued
      // prompt's turn-start row already exists, so no new row — the caller
      // flips the existing one via `startQueuedTurn`. A `system` thinking is
      // a prompt Claude Code authored itself (an injected task notification):
      // a real turn, but not the user's input, so it gets no row either —
      // the backend ships the verdict, the renderer never re-derives it.
      if (!status.auto && !status.system) {
        const turnStart: TimelineEvent = {
          id: this.nextId++,
          kind: 'turn-start',
          at: status.since,
          msg: status.msg,
        };
        // A prompt arriving while the previous turn is still in flight
        // (thinking or tool) is one Claude Code queued: tag it so the panel
        // can explain why its timestamp precedes the turn's execution.
        const prevKind = this.prev.status?.kind;
        if (prevKind === 'thinking' || prevKind === 'tool') turnStart.queued = true;
        events.push(turnStart);
      }
    }

    this.prev = snapshot;
    // One payload can carry both a new answer and a turn transition;
    // backend timestamps decide the order.
    return events.sort((a, b) => a.at - b.at);
  }
}

/**
 * A done payload means the head of the prompt queue just executed: Claude
 * Code auto-submits the oldest queued prompt the instant the previous turn
 * stops (the hook does NOT fire again), so the stop's timestamp is the best
 * available execution time when the backend's auto-submission payload never
 * arrives. Stamps the oldest unstamped queued turn-start — FIFO. Returns a
 * new array when something was stamped (React state); the input array is
 * never mutated, and is returned as-is when there is nothing to stamp.
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
 * arrives at queue time, and the FIFO stamping in `stampQueuedStarts` relies
 * on that — so the panel renders this sorted copy instead. A flip
 * (`startQueuedTurn`) therefore moves the row to when it actually executed.
 * The sort is stable: same-millisecond events keep their arrival order.
 */
export function displayOrder(events: TimelineEvent[]): TimelineEvent[] {
  return [...events].sort((a, b) => (a.startedAt ?? a.at) - (b.startedAt ?? b.at));
}
