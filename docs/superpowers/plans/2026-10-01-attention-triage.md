# 关注分诊(Attention Triage)Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把一次性的"关注闪烁"升级为持久的"等待输入"队列:⌘J 轮转分诊、Dock 角标计数、后台时弹 macOS 通知且点击直达对应标签。

**Architecture:** 队列状态是 `Registry` 里每个 `TabRecord` 的 `waiting: bool`(后端权威,webview reload 不丢)。现有三个 `tab-flash` 触发点置位,`pty_input` 成功清除。新模块 `attention.rs` 独占转移的副作用:`tab-waiting` 事件、角标重算、失焦时 spawn 一个阻塞在 `wait_for_click` 上的通知线程(点击 → `focus-tab` 事件)。⌘J 走原生菜单转发,renderer 在本地镜像里环形查找。

**Tech Stack:** Rust(tauri 2.11.3 自带 `Window::set_badge_count`;`mac-notification-sys` 0.6 直依赖,target-gated 到 macOS)、React 18 + TypeScript。

**Spec:** `docs/superpowers/specs/2026-10-01-attention-triage-design.md`

## Global Constraints

- **开工前提**:工作区已有未提交改动(cwd 继承功能 + README 文案,是另一项已完成的工作)。开始前先让用户把它作为独立 commit 提交;**任务提交只 `git add` 该任务列出的文件**,不得裹挟无关改动。
- UI 文案一律英文:通知 body `"Waiting for input"`,菜单项 `"Next Waiting Tab"`;README 中英双语同步。
- 快捷键必须走 `menu.rs` 原生菜单(CLAUDE.md 规则:不得在 renderer 里用 keydown 重新实现)。
- Mutex 一律经 `pty::lock()` 加锁(防中毒);`attention.rs` 只拿 registry 锁,绝不碰 session map(维持"两锁不同持"不变量)。
- 事件名两端逐字一致:`tab-waiting`(payload `{tab_id, waiting}`)、`focus-tab`(payload `{tab_id}`);payload 键用 snake_case(与现有 `tab-flash` 等一致),命令响应字段用 camelCase serde。
- 主窗口 label 是 `"main"`(`tauri.conf.json` 的 `app.windows[].label`)。
- 验证命令:Rust `cargo test --manifest-path src-tauri/Cargo.toml --lib`;前端只有 `npm run typecheck`(无测试跑器、无 linter)。
- 不改动:CSP、capabilities(非 tauri 插件)、`TAB_GONE` 协议、shell 集成 hook、CLAUDE_HOOKS.md。

## Review Focus

1. **连发信号**(Claude 连敲多次 BEL / 空闲 watcher 反复触发):只进队一次、只弹一条通知——由 Task 1 的转移语义测试钉死(`set_waiting` 二次调用返回 false)。
2. **通知滞留期间标签被关闭**:点击通知 → `focus-tab` 指向不存在的标签 → renderer 必须静默忽略(Task 5 守卫代码 + Task 7 手动步骤 4e)。
3. **⌘J 边界**:队列空、或只有当前标签自己在等待 → 完全无动作、无焦点抖动(Task 5 环形扫描不含 active + Task 7 手动步骤 4c/4d)。
4. **webview reload**:圆点与 ⌘J 队列从 `list_tabs` 恢复(`TabResponse.waiting`),角标本来就归后端(Task 4 字段 + Task 7 手动步骤 4f)。
5. **在非等待标签里打字**:`respond` 必须是无声 no-op,不发事件不动角标(Task 1 `clear_waiting` 转移测试)。

---

### Task 1: Registry 增加 waiting 状态

**Files:**
- Modify: `src-tauri/src/pty/registry.rs`
- Test: `src-tauri/src/pty/registry.rs`(文件内 `mod tests`,沿用现有模式)

