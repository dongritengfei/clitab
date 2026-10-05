# Timeline 摘要与导航 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 右侧常驻时间轴面板,展示活跃 tab 的关键会话事件(回合开始 / 请求确认 / 回合结束含耗时),点击事件把终端滚动到事发位置并短暂高亮。

**Architecture:** 纯渲染端实现,零后端改动。现有 `tab-status` 事件(OSC 7777 钩子协议)已携带完整状态与后端权威时间戳;`TimelineTracker` 对连续载荷做转换检测得到事件流,事件到达时用 xterm `registerMarker(0)` 绑定当前光标行——marker 自动跟随 scrollback 裁剪,`onDispose` 即事件失效信号;点击时 `scrollToLine(marker.line)` + `registerDecoration` 高亮 1.2s。

**Tech Stack:** React 18 + TypeScript、@xterm/xterm(marker / decoration / scrollToLine API)、现有 Tauri 事件通道。不新增依赖。

**Spec:** `docs/superpowers/specs/2026-10-02-timeline-navigation-design.md`

## Global Constraints

- **零 Rust 改动**:后端与 `cargo test` 完全不受影响;不改任何 IPC 载荷。
- **事件粒度**:只记 `turn-start`(prompt)/ `notice`(notify)/ `turn-end`(stop);`tool` 状态变化**不**产生事件。
- **时间戳**:一律取 `tab-status` 载荷里的后端 epoch-ms 值(`since` / `at`),**禁止** `Date.now()`。
- **上限**:每 tab `MAX_TIMELINE_EVENTS = 500` 条,超出丢最旧(并 dispose 其 marker)。
- **面板**:右侧常驻列,固定宽 230px;显示**活跃 tab** 的事件;UI 文案英文。
- **导航**:滚动 + 高亮 `backgroundColor: '#585b7066'`(与 selectionBackground 同色),持续 `HIGHLIGHT_MS = 1200`;焦点**留在面板**,不移回终端。
- **验证手段**(spec §6,项目现状无前端测试框架):每个任务 `npm run typecheck` 必须通过;带 UI 的任务按手动验证步骤在 `npm run tauri dev` 里核实。
- **文档同步**:README.md 与 README.zh-CN.md 必须同轮更新。
- 提交信息结尾带 `Co-Authored-By: Claude Code <noreply@anthropic.com>`。

## Review Focus

1. **scrollback 裁剪**(5000 行上限,Claude Code 重绘输出极易触发):被淘汰的事件必须置灰不可点,绝不能跳到漂移后的错误行——marker 机制保证行号,`onDispose` 保证置灰(Task 4 手动步骤 4 验证)。
2. **连续快速点击多个事件**:上一个高亮与定时器必须被替换清理,不留幽灵 decoration(Task 3 的 `highlight` ref 逻辑;Task 4 手动步骤 5 验证)。
3. **`tab-status` 早于终端挂载到达**(新建 tab 立刻触发钩子、或 attach 完成前):事件必须仍被记录,只是无 marker、显示为不可导航,不得抛错(Task 3 的 `term?.registerMarker` 守卫;Task 4 手动步骤 6 验证)。
4. **tab 关闭**:该 tab 的事件列表、tracker、marker 映射必须清掉;marker 随 `term.dispose()` 触发的 `onDispose` 不得把已关闭 tab 的键塞回 `staleEvents`(Task 3 的 `abandonTab` 清理 + `onDispose` 里的 map 存在性守卫)。
5. **同一 notice 重复载荷**:后续 `tab-status` 载荷会原样重发未变的 notice;只有 `notice.at` 变化才追加,不得重复记录;而 `at` 不同的两条 notify 必须各记一条(Task 2 的检测规则以 `at` 为键;Task 4 手动步骤 3 验证)。

---

### Task 1: 终端实例注册表(termRegistry)

**Files:**
- Create: `src/lib/termRegistry.ts`
- Modify: `src/components/Terminal.tsx`(挂载 effect 内 2 处:创建后注册、cleanup 里注销)

**Interfaces:**
- Consumes: 无(独立基础件)。
- Produces: `registerTerm(tabId: string, term: XTerm): void`、`unregisterTerm(tabId: string, term: XTerm): void`、`getTerm(tabId: string): XTerm | undefined` —— Task 3 依赖这三个函数。

