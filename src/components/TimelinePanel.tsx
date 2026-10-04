import React, { useEffect, useRef } from 'react';
import { displayOrder, type TimelineEvent } from '../lib/timeline';
import { formatDuration } from './TabItem';

interface TimelinePanelProps {
  /** The active tab, or null when no tab is open. */
  tabId: string | null;
  /** The active tab's events, oldest first. */
  events: TimelineEvent[];
  /** True when the event's terminal line has left the scrollback. */
  isStale: (eventId: number) => boolean;
  onNavigate: (eventId: number) => void;
}

/** Within this distance of the bottom, new events auto-scroll into view. */
const STICK_BOTTOM_PX = 24;

function formatTime(at: number): string {
  const d = new Date(at);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function labelOf(ev: TimelineEvent): string {
  switch (ev.kind) {
    case 'turn-start':
      // Show what the user submitted; the hook degrades to no message when
      // jq is missing.
      return ev.msg ?? 'Turn started';
    case 'notice':
      // The hook degrades to no message when jq is missing.
      return ev.msg ?? 'Needs attention';
    case 'answer':
      // The tool result text of the question dialog.
      return ev.msg ?? 'Answered';
    case 'turn-end':
      return ev.duration != null
        ? `Turn finished · ${formatDuration(ev.duration)}`
        : 'Turn finished';
  }
}

export const TimelinePanel: React.FC<TimelinePanelProps> = ({
  tabId,
  events,
  isStale,
  onNavigate,
}) => {
  const listRef = useRef<HTMLDivElement>(null);
  // Follow the tail only while the user is at the tail: scrolling up to
  // re-read must not be yanked away by arriving events.
  const stick = useRef(true);

  // A tab switch swaps the whole list; always land on its newest event.
  useEffect(() => {
    stick.current = true;
  }, [tabId]);

  useEffect(() => {
    const el = listRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [events, tabId]);

  const onScroll = () => {
    const el = listRef.current;
    if (!el) return;
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < STICK_BOTTOM_PX;
  };

  // Execution-time order for humans; the incoming array's arrival order is
  // load-bearing elsewhere (FIFO navigation pairing), so sort a copy.
  const ordered = displayOrder(events);

  return (
    <aside className="timeline-panel" aria-label="Session timeline">
      <div className="timeline-header">Timeline</div>
      <div className="timeline-items" ref={listRef} onScroll={onScroll}>
        {ordered.length === 0 ? (
          <div className="timeline-empty">
            <p>No events yet</p>
            <p className="timeline-empty-hint">
              Claude Code hooks report turn events — see CLAUDE_HOOKS.md for the one-time setup.
            </p>
          </div>
        ) : (
          ordered.map((ev) => {
            const stale = isStale(ev.id);
            return (
              <button
                key={ev.id}
                className={`timeline-event kind-${ev.kind}${stale ? ' stale' : ''}`}
                disabled={stale}
                onClick={() => onNavigate(ev.id)}
                title={
                  stale
                    ? 'This point has scrolled out of the terminal history'
                    : 'Jump to this point in the terminal'
                }
              >
                <span className="timeline-dot" aria-hidden="true" />
                <span className="timeline-time">{formatTime(ev.startedAt ?? ev.at)}</span>
                {ev.queued ? (
                  <span
                    className="timeline-tag"
                    title="Submitted while the previous turn was still running; it executes when that turn finishes"
                  >
                    Queued
                  </span>
                ) : null}
                {/* The label shows the message in full; the title still
                    exposes the raw hook text. The button's own title keeps
                    the navigation hint on the dot/time. */}
                <span className="timeline-label" title={ev.msg ?? undefined}>
                  {labelOf(ev)}
                </span>
              </button>
            );
          })
        )}
      </div>
    </aside>
  );
};