**Interfaces:**
- Consumes: 无(纯状态层)。
- Produces: `TabRecord.waiting: bool`;`Registry::set_waiting(&self, id: &str) -> bool`(仅 false→true 返回 true);`Registry::clear_waiting(&self, id: &str) -> bool`(仅 true→false 返回 true);`Registry::waiting_count(&self) -> usize`。Task 3/4 依赖这四个名字。

- [ ] **Step 1: 写失败测试**

在 `registry.rs` 的 `mod tests` 末尾追加:

```rust
    #[test]
    fn waiting_transitions_fire_only_once() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        assert!(registry.set_waiting("t1"));
        assert!(!registry.set_waiting("t1"), "second signal is not a transition");
        assert_eq!(registry.waiting_count(), 1);
        assert!(registry.clear_waiting("t1"));
        assert!(!registry.clear_waiting("t1"), "already out of the queue");
        assert_eq!(registry.waiting_count(), 0);
        assert!(
            registry.set_waiting("t1"),
            "responding and ringing again re-enters the queue"
        );
    }

    #[test]
    fn removed_tabs_leave_the_queue() {
        let registry = Registry::new();
        registry.insert("t1".into(), "/a".into());
        registry.insert("t2".into(), "/b".into());
        registry.set_waiting("t1");
        registry.set_waiting("t2");
        registry.remove("t1");
        assert_eq!(registry.waiting_count(), 1);
    }

    #[test]
    fn unknown_tab_waiting_is_ignored() {
        let registry = Registry::new();
        assert!(!registry.set_waiting("nope"));
        assert!(!registry.clear_waiting("nope"));
        assert_eq!(registry.waiting_count(), 0);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib registry`
Expected: 编译失败,`no method named 'set_waiting'` 等三个方法缺失。

- [ ] **Step 3: 最小实现**

`TabRecord` 加字段(带注释,文档化"输入才清除"语义):

```rust
    /// True while the tab is waiting for user input — the triage queue.
    /// Set at every `tab-flash` trigger; cleared only when the user types
    /// into the tab (`pty_input`). Switching tabs does NOT clear it.
    pub waiting: bool,
```

`insert()` 的构造里加 `waiting: false,`(在 `has_program_title: false,` 之后)。

在 `clear_program_title` 之后、`list()` 之前加三个方法:

```rust
    /// Mark a tab as waiting for input. Returns true only on the
    /// false→true transition, so repeated signals (BEL spam, the idle
    /// watcher re-firing) notify exactly once per queue entry.
    pub fn set_waiting(&self, id: &str) -> bool {
        let mut tabs = lock(&self.tabs);
        match tabs.iter_mut().find(|t| t.id == id) {
            Some(tab) if !tab.waiting => {
                tab.waiting = true;
                true
            }
            _ => false,
        }
    }

    /// Take a tab out of the waiting queue (the user typed in it). Returns
    /// true only on the true→false transition.
    pub fn clear_waiting(&self, id: &str) -> bool {
        let mut tabs = lock(&self.tabs);
        match tabs.iter_mut().find(|t| t.id == id) {
            Some(tab) if tab.waiting => {
                tab.waiting = false;
                true
            }
            _ => false,
        }
    }

    /// How many tabs are in the triage queue; drives the Dock badge.
    pub fn waiting_count(&self) -> usize {
        lock(&self.tabs).iter().filter(|t| t.waiting).count()
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: 全部 PASS(含既有 registry 测试——它们都经 `insert()` 构造,新字段不破坏)。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/pty/registry.rs
git commit -m "Registry: waiting-for-input state with transition-only setters

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 2: menu.rs 增加 ⌘J

**Files:**
- Modify: `src-tauri/src/menu.rs`
- Test: `src-tauri/src/menu.rs`(文件内 `mod tests`)

**Interfaces:**
- Consumes: 无。
- Produces: `menu::NEXT_WAITING = "next-waiting"`;该 id 经 `is_tab_action` 放行后由现有 `menu-shortcut` 事件转发。Task 5 的 renderer 匹配字符串 `"next-waiting"`。

- [ ] **Step 1: 写失败测试**

在 `tab_actions_are_forwarded` 测试里追加一行断言:

```rust
        assert!(is_tab_action(NEXT_WAITING));
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib menu`
Expected: 编译失败,`cannot find value 'NEXT_WAITING'`。

- [ ] **Step 3: 最小实现**

常量区(`SELECT_TAB` 之后)加:

```rust
pub const NEXT_WAITING: &str = "next-waiting";
```

`is_tab_action` 的 `matches!` 加一项:

```rust
    matches!(id, NEW_TAB | CLOSE_TAB | NEXT_TAB | PREV_TAB | NEXT_WAITING)