背景(执行者须知):`Terminal.tsx` 在 `useEffect(..., [tabId])` 里创建 xterm 实例并存入 `termRef`;隐藏 tab 保持挂载(`visibility: hidden`),所以注册表在 tab 生命周期内始终有实例。React 状态里拿不到 xterm 实例,这个模块级注册表是渲染端从 tabId 找实例的唯一途径。

- [ ] **Step 1: 创建 `src/lib/termRegistry.ts`**

```ts
import type { Terminal as XTerm } from '@xterm/xterm';

/**
 * tabId → live xterm instance. React state cannot hold the terminal (it is a
 * mutable native-ish object owned by Terminal.tsx's mount effect), and the
 * timeline needs the instance to bind events to buffer lines. This module is
 * the single place that maps a tab id back to its terminal.
 */
const terms = new Map<string, XTerm>();

export function registerTerm(tabId: string, term: XTerm): void {
  terms.set(tabId, term);
}

/**
 * Unregister only if `term` is still the registered instance: React StrictMode
 * remounts register the new instance before the old cleanup runs, and the old
 * cleanup must not delete the new entry.
 */
export function unregisterTerm(tabId: string, term: XTerm): void {
  if (terms.get(tabId) === term) terms.delete(tabId);
}

export function getTerm(tabId: string): XTerm | undefined {
  return terms.get(tabId);
}
```

- [ ] **Step 2: 在 `Terminal.tsx` 注册/注销**

顶部加 import:

```ts
import { registerTerm, unregisterTerm } from '../lib/termRegistry';
```

挂载 effect 内,`termRef.current = term;` 之后加一行:

```ts
    fitRef.current = fitAddon;
    termRef.current = term;
    registerTerm(tabId, term);
```

同一 effect 的 cleanup 里,`term.dispose();` 之前加一行:

```ts
      if (fitRef.current === fitAddon) fitRef.current = null;
      if (termRef.current === term) termRef.current = null;
      unregisterTerm(tabId, term);
      term.dispose();
```

注意:只加这两行和 import,不动 Terminal.tsx 其他任何代码(IM workaround、手势过滤、replay 分类等都有注释说明的刻意设计)。

- [ ] **Step 3: typecheck**

Run: `npm run typecheck`
Expected: 无错误退出。

- [ ] **Step 4: Commit**

```bash
git add src/lib/termRegistry.ts src/components/Terminal.tsx
git commit -m "feat: terminal instance registry keyed by tab id

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 2: 事件模型与转换检测(timeline.ts)

**Files:**
- Create: `src/lib/timeline.ts`

**Interfaces:**
- Consumes: `TabStatus`、`TabNotice`(已存在于 `src/types.ts`,分别对应 Rust 的 `TabStatus` / `Notice`,时间戳均为 epoch ms)。
- Produces(Task 3 依赖):
  - `type TimelineEventKind = 'turn-start' | 'notice' | 'turn-end'`
  - `interface TimelineEvent { id: number; kind: TimelineEventKind; at: number; msg?: string | null; duration?: number | null }`
  - `interface StatusSnapshot { status: TabStatus | null; notice: TabNotice | null }`
  - `const MAX_TIMELINE_EVENTS = 500`
  - `class TimelineTracker { push(snapshot: StatusSnapshot): TimelineEvent[] }` —— 有状态,每 tab 一个实例;返回本次快照新检测出的事件(可能为空数组),按 `at` 升序。

设计要点(来自 spec §1):`tab-status` 载荷是**完整状态替换**,所以事件 = 状态转换。notice 的检测以 `at` 字段变化为准而非 null→非 null,因为 ack 只改渲染端本地状态、不产生载荷,后续载荷会原样重发同一个 notice(Review Focus #5)。

- [ ] **Step 1: 创建 `src/lib/timeline.ts`**

```ts
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
```

- [ ] **Step 2: typecheck**

Run: `npm run typecheck`
Expected: 无错误退出。

- [ ] **Step 3: Commit**

```bash
git add src/lib/timeline.ts
git commit -m "feat: timeline event model and tab-status transition detection

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 3: useTabManager 接线(事件历史、marker 绑定、导航)

**Files:**
- Modify: `src/hooks/useTabManager.ts`(接口 `TabManagerState`、hook 体、`abandonTab`、`tab-status` 监听器、返回值)

**Interfaces:**
- Consumes: Task 1 的 `getTerm(tabId)`;Task 2 的 `TimelineTracker` / `TimelineEvent` / `MAX_TIMELINE_EVENTS`;xterm 类型 `IMarker` / `IDecoration`(仅类型导入,运行时零依赖);现有 `TabStatusPayload`(`{ tab_id, status, notice }`)。
- Produces(Task 4 依赖,均挂在 `TabManagerState` 上):
  - `timelines: Record<string, TimelineEvent[]>` —— 每 tab 事件列表,时间正序
  - `staleEvents: ReadonlySet<string>` —— 键为 `` `${tabId}:${eventId}` ``,表示该事件的终端行已被裁剪
  - `navigateToEvent: (tabId: string, eventId: number) => void`

背景(执行者须知):`useTabManager` 是唯一与 Rust 通信的地方;`tab-status` 监听器已存在(更新 `tabs` state),本任务在**同一个监听器**里追加 timeline 逻辑,不新加事件订阅。监听器在 app 生命周期内注册一次,内部只能用 ref / 稳定 setter,不能引用会变的闭包值——现有代码已用 `tabsRef` 模式,新增逻辑同样只碰 ref 和 setter。marker 是非序列化对象,**不进 React state**:事件列表进 state,marker 走 ref 里的二级 Map。

- [ ] **Step 1: 顶部 import**

在现有 import 区加(`@xterm/xterm` 是**类型导入**,编译后消失):

```ts
import type { IDecoration, IMarker } from '@xterm/xterm';
import { getTerm } from '../lib/termRegistry';
import { MAX_TIMELINE_EVENTS, TimelineTracker, type TimelineEvent } from '../lib/timeline';
```

- [ ] **Step 2: 模块级常量与 `TabManagerState` 接口扩展**

在 `type StreamSink = ...` 附近加:

```ts
/** How long the jumped-to terminal line stays highlighted. */
const HIGHLIGHT_MS = 1200;
/** Same color as xterm's selectionBackground (Catppuccin surface2, translucent). */
const HIGHLIGHT_COLOR = '#585b7066';
```

`TabManagerState` 接口在 `resizePty` 之后追加三个成员:

```ts
  /** Per-tab timeline of key hook-protocol events (renderer-only history). */
  timelines: Record<string, TimelineEvent[]>;
  /** Keys `${tabId}:${eventId}` whose terminal line left the scrollback. */
  staleEvents: ReadonlySet<string>;
  /** Scroll the tab's terminal to a timeline event and highlight the line. */
  navigateToEvent: (tabId: string, eventId: number) => void;
```

- [ ] **Step 3: hook 体内新增 state 与 ref**

在 `const [error, setError] = useState<string | null>(null);` 之后加:

```ts
  const [timelines, setTimelines] = useState<Record<string, TimelineEvent[]>>({});
  const [staleEvents, setStaleEvents] = useState<ReadonlySet<string>>(() => new Set());

  // Timeline transition detection is per-tab stateful. Markers bind an event
  // to its terminal line; they are mutable xterm objects, so they live in a
  // ref (tabId → eventId → marker), never in state.
  const trackers = useRef(new Map<string, TimelineTracker>());
  const markers = useRef(new Map<string, Map<number, IMarker>>());
  // The one highlight decoration currently on screen, plus its expiry timer.
  const highlight = useRef<{ decoration: IDecoration; timer: number } | null>(null);
```

- [ ] **Step 4: `abandonTab` 清理**

现有 `abandonTab` 回调体内,`handlers.current.delete(tabId);` 之后加:

```ts
      trackers.current.delete(tabId);
      // The tab's markers die with its terminal instance; dropping the map
      // first also disarms their onDispose guards (see the tab-status
      // listener), so a closing tab never re-adds stale keys.
      markers.current.delete(tabId);
      setTimelines((prev) => {
        const { [tabId]: _dropped, ...rest } = prev;
        return rest;
      });
      setStaleEvents((prev) => {
        let next: Set<string> | null = null;
        for (const key of prev) {
          if (key.startsWith(`${tabId}:`)) (next ??= new Set(prev)).delete(key);
        }
        return next ?? prev;
      });
```