```

`build()` 里 `prev_tab` 之后构造:

```rust
    let next_waiting = item(app, NEXT_WAITING, "Next Waiting Tab", "CmdOrCtrl+J")?;
```

Tabs 子菜单在 `.item(&prev_tab)` 之后、`.separator()` 之前插入 `.item(&next_waiting)`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: 全部 PASS。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/menu.rs
git commit -m "Menu: ⌘J forwards next-waiting to the renderer

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 3: attention.rs(角标 + 通知 + 事件)与依赖

**Files:**
- Create: `src-tauri/src/attention.rs`
- Modify: `src-tauri/src/lib.rs`(仅加 `mod attention;`)
- Modify: `src-tauri/Cargo.toml`(target-gated 依赖)
- Test: `src-tauri/src/attention.rs`(文件内 `mod tests`,只测纯函数)

**Interfaces:**
- Consumes: Task 1 的 `Registry::{set_waiting, clear_waiting, waiting_count, get}`。
- Produces:
  - `attention::enter_waiting(app: &AppHandle, registry: &Registry, tab_id: &str)` — Task 4 在三个 flash 点调用;
  - `attention::respond(app: &AppHandle, registry: &Registry, tab_id: &str)` — Task 4 在 `write_input` 成功后调用;
  - `attention::update_badge(app: &AppHandle, registry: &Registry)` — Task 4 在标签移除后调用;
  - `attention::badge_for(count: usize) -> Option<i64>`;
  - 事件 `tab-waiting {tab_id, waiting}`、`focus-tab {tab_id}` — Task 5 的 renderer 监听这两个名字。

- [ ] **Step 1: 加依赖**

`src-tauri/Cargo.toml` 末尾(`[dependencies]` 段之后)加:

```toml
[target.'cfg(target_os = "macos")'.dependencies]
mac-notification-sys = "0.6"
```

- [ ] **Step 2: 写失败测试**

创建 `src-tauri/src/attention.rs`,先只放纯函数和测试:

```rust
/// Badge mapping: `None` hides the badge on macOS; a count of 0 must not
/// render as "0".
pub fn badge_for(count: usize) -> Option<i64> {
    (count > 0).then_some(count as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_hides_the_badge() {
        assert_eq!(badge_for(0), None);
        assert_eq!(badge_for(1), Some(1));
        assert_eq!(badge_for(3), Some(3));
    }
}
```

`lib.rs` 顶部 `mod menu;` 之前加 `mod attention;`。

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib attention`
Expected: PASS(`badge_for` 无外部依赖,先立住;下面补全模块时保持绿)。

- [ ] **Step 3: 补全模块实现**

`attention.rs` 完整内容(模块文档解释职责边界,与 spec 一致):

```rust
//! Side effects of the "waiting for input" triage queue: the Dock badge,
//! macOS notifications, and the events the renderer mirrors.
//!
//! The queue state itself lives in the [`Registry`](crate::pty::registry::Registry)
//! (`waiting` on each tab record); this module is the only place that reacts
//! to transitions:
//!
//!   * `tab-waiting` keeps the renderer's mirror in sync;
//!   * the badge is the count of waiting tabs, recomputed on every change so
//!     it stays correct across a webview reload;
//!   * a notification fires only while the main window is unfocused, one per
//!     false→true transition. Its thread blocks in `send_notification` until
//!     the user clicks or dismisses; a click focuses the window and routes
//!     back to the tab via `focus-tab`.
//!
//! Locking: only the registry lock is ever taken here — never the session
//! map — preserving the "two locks, never held together" invariant.

use crate::pty::registry::Registry;
use tauri::{AppHandle, Emitter, Manager};

/// The window label in `tauri.conf.json`; must match `app.windows[].label`.
const MAIN_WINDOW: &str = "main";

/// Badge mapping: `None` hides the badge on macOS; a count of 0 must not
/// render as "0".
pub fn badge_for(count: usize) -> Option<i64> {
    (count > 0).then_some(count as i64)
}

pub fn update_badge(app: &AppHandle, registry: &Registry) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.set_badge_count(badge_for(registry.waiting_count()));
    }
}

/// A tab rang for attention (BEL, `claude-done`, stalled turn): enter the
/// triage queue. Called at every site that emits `tab-flash`, after any
/// registry title updates so a notification carries the title the tab bar
/// currently shows.
pub fn enter_waiting(app: &AppHandle, registry: &Registry, tab_id: &str) {
    if !registry.set_waiting(tab_id) {
        return; // already queued: repeated signals notify once per entry
    }
    let _ = app.emit(
        "tab-waiting",
        serde_json::json!({ "tab_id": tab_id, "waiting": true }),
    );
    update_badge(app, registry);

    // Watching the app, the flash and the badge are noise enough.
    let focused = app
        .get_webview_window(MAIN_WINDOW)
        .map(|w| w.is_focused())
        .unwrap_or(false);
    if focused {
        return;
    }
    let title = registry
        .get(tab_id)
        .map(|tab| tab.title)
        .unwrap_or_else(|| "clitab".to_string());
    spawn_notification(app.clone(), tab_id.to_string(), title);
}

/// The user typed into this tab: it answered, leave the queue.
pub fn respond(app: &AppHandle, registry: &Registry, tab_id: &str) {
    if !registry.clear_waiting(tab_id) {
        return; // was not waiting: a silent no-op
    }
    let _ = app.emit(
        "tab-waiting",
        serde_json::json!({ "tab_id": tab_id, "waiting": false }),
    );
    update_badge(app, registry);
}

/// One thread per notification: `send_notification` blocks (condvar inside
/// `mac-notification-sys`) until the user clicks or dismisses it, and the
/// response is what routes the click back to `tab_id`.
#[cfg(target_os = "macos")]
fn spawn_notification(app: AppHandle, tab_id: String, title: String) {
    std::thread::spawn(move || {
        use mac_notification_sys::{Notification, NotificationResponse};
        let response = Notification::new()
            .title(&title)
            .message("Waiting for input")
            .wait_for_click(true)
            .send();
        if matches!(response, Ok(NotificationResponse::Click)) {
            if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                let _ = window.set_focus();
            }
            // The tab may be gone by now; the renderer guards on its mirror.
            let _ = app.emit("focus-tab", serde_json::json!({ "tab_id": tab_id }));
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn spawn_notification(_app: AppHandle, _tab_id: String, _title: String) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_hides_the_badge() {
        assert_eq!(badge_for(0), None);
        assert_eq!(badge_for(1), Some(1));
        assert_eq!(badge_for(3), Some(3));
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: 全部 PASS。若 `set_badge_count` / `is_focused` / `set_focus` 报方法不存在,核对 tauri 版本是否 2.11.3(`Cargo.lock`),并确认 `use tauri::Manager` 在作用域内。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/attention.rs src-tauri/src/lib.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "Attention: triage side effects (badge, notifications, tab-waiting events)

Notifications go through mac-notification-sys directly: the official
plugin ignores actions on desktop and has no click callback. One blocked
thread per notification routes the click back to its tab id.

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 4: 后端接线(session / manager / lib)

**Files:**
- Modify: `src-tauri/src/pty/session.rs`(Bell、PromptReady、空闲 watcher 三处 + watcher 需要 registry 克隆)
- Modify: `src-tauri/src/pty/manager.rs`(`write_input` / `close_tab` / `remove_session`)
- Modify: `src-tauri/src/lib.rs`(`TabResponse` 加 `waiting`)
- Test: 无新增单测(接线全是 AppHandle 副作用,无法单测);回归 = 既有测试全绿。

**Interfaces:**
- Consumes: Task 3 的 `attention::{enter_waiting, respond, update_badge}`;Task 1 的 `TabRecord.waiting`。
- Produces: `TabResponse.waiting`(camelCase serde → 前端 `TabResponse.waiting`,Task 5 消费);运行时行为:三个 flash 点进队、`pty_input` 成功出队、标签移除后角标重算。

- [ ] **Step 1: session.rs — watcher 拿到 registry**

`PtySession::new` 中,reader 线程块**之前**(`let last_activity = ...` 与 `let program_active = ...` 之后)加:

```rust
        // The attention watcher needs the registry too (enter_waiting), and
        // the reader thread takes ownership of the original below.
        let watcher_registry = Arc::clone(&registry);
```

attention watcher 线程块内,捕获列表(`let program_active = Arc::clone(&program_active);` 之后)加:

```rust
            let registry = watcher_registry;
```

- [ ] **Step 2: session.rs — 三个 flash 点进队**

`handle_osc` 的 `OscEvent::Bell` 分支,在 emit `tab-flash` 之前加:

```rust
                crate::attention::enter_waiting(app, registry, tab_id);
```

`OscEvent::PromptReady` 分支,在 `*lock(last_activity) = Instant::now();` 之后、emit 之前加同一行。**位置重要**:必须在 `clear_program_title` 之后调用,通知标题才与标签栏当前显示一致(回合结束后标题已回落为 cwd)。

attention watcher 的 `if !flashed { ... }` 块内,`flashed = true;` 之后、emit 之前加:

```rust
                        crate::attention::enter_waiting(&app, &registry, &tab_id);
```

- [ ] **Step 3: manager.rs — 输入清除、移除重算**

`write_input` 改为:

```rust
    pub fn write_input(&self, tab_id: &str, data: &[u8]) -> Result<(), ManagerError> {
        self.session(tab_id)?.write(data)?;
        // Typing into the tab is the answer: it leaves the triage queue.
        crate::attention::respond(&self.app, &self.registry, tab_id);
        Ok(())
    }
```

`close_tab` 的 `self.registry.remove(tab_id);` 之后、`Ok(())` 之前,以及 `remove_session` 的 `self.registry.remove(tab_id);` 之后,各加:

```rust
        crate::attention::update_badge(&self.app, &self.registry);
```

(`remove_session` 返回 `()`,直接加在末尾。)

- [ ] **Step 4: lib.rs — TabResponse 带 waiting**

`TabResponse` 结构体加字段(放在 `has_claude_title` 之后):

```rust
    pub waiting: bool,
```

`impl From<TabRecord> for TabResponse` 的构造里加:

```rust
            waiting: tab.waiting,
```

- [ ] **Step 5: 回归**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: 全部 PASS,无新 warning(`cargo build` 若有 dead_code 警告说明接线漏了调用点)。

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/pty/session.rs src-tauri/src/pty/manager.rs src-tauri/src/lib.rs
git commit -m "Wire the triage queue into flash triggers, pty_input and tab removal

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 5: 前端类型镜像 + useTabManager

**Files:**
- Modify: `src/types.ts`
- Modify: `src/hooks/useTabManager.ts`
- Test: 无跑器;验证 = `npm run typecheck`。

**Interfaces:**
- Consumes: Task 4 的 `TabResponse.waiting`;Task 3 的事件 `tab-waiting {tab_id, waiting}` / `focus-tab {tab_id}`;Task 2 的 `menu-shortcut` id `"next-waiting"`。
- Produces: `Tab.waiting: boolean`(Task 6 的 TabItem 消费);内部 `jumpToNextWaiting`(⌘J 行为)。

- [ ] **Step 1: types.ts**

`Tab` 接口加字段(`flashing` 之后):

```ts
  /** Backend-owned: the tab is waiting for input (triage queue). Cleared
   *  only by typing into it, never by switching. */
  waiting: boolean;
```

`TabResponse` 加字段(`hasClaudeTitle` 之后):

```ts
  waiting: boolean;
```

文件末尾(`TAB_GONE` 之前)加两个 payload 类型:

```ts
export interface TabWaitingPayload {
  tab_id: string;
  waiting: boolean;
}

/** Emitted when the user clicks a "Waiting for input" notification. */
export interface FocusTabPayload {
  tab_id: string;
}
```

注意:`toTab()` 是 `{ ...response, flashing: false }`,`waiting` 随 spread 自动带上,无需改动。

- [ ] **Step 2: useTabManager.ts — import**

import 列表加 `type FocusTabPayload,` 和 `type TabWaitingPayload,`(按现有字母序插入)。

- [ ] **Step 3: useTabManager.ts — jumpToNextWaiting**

`cycleTab` 定义之后加:

```ts
  // ⌘J: triage. Round-robin from the tab *after* the active one; the active
  // tab is never a target (you are already looking at it). An empty queue
  // does nothing at all — no focus change, no toast.
  const jumpToNextWaiting = useCallback(() => {
    const list = tabsRef.current;
    const active = list.findIndex((tab) => tab.id === activeTabRef.current);
    for (let step = 1; step < list.length; step++) {
      const candidate = list[(active + step + list.length) % list.length];
      if (candidate?.waiting) {
        switchTab(candidate.id);
        return;
      }
    }
  }, [switchTab]);
```

(`active` 为 -1 时 `step=1` 落到 index 0,行为正确,无需特判。)

- [ ] **Step 4: useTabManager.ts — actions 与 menu-shortcut**

`const actions = { createTab, closeActiveTab, selectTabByIndex, cycleTab };` 改为:

```ts
  const actions = { createTab, closeActiveTab, selectTabByIndex, cycleTab, switchTab, jumpToNextWaiting };
```

`menu-shortcut` 监听器的 `switch (payload.id)` 里加一个 case(`prev-tab` 之后):

```ts
          case 'next-waiting':
            actions.jumpToNextWaiting();
            break;
```

- [ ] **Step 5: useTabManager.ts — 两个新监听器**

`listeners.push(...)` 里,`prompt-ready` 监听之后加:

```ts
      listen<TabWaitingPayload>('tab-waiting', ({ payload }) => {
        setTabs((prev) =>
          prev.map((tab) =>
            tab.id === payload.tab_id ? { ...tab, waiting: payload.waiting } : tab
          )
        );
      }),
      listen<FocusTabPayload>('focus-tab', ({ payload }) => {
        // The tab may have been closed while its notification sat in the
        // notification center; switching to a ghost would blank the UI.
        if (tabsRef.current.some((tab) => tab.id === payload.tab_id)) {
          actionsRef.current.switchTab(payload.tab_id);
        }
      }),
```

- [ ] **Step 6: typecheck + Commit**

Run: `npm run typecheck`
Expected: 无错误。

```bash
git add src/types.ts src/hooks/useTabManager.ts
git commit -m "Renderer: mirror the waiting queue, ⌘J round-robin, focus-tab jumps

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 6: 标签栏 waiting 圆点

**Files:**
- Modify: `src/components/TabItem.tsx`
- Modify: `src/App.css`
- Test: 无跑器;验证 = `npm run typecheck` + Task 7 目测。

**Interfaces:**
- Consumes: Task 5 的 `Tab.waiting`。
- Produces: 纯视觉,无下游。

- [ ] **Step 1: TabItem.tsx**

常量区(`CHROME_WIDTH` 之后)加:

```tsx
/** Extra width of the waiting dot when present: 6px dot + 4px gap. */
const WAITING_DOT_WIDTH = 10;
```

组件体内,`badge` 计算之前加:

```tsx
  // The dot only costs width while it is there; folding it into the chrome
  // budget keeps the title ellipsis accurate either way.
  const chromeWidth = CHROME_WIDTH + (tab.waiting ? WAITING_DOT_WIDTH : 0);
```

`useLayoutEffect` 内两处 `CHROME_WIDTH` 改为 `chromeWidth`,依赖数组 `[tab.title, tab.cwd, isPath]` 改为 `[tab.title, tab.cwd, isPath, chromeWidth]`。

`className` 模板串追加:

```tsx
      className={`tab-item ${isActive ? 'active' : ''} ${tab.flashing ? 'flashing' : ''} ${tab.waiting ? 'waiting' : ''}`}
```

`<span className="tab-index">…</span>` 与 `<div className="tab-text">` 之间插入:

```tsx
      {tab.waiting && <span className="waiting-dot" aria-hidden="true" />}
```

- [ ] **Step 2: App.css**

`.tab-index` 规则块之前(即 `.tab-item.flashing` / `@keyframes flash` 之后)加:

```css
/* Steady amber dot: the tab is in the waiting-for-input triage queue. The
   flash animation means "just rang"; the dot survives switching away and
   only goes out when the user types into that tab. */
.waiting-dot {
  flex-shrink: 0;
  align-self: flex-start;
  width: 6px;
  height: 6px;
  margin-top: 8px;
  border-radius: 50%;
  background: #fab387;
}
```

(`.tab-item` 是横向 flex;`align-self: flex-start` + `margin-top` 让圆点对齐标题首行,与 `.tab-index` 同法。色值取自现有 catppuccin 系配色。)

- [ ] **Step 3: typecheck + Commit**

Run: `npm run typecheck`
Expected: 无错误。

```bash
git add src/components/TabItem.tsx src/App.css
git commit -m "TabItem: steady amber dot for tabs waiting for input

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

### Task 7: 手动验收(dev + 打包 app)

**Files:** 无代码改动;发现 bug 就地修复并归入对应模块的 commit(`git commit --amend` 或新 fix commit)。

**Interfaces:**
- Consumes: Task 1–6 全部。
- Produces: spec 成功标准的实证记录(在最终汇报里逐条对照)。

- [ ] **Step 1: 自动化门槛**

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib && npm run typecheck
```
Expected: 两者全绿。

- [ ] **Step 2: 起 dev**

`npm run dev:log`(日志 tee 到 `/tmp/clitab-dev.log`,出 bug 时附上)。

- [ ] **Step 3: dev 验收清单(逐项勾选)**

a. 标签 B 里执行 `sleep 3; printf '\a'`,3 秒内切回标签 A → B 闪烁 + B 出现琥珀圆点 + Dock 角标 1;
b. ⌘J → 跳到 B(闪烁熄灭,**圆点仍在**);在 B 敲任意一键 → 圆点消失、角标 0;
c. 重新触发 B 等待(`printf '\a'`),⌘J 跳到 B 后**只切换不敲键**、再切回 A → B 圆点保留,再按 ⌘J 会跳回 B(看过≠处理完);在 B 敲键清账后,队列已空,按 ⌘J → **无任何动作**;
d. 仅当前标签在等待时按 ⌘J → 无动作(扫描不含 active);
e. 关闭一个带圆点的标签 → 角标随之减少;
f. dev 下 reload webview(⌘R)→ 圆点与角标状态不丢;
g. 双标签同时等待 → 角标 2,⌘J 按标签栏顺序轮转。

- [ ] **Step 4: 打包 app 验收(通知只能在打包态完整验证)**

```bash
npm run package:macos
open src-tauri/target/release/bundle/macos/clitab.app
```

a. 触发等待(BEL)时窗口在**前台** → 不弹通知(圆点、角标照常);
b. 切到别的全屏 app,让某标签触发等待 → 通知出现,标题 = 标签标题,正文 = "Waiting for input";
c. 点击通知 → clitab 聚焦并切到对应标签;
d. 同一标签连续两次 `printf '\a'`(未响应期间)→ 只有一条通知;
e. 通知滞留时关闭该标签,再点通知 → app 聚焦,无崩溃、无空白 UI;
f. dev 裸二进制下通知归属可能显示为 Finder——已知限制,不算 bug(spec"依赖与配置")。

- [ ] **Step 5: 若步骤中修复了代码**

重跑 Step 1,然后按修复所属模块单独 commit(消息前缀沿用该模块,如 `Attention:` / `Renderer:`)。

### Task 8: 文档(README ×2 + CLAUDE.md)

**Files:**
- Modify: `README.md`
- Modify: `README.zh-CN.md`
- Modify: `CLAUDE.md`

**Interfaces:**
- Consumes: 已验收的行为(Task 7)。
- Produces: 无代码接口。

- [ ] **Step 1: README.md**

Features 一节,"Attention flash" 条目之后加:

```markdown
- **Attention triage** — a session that needs input joins a waiting queue:
  the Dock badge counts them, `⌘J` jumps to the next one, and while clitab
  is in the background a macOS notification announces each; clicking a
  notification goes straight to that tab. A tab leaves the queue when you
  type in it — switching alone does not.
```

Shortcuts 表,`⌃Tab` 行之后加:

```markdown
| `⌘J` | Jump to the next tab waiting for input |
```

Shortcuts 表之后加引注:

```markdown
> Notifications appear only while clitab is in the background. If they never
> show up, check System Settings → Notifications → clitab.
```

- [ ] **Step 2: README.zh-CN.md(与英文逐句对应)**

Features 一节,"关注闪烁" 条目之后加:

```markdown
- **关注分诊** — 需要你输入的会话进入等待队列:Dock 角标计数,⌘J 跳到
  下一个;clitab 在后台时每次进入等待弹一条 macOS 通知,点击通知直达
  对应标签。在该标签敲键盘才离开队列——仅切换不清除。
```

快捷键表,`⌃Tab` 行之后加:

```markdown
| `⌘J` | 跳到下一个等待输入的标签 |
```

快捷键表之后加引注:

```markdown
> 通知仅在 clitab 位于后台时弹出。若从未出现,请检查 系统设置 → 通知 → clitab。
```

- [ ] **Step 3: CLAUDE.md**

架构一节,Rust backend 列表 `osc.rs` 条目之后加:

```markdown
- `attention.rs` — side effects of the waiting-for-input triage queue: `tab-waiting` events, the Dock badge (count of waiting tabs), and macOS notifications via `mac-notification-sys` (one thread blocked in `wait_for_click` per notification; a click emits `focus-tab`). The queue state itself is `Registry.waiting`: set at every `tab-flash` trigger, cleared **only** by `pty_input` — switching tabs does not clear it.
```

`menu.rs` 条目里 `(⌘T/⌘W/⌃Tab/⌘1–9)` 改为 `(⌘T/⌘W/⌃Tab/⌘J/⌘1–9)`。

Data flow 一节 Events 行改为:

```markdown
- **Events (Rust → renderer):** `pty-output`, `tab-title`, `tab-cwd`, `tab-flash`, `tab-waiting`, `focus-tab`, `prompt-ready`, `tab-exit`, `menu-shortcut`. `tab-exit` is also listened to inside Rust (`lib.rs`) so a shell that exits on its own is removed from the session map.
```

- [ ] **Step 4: Commit**

```bash
git add README.md README.zh-CN.md CLAUDE.md
git commit -m "docs: attention triage (⌘J, dock badge, notifications)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```