(`abandonTab` 的 `useCallback` 依赖数组保持 `[]` 不变——新引用的全是 ref 和稳定 setter。)

- [ ] **Step 5: `tab-status` 监听器追加 timeline 逻辑**

现有监听器(只做 `setTabs`)整体替换为:

```ts
      listen<TabStatusPayload>('tab-status', ({ payload }) => {
        // The backend sends the tab's complete protocol state; replace both
        // fields rather than merging, so a cleared notice really disappears.
        setTabs((prev) =>
          prev.map((tab) =>
            tab.id === payload.tab_id
              ? { ...tab, status: payload.status, notice: payload.notice }
              : tab
          )
        );

        // Timeline: derive events from the state transition, then bind each
        // new event to the terminal line it arrived at.
        const tabId = payload.tab_id;
        let tracker = trackers.current.get(tabId);
        if (!tracker) {
          tracker = new TimelineTracker();
          trackers.current.set(tabId, tracker);
        }
        const newEvents = tracker.push({ status: payload.status, notice: payload.notice });
        if (newEvents.length === 0) return;

        const existing = markers.current.get(tabId);
        const markerMap = existing ?? new Map<number, IMarker>();
        if (!existing) markers.current.set(tabId, markerMap);

        const term = getTerm(tabId);
        for (const ev of newEvents) {
          // No terminal mounted yet (mid-attach): record the event anyway —
          // it renders non-navigable instead of being lost.
          const marker = term?.registerMarker(0);
          if (!marker) continue;
          markerMap.set(ev.id, marker);
          const key = `${tabId}:${ev.id}`;
          marker.onDispose(() => {
            // Skip when the dispose was cleanup we initiated ourselves (tab
            // closed, event dropped by the cap): the map entry is already
            // gone in those cases, and the event is no longer rendered.
            if (!markers.current.get(tabId)?.has(ev.id)) return;
            setStaleEvents((prev) => new Set(prev).add(key));
          });
        }

        setTimelines((prev) => {
          const merged = [...(prev[tabId] ?? []), ...newEvents];
          if (merged.length <= MAX_TIMELINE_EVENTS) return { ...prev, [tabId]: merged };
          // Cap reached: drop the oldest events and release their markers.
          // Delete from the map *before* dispose so the onDispose guard above
          // sees them as cleaned-up, not as trimmed lines.
          for (const ev of merged.slice(0, merged.length - MAX_TIMELINE_EVENTS)) {
            const marker = markerMap.get(ev.id);
            markerMap.delete(ev.id);
            marker?.dispose();
          }
          return { ...prev, [tabId]: merged.slice(-MAX_TIMELINE_EVENTS) };
        });
      }),
```

- [ ] **Step 6: `navigateToEvent` 实现**

在 `dismissError` 定义附近加:

```ts
  const navigateToEvent = useCallback((tabId: string, eventId: number) => {
    const term = getTerm(tabId);
    const marker = markers.current.get(tabId)?.get(eventId);
    // A trimmed line has no valid position anymore: no-op (the UI grays the
    // event out via staleEvents, but double-guard here).
    if (!term || !marker || marker.isDisposed) return;

    term.scrollToLine(marker.line);

    // Replace a still-showing highlight from a previous jump, so rapid clicks
    // never leave ghost decorations or timers behind.
    if (highlight.current) {
      window.clearTimeout(highlight.current.timer);
      highlight.current.decoration.dispose();
      highlight.current = null;
    }
    const decoration = term.registerDecoration({
      marker,
      width: term.cols,
      height: 1,
      backgroundColor: HIGHLIGHT_COLOR,
    });
    if (!decoration) return;
    highlight.current = {
      decoration,
      timer: window.setTimeout(() => {
        decoration.dispose();
        if (highlight.current?.decoration === decoration) highlight.current = null;
      }, HIGHLIGHT_MS),
    };
    // Focus deliberately stays in the timeline panel: consecutive jumps
    // should not each cost a click back into the panel.
  }, []);
```

- [ ] **Step 7: 返回值扩展**

hook 末尾 `return { ... }` 里,`resizePty,` 之后加:

```ts
    timelines,
    staleEvents,
    navigateToEvent,
```

- [ ] **Step 8: typecheck**

Run: `npm run typecheck`
Expected: 无错误退出。(运行时行为在 Task 4 有 UI 后统一手动验证。)

- [ ] **Step 9: Commit**

```bash
git add src/hooks/useTabManager.ts
git commit -m "feat: timeline history, marker binding and navigation in useTabManager

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 4: 时间轴面板 UI(TimelinePanel + 布局 + 样式)

**Files:**
- Create: `src/components/TimelinePanel.tsx`
- Modify: `src/App.tsx`(第三列)、`src/App.css`(面板样式,文件末尾追加)

**Interfaces:**
- Consumes: Task 2 的 `TimelineEvent`;Task 3 的 `timelines` / `staleEvents` / `navigateToEvent`;现有 `formatDuration`(从 `./TabItem` 具名导入,已导出,勿重复实现)。
- Produces: `TimelinePanel` 组件(props 见下),App 专用,无下游任务。

- [ ] **Step 1: 创建 `src/components/TimelinePanel.tsx`**

```tsx
import React, { useEffect, useRef } from 'react';
import type { TimelineEvent } from '../lib/timeline';
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
      return 'Turn started';
    case 'notice':
      // The hook degrades to no message when jq is missing.
      return ev.msg ?? 'Needs attention';
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

  return (
    <aside className="timeline-panel" aria-label="Session timeline">
      <div className="timeline-header">Timeline</div>
      <div className="timeline-items" ref={listRef} onScroll={onScroll}>
        {events.length === 0 ? (
          <div className="timeline-empty">
            <p>No events yet</p>
            <p className="timeline-empty-hint">
              Claude Code hooks report turn events — see CLAUDE_HOOKS.md for the one-time setup.
            </p>
          </div>
        ) : (
          events.map((ev) => {
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
                <span className="timeline-time">{formatTime(ev.at)}</span>
                <span className="timeline-label">{labelOf(ev)}</span>
              </button>
            );
          })
        )}
      </div>
    </aside>
  );
};
```

- [ ] **Step 2: `App.tsx` 加第三列**

import 区加:

```tsx
import { TimelinePanel } from './components/TimelinePanel';
import type { TimelineEvent } from './lib/timeline';
```

`useTabManager()` 解构里,`resizePty,` 之后加 `timelines,`、`staleEvents,`、`navigateToEvent,`。

组件外(模块级)加稳定空数组,避免每次渲染新建 `[]`:

```tsx
const NO_EVENTS: TimelineEvent[] = [];
```

JSX:`<div className="terminal-area">...</div>` 的闭合标签之后、`</div>`(app-container)之前插入:

```tsx
      <TimelinePanel
        tabId={activeTabId}
        events={activeTabId ? timelines[activeTabId] ?? NO_EVENTS : NO_EVENTS}
        isStale={(eventId) => activeTabId !== null && staleEvents.has(`${activeTabId}:${eventId}`)}
        onNavigate={(eventId) => {
          if (activeTabId) navigateToEvent(activeTabId, eventId);
        }}
      />
```

(面板常驻渲染——没有 tab 时显示空状态,与 spec §4 一致。)

- [ ] **Step 3: `App.css` 末尾追加面板样式**

配色沿用现有 Catppuccin 值(base `#1e1e2e`、mantle `#181825`、surface `#313244`/`#45475a`、overlay `#6c7086`、blue `#89b4fa`、yellow `#f9e2af`、green `#a6e3a1`):

```css
/* Timeline Panel — right-hand session history for the active tab. Mirrors the
   tab list's chrome (same width family, header style, scrollbar). */
.timeline-panel {
  width: 230px;
  min-width: 230px;
  background: #181825;
  border-left: 1px solid #313244;
  display: flex;
  flex-direction: column;
}

.timeline-header {
  padding: 12px 16px;
  font-size: 12px;
  font-weight: 600;
  text-transform: uppercase;
  color: #6c7086;
  border-bottom: 1px solid #313244;
}

.timeline-items {
  flex: 1;
  overflow-y: auto;
  padding: 8px 0;
}

.timeline-event {
  display: grid;
  grid-template-columns: 8px auto 1fr;
  column-gap: 8px;
  align-items: baseline;
  width: 100%;
  padding: 6px 12px;
  background: none;
  border: none;
  border-left: 3px solid transparent;
  cursor: pointer;
  text-align: left;
  font-family: inherit;
  font-size: 12px;
  color: #cdd6f4;
}

.timeline-event:hover:not(.stale) {
  background: #1e1e2e;
}

.timeline-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  align-self: center;
}

.kind-turn-start .timeline-dot {
  background: #89b4fa;
}

.kind-notice .timeline-dot {
  background: #f9e2af;
}

.kind-turn-end .timeline-dot {
  background: #a6e3a1;
}

.kind-notice .timeline-label {
  color: #f9e2af;
}

.timeline-time {
  font-size: 11px;
  color: #6c7086;
  font-variant-numeric: tabular-nums;
}

.timeline-label {
  min-width: 0;
  overflow-wrap: anywhere;
}

/* The event's terminal line left the scrollback: visible as history, but
   there is nothing to jump to anymore. */
.timeline-event.stale {
  opacity: 0.4;
  cursor: default;
}

.timeline-empty {
  padding: 16px;
  color: #6c7086;
  font-size: 12px;
}

.timeline-empty-hint {
  margin-top: 8px;
  font-size: 11px;
  line-height: 1.5;
}
```

- [ ] **Step 4: typecheck**

Run: `npm run typecheck`
Expected: 无错误退出。

- [ ] **Step 5: 手动验证(覆盖 Review Focus 1–3、5)**

Run: `npm run tauri dev`,然后逐项核对:

1. **空状态**:新 tab 的右侧面板显示 "No events yet" 与钩子提示;终端宽度比之前窄约 230px 且输出正常重排(ResizeObserver 生效)。
2. **事件产生**:在 tab 里依次执行(每条回车):

   ```bash
   printf '\033]7777;{"e":"prompt"}\033\\'
   printf '\033]7777;{"e":"notify","msg":"needs permission"}\033\\'
   printf '\033]7777;{"e":"notify","msg":"needs permission"}\033\\'
   printf '\033]7777;{"e":"stop"}\033\\'
   ```

   面板应出现:`Turn started`(蓝点)、两条 `needs permission`(黄点,间隔 ≥1ms 即两条——Review Focus #5)、`Turn finished · Xs`(绿点);时间戳与本机时钟一致;`tool` 事件(`printf '\033]7777;{"e":"tool","tool":"Bash"}\033\\'`)**不**产生新行,只影响左侧标签仪表盘。
3. **导航+高亮**:先 `seq 1 100` 制造一些输出,再点击 `Turn started` → 终端滚回该行、整行出现半透明高亮约 1.2s 后消失;焦点仍在面板(直接再点另一个事件可用)。
4. **裁剪置灰(Review Focus #1)**:记下最早的事件,执行 `seq 1 9000`(超过 5000 行 scrollback)→ 早期事件变灰、`disabled`、hover 提示 "scrolled out";点击无反应;较新的事件仍可导航。
5. **连续点击(Review Focus #2)**:快速连点两个可导航事件 → 只有最后点击的行有高亮,无残留色块。
6. **早于挂载的事件(Review Focus #3)**:⌘T 新建 tab 后**立刻**(1 秒内)粘贴执行 prompt 的 printf → 事件必须出现在面板(可能置灰,取决于终端是否已挂载),app 不崩溃、控制台无红色报错。
7. **tab 生命周期**:切换 tab → 面板列表随之切换、滚到底部;关闭一个有事件的 tab → 面板显示新活跃 tab,无报错。

Expected: 全部符合;任何一项不符先修复再进入下一步。

- [ ] **Step 6: Commit**

```bash
git add src/components/TimelinePanel.tsx src/App.tsx src/App.css
git commit -m "feat: right-hand timeline panel with click-to-navigate

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 5: 文档同步(README ×2 + CLAUDE.md)

**Files:**
- Modify: `README.md`(Features 列表、Claude Code integration 一节)
- Modify: `README.zh-CN.md`(特性列表、Claude Code 集成一节——与英文版逐条对应)
- Modify: `CLAUDE.md`(Frontend 一节)

**Interfaces:**
- Consumes: 无代码依赖;描述 Task 1–4 的既成事实。
- Produces: 无。

- [ ] **Step 1: `README.md` Features 列表**

在 "**Tab dashboard**" 条目之后插入:

```markdown
- **Session timeline** — the right-hand panel logs the key moments of the
  active tab's Claude Code session: turn started, permission requests, turn
  finished (with duration). Click an entry to scroll the terminal to that
  point; entries whose output has scrolled out of the 5000-line history are
  grayed out. Powered by the same hooks as the dashboard.
```

- [ ] **Step 2: `README.md` Claude Code integration 一节**

段落 "The attention flash and the tab dashboard are powered by Claude Code hooks:" 改为:

```markdown
The attention flash, the tab dashboard and the session timeline are powered by
Claude Code hooks:
```

(该句其余部分不动。)

- [ ] **Step 3: `README.zh-CN.md` 特性列表**

在 "**标签仪表盘**" 条目之后插入:

```markdown
- **会话时间轴** — 右侧面板按时间记录当前标签 Claude Code 会话的关键时刻:
  回合开始、请求确认、回合结束(含耗时)。点击条目即把终端滚动到当时的
  输出位置;已滚出 5000 行历史缓冲的条目会置灰。与仪表盘共用同一套 hooks。
```

- [ ] **Step 4: `README.zh-CN.md` Claude Code 集成一节**

句子 "关注闪烁与标签仪表盘由 Claude Code hooks 驱动:" 改为:

```markdown
关注闪烁、标签仪表盘与会话时间轴由 Claude Code hooks 驱动:
```

- [ ] **Step 5: `CLAUDE.md` Frontend 一节**

在 `hooks/useTabManager.ts` 与 `types.ts` 两个条目之间插入:

```markdown
- `lib/timeline.ts` — `TimelineTracker` derives the per-tab timeline (turn-start / notice / turn-end) from `tab-status` payload *transitions*; notice detection keys on `notice.at` changes, not null transitions (payloads re-send unchanged notices). Renderer-only by design: the history and its navigation targets die with the webview, and that is accepted. `lib/termRegistry.ts` maps tabId → xterm instance; `useTabManager` binds each timeline event to a terminal line with `registerMarker(0)` — markers track scrollback trimming (`onDispose` ⇒ the event grays out), and click-to-navigate is `scrollToLine(marker.line)` plus a 1.2 s `registerDecoration` highlight. The panel itself is `components/TimelinePanel.tsx` (constant right column, active tab only).
```

- [ ] **Step 6: 核对两份 README 逐条对应,然后 Commit**

检查:英文 Features 与中文特性条目数量一致、新条目位置一致(都在 dashboard 之后)。

```bash
git add README.md README.zh-CN.md CLAUDE.md
git commit -m "docs: session timeline in READMEs and CLAUDE.md

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

## Self-Review 结果

- **Spec 覆盖**:§1 数据模型/转换检测 → Task 2;§2 注册表/Marker → Task 1+3;§3 导航高亮 → Task 3;§4 UI/布局 → Task 4;§5 边界(tab 关闭、早于挂载、连续点击、重载清空、钩子未装=空状态)→ Task 3 清理逻辑 + Task 4 手动步骤;§6 验证与文档 → 各任务 typecheck + Task 4 Step 5 + Task 5。500 条上限 → Task 3 Step 5。无缺口。
- **占位符扫描**:所有代码步骤均为完整可用代码;手动验证均为具体命令与预期。无 TBD。
- **类型一致性**:`TimelineEvent` / `StatusSnapshot` / `MAX_TIMELINE_EVENTS`(Task 2 定义)与 Task 3/4 的 import 一致;`getTerm/registerTerm/unregisterTerm`(Task 1)与 Task 3 一致;`timelines/staleEvents/navigateToEvent`(Task 3 Produces)与 Task 4 Consumes 一致;`formatDuration` 复用 TabItem 现有导出。
- **Review Focus**:5 条均已落到任务(见各条括注)。
