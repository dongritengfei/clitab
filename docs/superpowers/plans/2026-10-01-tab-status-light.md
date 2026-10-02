# 标签持久状态灯 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 每个标签常驻一个状态点(运行中/等待输入/回合完成/已退出),状态机维护在 Rust 侧 Registry,webview 重载不丢,shell 退出后标签保留可回看。

**Architecture:** 方案 A——`TabRecord` 增加 `status` 字段;`pty/status.rs` 持有纯函数状态机,唯一入口 `transition()` 在 Registry 锁内完成"变更 + emit `tab-status`"(与 pty-output 在 stream 锁内发射是同一顺序保证模式)。reader 线程(OSC 信号)、attention watcher(2 秒静默/输出活动)、exit watcher(退出)分别驱动转换。`tab-exit` 事件整体移除;退出的 session 惰性留在 map 里(replay ring 可回看),`close_tab` 是唯一删除路径。

**Tech Stack:** Rust(Tauri 2 + portable-pty + serde),React 18 + TypeScript,xterm.js 不涉及。

**Spec:** `docs/superpowers/specs/2026-10-01-tab-status-light-design.md`

## Global Constraints

- 测试命令:Rust `cargo test --manifest-path src-tauri/Cargo.toml --lib`(单个测试加名字);前端只有 `npm run typecheck`(无测试运行器、无 linter)。
- 状态字符串在 Rust(serde)与 TS(联合类型)两侧必须逐字一致:`idle` / `running` / `waiting_input` / `turn_done` / `exited`。
- `tab-status` 事件必须在持有 Registry 锁时发射(变更序=事件序);代码注释写明,不得"优化"出锁。
- 闪烁行为逐字节不变:现有每一处 `tab-flash` 发射点(Bell、PromptReady、watcher flash-once、活跃 tab 不闪的前端过滤)全部保留。
- 事件全集(改后):`pty-output`、`tab-title`、`tab-cwd`、`tab-flash`、`prompt-ready`、`tab-status`、`menu-shortcut`。`tab-exit` 从 Rust 与前端全部移除。
- `TAB_GONE` 语义不变;`prompt-ready` 事件保留原行为。
- 常量:`TURN_IDLE = 2s`(现有),`TURN_DONE_SETTLE = 3s`(新增,session.rs)。
- 状态点配色跟随 App.css 现有 Catppuccin Mocha 主题(spec 中 Tailwind 色值为初稿,按 spec 授权微调):running `#89b4fa`、waiting_input `#fab387`、turn_done `#a6e3a1`、exited `#f38ba8`。
- `TabItem.tsx` 的 `CHROME_WIDTH` 必须加上状态点宽度(67 → 79),否则标题截断位置错误。
- `README.md` 与 `README.zh-CN.md` 必须同步更新;CLAUDE_HOOKS.md 的 hook 命令统一 `> /dev/tty` 形式。
- **前置条件(开工前必须满足):** 工作区现有的 finder-new-tab 未提交改动(README×2、lib.rs、manager.rs、useTabManager.ts)与本计划触碰同一批文件。开始 Task 1 前先与用户确认这批改动已单独提交,执行中所有 `git add` 只加本计划产出的文件,绝不 `git add -A`。

## Review Focus

spec 暗示但单测覆盖不到的五类失败,按最可能咬人排序;每条注明钉住它的测试/验证:

1. **reader 线程与 watcher 并发转换导致事件乱序**(前端最终状态错误)——由 `transition_status` 锁内回调结构消除;Task 3 的代码注释 + Task 9 手动验证 1–6 的连续状态翻转覆盖。
2. **webview 重载丢状态**——Task 5 的 serde 单测钉住线上格式(`status` 字段、snake_case 值);Task 9 手动验证 7 覆盖端到端恢复。
3. **Stop hook 后 ink 重绘把 turn_done 立刻翻回 running**(绿灯留不住)——Task 4 的 settle 窗口逻辑;Task 9 手动验证 4 钉住("3s 内重绘不翻蓝")。
4. **未配置任何 hook 的降级体验**(不得报错、不得出现假绿灯)——Task 1 的 `degraded_mode_without_hooks_still_reaches_waiting_input` 单测钉住状态机路径;Task 9 手动验证 2(无 Notification hook 时 ~2s 落琥珀)。
5. **已退出标签上的操作**(close_tab 必须成功且不弹确认框、输入/缩放静默失败、重载后能回看最后一屏)——Task 5 保留 session 于 map 的改动;Task 9 手动验证 7 钉住全部三个行为。

---

### Task 1: 状态机纯函数(`pty/status.rs`)

**Files:**
- Create: `src-tauri/src/pty/status.rs`
- Modify: `src-tauri/src/pty/mod.rs`(加一行 `pub mod status;`)
- Test: `src-tauri/src/pty/status.rs` 内 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: 无(纯逻辑,不依赖其他任务)。
- Produces: `pub enum TabStatus { Idle, Running, WaitingInput, TurnDone, Exited }`(`Serialize`,snake_case,`Copy + PartialEq + Eq + Debug + Clone`);`pub enum Signal { ProgramTitle, Bell, TurnDone, Idle2s, Output, ClaudeDone, Exit }`(`Copy + PartialEq + Eq + Debug`);`pub fn next(current: TabStatus, signal: Signal) -> Option<TabStatus>`(返回 `None` = 忽略该信号,含"目标态==当前态")。Task 3/4/5 依赖这些名字。

- [ ] **Step 1: 写失败测试(转换表全覆盖)**

创建 `src-tauri/src/pty/status.rs`,先只写测试模块(枚举和函数此时不存在,编译失败即"红"):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_title_starts_and_resumes_a_session() {
        assert_eq!(next(TabStatus::Idle, Signal::ProgramTitle), Some(TabStatus::Running));
        assert_eq!(next(TabStatus::WaitingInput, Signal::ProgramTitle), Some(TabStatus::Running));
        assert_eq!(next(TabStatus::TurnDone, Signal::ProgramTitle), Some(TabStatus::Running));
        // Already running: a mid-turn title rename must not re-emit.
        assert_eq!(next(TabStatus::Running, Signal::ProgramTitle), None);
    }

    #[test]
    fn bell_means_needs_input_but_only_inside_a_session() {
        assert_eq!(next(TabStatus::Running, Signal::Bell), Some(TabStatus::WaitingInput));
        assert_eq!(next(TabStatus::TurnDone, Signal::Bell), Some(TabStatus::WaitingInput));
        // A vim error bell at a plain shell prompt must not light the dot.
        assert_eq!(next(TabStatus::Idle, Signal::Bell), None);
        assert_eq!(next(TabStatus::WaitingInput, Signal::Bell), None);
    }

    #[test]
    fn stop_hook_reports_turn_completion() {
        assert_eq!(next(TabStatus::Running, Signal::TurnDone), Some(TabStatus::TurnDone));
        assert_eq!(next(TabStatus::WaitingInput, Signal::TurnDone), Some(TabStatus::TurnDone));
        assert_eq!(next(TabStatus::TurnDone, Signal::TurnDone), None);
        assert_eq!(next(TabStatus::Idle, Signal::TurnDone), None);
    }

    #[test]
    fn idle_heuristic_never_demotes_turn_done() {
        assert_eq!(next(TabStatus::Running, Signal::Idle2s), Some(TabStatus::WaitingInput));
        // The Stop hook already parked us in TurnDone; silence after a
        // completed turn is expected, not "waiting for input".
        assert_eq!(next(TabStatus::TurnDone, Signal::Idle2s), None);
        assert_eq!(next(TabStatus::WaitingInput, Signal::Idle2s), None);
        assert_eq!(next(TabStatus::Idle, Signal::Idle2s), None);
    }

    #[test]
    fn output_resumes_the_session() {
        assert_eq!(next(TabStatus::WaitingInput, Signal::Output), Some(TabStatus::Running));
        assert_eq!(next(TabStatus::TurnDone, Signal::Output), Some(TabStatus::Running));
        assert_eq!(next(TabStatus::Idle, Signal::Output), None);
        assert_eq!(next(TabStatus::Running, Signal::Output), None);
    }

    #[test]
    fn claude_exiting_returns_to_idle() {
        for from in [TabStatus::Running, TabStatus::WaitingInput, TabStatus::TurnDone] {
            assert_eq!(next(from, Signal::ClaudeDone), Some(TabStatus::Idle));
        }
        assert_eq!(next(TabStatus::Idle, Signal::ClaudeDone), None);
    }

    #[test]
    fn exit_is_terminal() {
        for from in [
            TabStatus::Idle,
            TabStatus::Running,
            TabStatus::WaitingInput,
            TabStatus::TurnDone,
        ] {
            assert_eq!(next(from, Signal::Exit), Some(TabStatus::Exited));
        }
        // Nothing leaves Exited; close_tab removes the record instead.
        for signal in [
            Signal::ProgramTitle,
            Signal::Bell,
            Signal::TurnDone,
            Signal::Idle2s,
            Signal::Output,
            Signal::ClaudeDone,
            Signal::Exit,
        ] {
            assert_eq!(next(TabStatus::Exited, signal), None);
        }
    }

    /// Review Focus #4: with neither hook configured the whole lifecycle must
    /// still work at today's flash-level precision, and turn_done (the green
    /// light that only the Stop hook can prove) must never appear.
    #[test]
    fn degraded_mode_without_hooks_still_reaches_waiting_input() {
        let mut s = TabStatus::Idle;
        s = next(s, Signal::ProgramTitle).unwrap();
        assert_eq!(s, TabStatus::Running);
        s = next(s, Signal::Idle2s).unwrap();
        assert_eq!(s, TabStatus::WaitingInput);
        s = next(s, Signal::Output).unwrap();
        assert_eq!(s, TabStatus::Running);
        s = next(s, Signal::ClaudeDone).unwrap();
        assert_eq!(s, TabStatus::Idle);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib status`
Expected: 编译失败,`cannot find type TabStatus` / `function next`(红)。

- [ ] **Step 3: 最小实现**

在 `status.rs` 测试模块**上方**写实现(文件头注释说明状态机职责与 settle 由调用方负责):

```rust
//! Per-tab activity status: the state machine behind the tab-bar status dot.
//!
//! `next()` is a pure function so the whole transition table is unit-testable
//! without threads or PTYs. Timing context that the table deliberately does
//! not know about (the turn_done settle window) is gated by the caller — the
//! attention watcher only sends `Signal::Output` once the window has passed.

use serde::Serialize;

/// Wire format is snake_case: `src/types.ts` mirrors these exact strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TabStatus {
    /// At a shell prompt; Claude is not running. The dot is hidden.
    Idle,
    /// A Claude session is in flight and producing output.
    Running,
    /// Claude needs the user (permission prompt, question, idle notification).
    WaitingInput,
    /// A turn completed (Stop hook); Claude is parked at the composer.
    TurnDone,
    /// The shell exited. Terminal state — `close_tab` removes the record.
    Exited,
}

/// Everything that can move the state machine. Produced by the OSC handler
/// (reader thread), the attention watcher, and the exit watcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// OSC 0/1/2 title classified as program-set (Claude started or renamed).
    ProgramTitle,
    /// Bare BEL — with the Notification hook, Claude's precise "need input".
    Bell,
    /// OSC 9 `claude-turn-done` — the Stop hook fired.
    TurnDone,
    /// program_active and no PTY output for TURN_IDLE.
    Idle2s,
    /// PTY output is flowing again (watcher poll saw idle < TURN_IDLE).
    Output,
    /// OSC 9 `claude-done` — the claude process exited to a shell prompt.
    ClaudeDone,
    /// The tab's child process is gone.
    Exit,
}

/// The full transition table. `None` means "ignore": the signal does not apply
/// in this state, or the transition would be a no-op (same state) — callers
/// rely on `None` to avoid emitting redundant `tab-status` events.
pub fn next(current: TabStatus, signal: Signal) -> Option<TabStatus> {
    use TabStatus as S;
    let next = match (current, signal) {
        (_, Signal::Exit) if current != S::Exited => S::Exited,
        (S::Idle, Signal::ProgramTitle) => S::Running,
        (S::WaitingInput, Signal::ProgramTitle) => S::Running,
        (S::TurnDone, Signal::ProgramTitle) => S::Running,
        (S::Running, Signal::Bell) => S::WaitingInput,
        (S::TurnDone, Signal::Bell) => S::WaitingInput,
        (S::Running, Signal::TurnDone) => S::TurnDone,
        (S::WaitingInput, Signal::TurnDone) => S::TurnDone,
        // Only ever from Running: the 2s silence heuristic must not demote a
        // Stop-hook-proven TurnDone into WaitingInput.
        (S::Running, Signal::Idle2s) => S::WaitingInput,
        (S::WaitingInput, Signal::Output) => S::Running,
        (S::TurnDone, Signal::Output) => S::Running,
        (S::Running, Signal::ClaudeDone) => S::Idle,
        (S::WaitingInput, Signal::ClaudeDone) => S::Idle,
        (S::TurnDone, Signal::ClaudeDone) => S::Idle,
        _ => return None,
    };
    if next == current {
        None
    } else {
        Some(next)
    }
}
```

并在 `src-tauri/src/pty/mod.rs` 的模块声明区(按字母序,`pub mod shell_integration;` 之后)加:

```rust
pub mod status;
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib status`
Expected: PASS,8 个测试全绿;随后跑全量 `cargo test --manifest-path src-tauri/Cargo.toml --lib` 确认无回归。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/pty/status.rs src-tauri/src/pty/mod.rs
git commit -m "feat: per-tab status state machine (pure transition table)"
```

---

### Task 2: OSC 解析 `claude-turn-done`(`osc.rs`)

**Files:**
- Modify: `src-tauri/src/osc.rs`(`OscEvent` 枚举、`interpret()`、tests)

**Interfaces:**
- Consumes: 无。
- Produces: `OscEvent::TurnDone` 变体。Task 4 的 `handle_osc` 依赖它。

- [ ] **Step 1: 写失败测试**

在 `osc.rs` 的 tests 模块中,`osc9_claude_done` 测试之后追加:

```rust
#[test]
fn osc9_claude_turn_done() {
    let mut parser = OscParser::new();
    // The Stop hook writes ST-terminated OSC 9 straight to the tty.
    let events = parser.parse(b"\x1b]9;claude-turn-done\x1b\\");
    assert_eq!(events, vec![OscEvent::TurnDone]);
    // ...and the existing claude-done must not be confused with it.
    let events = parser.parse(b"\x1b]9;claude-done\x1b\\");
    assert_eq!(events, vec![OscEvent::PromptReady]);
}

#[test]
fn turn_done_split_across_reads() {
    let mut parser = OscParser::new();
    assert!(parser.parse(b"\x1b]9;claude-tur").is_empty());
    let events = parser.parse(b"n-done\x1b\\");
    assert_eq!(events, vec![OscEvent::TurnDone]);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib turn_done`
Expected: 编译失败,`no variant or associated item named 'TurnDone'`(红)。

- [ ] **Step 3: 最小实现**

`OscEvent` 枚举(第 19-25 行)加一个变体:

```rust
pub enum OscEvent {
    TitleChanged(String),
    CwdChanged(String),
    Bell,
    PromptReady,
    /// OSC 9 `claude-turn-done`: one assistant turn ended (Stop hook).
    TurnDone,
}
```

`interpret()` 的 match(第 150-155 行)加一个分支,放在现有 `"9"` 分支旁边:

```rust
"9" if value == "claude-done" => Some(OscEvent::PromptReady),
"9" if value == "claude-turn-done" => Some(OscEvent::TurnDone),
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib osc`
Expected: PASS(新旧 OSC 测试全绿)。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/osc.rs
git commit -m "feat: parse OSC 9 claude-turn-done into OscEvent::TurnDone"
```

---

### Task 3: Registry 持有状态 + 锁内转换入口(`registry.rs` / `status.rs`)

**Files:**
- Modify: `src-tauri/src/pty/registry.rs`(`TabRecord`、`insert()`、新方法 `transition_status`、tests)
- Modify: `src-tauri/src/pty/status.rs`(追加 `transition()` 发射包装)
- Test: `registry.rs` 内 tests 模块

**Interfaces:**
- Consumes: Task 1 的 `TabStatus` / `Signal` / `next()`。
- Produces:
  - `TabRecord` 新字段 `pub status: TabStatus`、`pub status_at: std::time::Instant`(均随 `TabRecord` clone;`status_at` 不进任何序列化——TabRecord 本身不序列化,Task 5 的 TabResponse 才序列化,且只带 `status`)。
  - `Registry::transition_status(&self, id: &str, signal: Signal, on_change: impl FnOnce(TabStatus)) -> bool`——锁内计算 `next()`、更新 `status`/`status_at`、状态变化时调用回调并返回 true;忽略/no-op 返回 false 且**不碰 `status_at`、不调回调**。
  - `status::transition(registry: &Registry, app: &AppHandle, tab_id: &str, signal: Signal)`——Task 4 所有调用点用的唯一入口。

- [ ] **Step 1: 写失败测试**

在 `registry.rs` 的 tests 模块追加(现有测试不动):

```rust
use crate::pty::status::{Signal, TabStatus};

#[test]
fn insert_starts_idle() {
    let registry = Registry::new();
    let record = registry.insert("t1".into(), "/tmp".into());
    assert_eq!(record.status, TabStatus::Idle);
}

#[test]
fn transition_changes_status_and_reports_under_the_callback() {
    let registry = Registry::new();
    registry.insert("t1".into(), "/tmp".into());
    let mut seen = Vec::new();
    let changed = registry.transition_status("t1", Signal::ProgramTitle, |s| seen.push(s));
    assert!(changed);
    assert_eq!(seen, vec![TabStatus::Running]);
    assert_eq!(registry.get("t1").unwrap().status, TabStatus::Running);
}

#[test]
fn ignored_signal_is_a_silent_noop() {
    let registry = Registry::new();
    registry.insert("t1".into(), "/tmp".into());
    let before = registry.get("t1").unwrap().status_at;
    let mut called = false;
    // Bell at idle is ignored by the state machine (vim bells must not light
    // the dot), so nothing may be touched.
    let changed = registry.transition_status("t1", Signal::Bell, |_| called = true);
    assert!(!changed);
    assert!(!called);
    assert_eq!(registry.get("t1").unwrap().status_at, before);
}

#[test]
fn status_at_refreshes_on_change() {
    let registry = Registry::new();
    registry.insert("t1".into(), "/tmp".into());
    let before = registry.get("t1").unwrap().status_at;
    std::thread::sleep(std::time::Duration::from_millis(5));
    registry.transition_status("t1", Signal::ProgramTitle, |_| {});
    assert!(registry.get("t1").unwrap().status_at > before);
}

#[test]
fn unknown_tab_transition_is_ignored() {
    let registry = Registry::new();
    assert!(!registry.transition_status("nope", Signal::Exit, |_| {}));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib registry`
Expected: 编译失败,`TabRecord` 无 `status` 字段 / 无 `transition_status` 方法(红)。

- [ ] **Step 3: 实现 Registry 侧**

`registry.rs` 顶部 use 区加:

```rust
use super::status::{self, Signal, TabStatus};
use std::time::Instant;
```

`TabRecord` 加字段(`has_program_title` 之后):

```rust
    /// Where the tab is in the Claude-session lifecycle; drives the tab-bar
    /// status dot. Transitioned only via `transition_status`.
    pub status: TabStatus,
    /// When `status` last changed. The attention watcher reads it to apply the
    /// turn_done settle window. Never serialized.
    pub status_at: Instant,
```

`insert()` 的构造体加两个初始值:

```rust
        let record = TabRecord {
            id: id.clone(),
            title: cwd.clone(),
            cwd,
            has_program_title: false,
            status: TabStatus::Idle,
            status_at: Instant::now(),
        };
```

新增方法(放在 `clear_program_title` 与 `list` 之间):

```rust
    /// Apply one state-machine signal to a tab.
    ///
    /// `on_change` runs **while the registry lock is held**, so the caller's
    /// `tab-status` emit is ordered exactly like the state change even when
    /// the reader thread and the attention watcher race — the same deliberate
    /// pattern as emitting `pty-output` under the stream lock in session.rs.
    /// Do not "optimize" the callback out of the lock.
    ///
    /// Returns false (touching nothing, `status_at` included) when the signal
    /// is ignored or would be a no-op.
    pub fn transition_status(
        &self,
        id: &str,
        signal: Signal,
        on_change: impl FnOnce(TabStatus),
    ) -> bool {
        let mut tabs = lock(&self.tabs);
        let Some(tab) = tabs.iter_mut().find(|t| t.id == id) else {
            return false;
        };
        let Some(next) = status::next(tab.status, signal) else {
            return false;
        };
        tab.status = next;
        tab.status_at = Instant::now();
        on_change(next);
        true
    }
```

- [ ] **Step 4: 实现 `status::transition`(锁内发射的唯一入口)**

`status.rs` 实现区(`next()` 之后)追加:

```rust
use super::registry::Registry;
use tauri::{AppHandle, Emitter};

/// The only way to move a tab's status: compute + mutate + emit happen inside
/// one registry lock hold (see `Registry::transition_status`), so the renderer
/// never sees `tab-status` events out of order.
pub fn transition(registry: &Registry, app: &AppHandle, tab_id: &str, signal: Signal) {
    registry.transition_status(tab_id, signal, |status| {
        let _ = app.emit(
            "tab-status",
            serde_json::json!({ "tab_id": tab_id, "status": status }),
        );
    });
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib registry`
Expected: PASS;再跑全量 `cargo test --manifest-path src-tauri/Cargo.toml --lib` 确认无回归。

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/pty/registry.rs src-tauri/src/pty/status.rs
git commit -m "feat: registry stores per-tab status; transitions emit tab-status under the lock"
```

---

### Task 4: session.rs 接线(OSC 处理、attention watcher、exit watcher)

**Files:**
- Modify: `src-tauri/src/pty/session.rs`(常量区、`new()` 的三个线程、`handle_osc`)

**Interfaces:**
- Consumes: Task 1/3 的 `status::transition`、`Signal`、`TabStatus`;Task 2 的 `OscEvent::TurnDone`;现有 `Registry::get`(读 `status_at` 做 settle 判断)。
- Produces: 运行时行为——`tab-status` 事件开始随真实信号发射;`tab-exit` 事件**停止发射**(Task 5 删它的消费者)。exit watcher 不再让 session 从 map 移除(Task 5 删 `remove_session`)。

- [ ] **Step 1: 加常量与 import**

常量区(`TURN_IDLE` 之后)加:

```rust
/// After a Stop-hook turn completion, ignore output for this long before
/// flipping back to Running: ink repaints the composer right after the turn
/// ends, and those redraws are not a new turn.
const TURN_DONE_SETTLE: Duration = Duration::from_secs(3);
```

顶部 use 区加:

```rust
use super::status::{self, Signal, TabStatus};
```

- [ ] **Step 2: `handle_osc` 接入状态信号**

逐分支修改(现有 flash/prompt-ready/标题逻辑**全部保留**,只叠加 transition 调用):

`TitleChanged` 分支,在 `program_active.store(...)` 之后、`registry.set_title(...)` 之前插入:

```rust
                if is_program_title {
                    status::transition(registry, app, tab_id, Signal::ProgramTitle);
                }
```

`Bell` 分支改为(闪烁照旧):

```rust
            OscEvent::Bell => {
                // With the Notification hook this is Claude's precise "I need
                // input"; a bare bell at a shell prompt only flashes (the
                // state machine ignores Bell while Idle).
                status::transition(registry, app, tab_id, Signal::Bell);
                let _ = app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
            }
```

新增 `TurnDone` 分支(放在 `Bell` 与 `PromptReady` 之间):

```rust
            OscEvent::TurnDone => {
                status::transition(registry, app, tab_id, Signal::TurnDone);
            }
```

`PromptReady` 分支,在 `program_active.store(false, ...)` 之后插入一行(其余不动):

```rust
                status::transition(registry, app, tab_id, Signal::ClaudeDone);
```

- [ ] **Step 3: `new()` 里为 watcher 线程克隆 registry**

reader 线程的 spawn 块把 `registry` move 走了。在该块**之前**加两行克隆,供后两个 watcher 使用:

```rust
        let registry_for_attention = Arc::clone(&registry);
        let registry_for_exit = Arc::clone(&registry);
```

- [ ] **Step 4: attention watcher 升级为状态机 ticker**

整个 attention watcher spawn 块替换为(flash-once 逻辑原样保留,只是叠加了转换;`Idle2s`/`Output` 仅在 `program_active` 时评估——沿用现有 `continue` 分支):

```rust
        // Attention watcher: an assistant turn that stops producing output for
        // a while is waiting for the user, so flash the tab (once per turn).
        // The same poll also drives the status state machine's time-based
        // signals (Idle2s / Output); the reader thread only timestamps
        // last_activity, so no per-chunk registry locking happens.
        {
            let tab_id = tab_id.clone();
            let app = app.clone();
            let running = Arc::clone(&running);
            let last_activity = Arc::clone(&last_activity);
            let program_active = Arc::clone(&program_active);
            let registry = registry_for_attention;
            thread::spawn(move || {
                let mut flashed = false;
                while running.load(Ordering::Relaxed) {
                    thread::sleep(WATCHER_POLL);
                    if !program_active.load(Ordering::Relaxed) {
                        flashed = false;
                        continue;
                    }
                    let idle = lock(&last_activity).elapsed();
                    if idle >= TURN_IDLE {
                        // No-op unless currently Running: a Stop-hook TurnDone
                        // must not be demoted by post-turn silence.
                        status::transition(&registry, &app, &tab_id, Signal::Idle2s);
                        if !flashed {
                            flashed = true;
                            let _ =
                                app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
                        }
                    } else {
                        // Output is flowing. TurnDone only yields to it after
                        // the settle window, so the composer repaints right
                        // after a Stop hook do not flip the green dot back.
                        let settled = registry
                            .get(&tab_id)
                            .map(|t| {
                                t.status != TabStatus::TurnDone
                                    || t.status_at.elapsed() > TURN_DONE_SETTLE
                            })
                            .unwrap_or(false);
                        if settled {
                            status::transition(&registry, &app, &tab_id, Signal::Output);
                        }
                    }
                }
            });
        }
```

- [ ] **Step 5: exit watcher 置 Exited,移除 `tab-exit` 事件**

整个 exit watcher spawn 块替换为(仍然必须 `child.wait()` 收尸;`code` 与事件发射删除):

```rust
        // Exit watcher: mark the tab exited and stop every other thread. The
        // session itself stays in the manager's map (inert) so its replay ring
        // survives — an exited tab can still show its last screen after a
        // webview reload. close_tab is the only path that removes it.
        {
            let app = app.clone();
            let running = Arc::clone(&running);
            let program_active = Arc::clone(&program_active);
            let registry = registry_for_exit;
            thread::spawn(move || {
                // Even when wait() itself fails the child is gone as far as we
                // are concerned; skipping the transition would strand a dead
                // tab showing a live status forever.
                let _ = child.wait();
                program_active.store(false, Ordering::Relaxed);
                running.store(false, Ordering::SeqCst);
                status::transition(&registry, &app, &tab_id, Signal::Exit);
            });
        }
```

注意:此块直接 move 外层的 `tab_id`(原代码即如此,reader/attention 块用的都是 `tab_id.clone()`,exit 块拿最后的所有权——若编译器因块顺序报 borrow 错误,在本块开头加 `let tab_id = tab_id.clone();` 即可,与原代码行为一致)。

- [ ] **Step 6: 编译 + 全量测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: PASS。此时 lib.rs 的 `tab-exit` 监听成为死代码(事件不再发射),`remove_session` 不再被调用——Task 5 清理;本步允许存在 `unused` 警告但不允许 error。

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/pty/session.rs
git commit -m "feat: drive tab status from OSC handler, attention watcher and exit watcher"
```

---

### Task 5: 退出保留 + 协议收口(`manager.rs` / `lib.rs`)

**Files:**
- Modify: `src-tauri/src/pty/manager.rs`(删 `remove_session`)
- Modify: `src-tauri/src/lib.rs`(删 `tab-exit` 监听与 `TabExitPayload`、`TabResponse` 加 `status`、tests)
- Test: `lib.rs` 内 tests 模块

**Interfaces:**
- Consumes: Task 1 的 `TabStatus`(序列化)。
- Produces: `TabResponse` 新字段 `pub status: TabStatus`(camelCase 结构体中字段名即 `status`,值为 snake_case 字符串)——Task 6 的 TS 类型以此为准。`create_tab` / `list_tabs` 自动携带。

- [ ] **Step 1: 写失败测试(钉住线上格式,Review Focus #2)**

`lib.rs` tests 模块追加:

```rust
    /// The renderer's TabStatus union mirrors these exact strings; a serde
    /// rename here would silently desync the wire format.
    #[test]
    fn tab_response_serializes_status_as_snake_case() {
        let json = serde_json::to_value(TabResponse {
            id: "t1".into(),
            title: "x".into(),
            cwd: "/x".into(),
            has_claude_title: false,
            status: pty::status::TabStatus::WaitingInput,
        })
        .unwrap();
        assert_eq!(json["status"], "waiting_input");
        assert_eq!(json["hasClaudeTitle"], false);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib tab_response`
Expected: 编译失败,`TabResponse` 无 `status` 字段(红)。

- [ ] **Step 3: 实现**

`lib.rs`:

1. `TabResponse` 加字段 + `From` 补齐:

```rust
pub struct TabResponse {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub has_claude_title: bool,
    pub status: pty::status::TabStatus,
}

impl From<TabRecord> for TabResponse {
    fn from(tab: TabRecord) -> Self {
        Self {
            id: tab.id,
            title: tab.title,
            cwd: tab.cwd,
            has_claude_title: tab.has_program_title,
            status: tab.status,
        }
    }
}
```

2. 删除 `setup` 中整个 `tab-exit` 监听块(`let manager = tab_manager.clone(); app.listen("tab-exit", ...)`),以及文件底部的 `TabExitPayload` 结构体。
3. use 区:删 `Listener`(`use tauri::{AppHandle...}` 处仅保留仍在用的项;`Manager`/`State`/`Emitter` 按实际使用保留),删 `serde::Deserialize`(若无其他使用者)。

`manager.rs`:删除整个 `remove_session` 方法(其唯一调用方是刚删掉的 lib.rs 监听;退出的 session 由 Task 4 起惰性留在 map,等 `close_tab` 收走)。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: PASS,全量无回归、无新 warning(`cargo build --manifest-path src-tauri/Cargo.toml 2>&1 | grep warning` 应为空或仅既有的)。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/lib.rs src-tauri/src/pty/manager.rs
git commit -m "feat: exited tabs stay until closed; tab-status replaces tab-exit on the wire"
```

---

### Task 6: 前端类型与事件接线(`types.ts` / `useTabManager.ts`)

**Files:**
- Modify: `src/types.ts`
- Modify: `src/hooks/useTabManager.ts`

**Interfaces:**
- Consumes: Task 5 的 `TabResponse.status`(snake_case 字符串)与 `tab-status` 事件 payload `{ tab_id, status }`。
- Produces: `TabStatus` 联合类型、`Tab.status`、`TabStatusPayload`;`tab-exit` 监听移除(不再 abandonTab)。Task 7 依赖 `tab.status`。

- [ ] **Step 1: `types.ts` 改动**

1. 文件顶部(`Tab` 之前)加:

```ts
/** Per-tab activity state; mirrors `TabStatus` in `src-tauri/src/pty/status.rs`. */
export type TabStatus = 'idle' | 'running' | 'waiting_input' | 'turn_done' | 'exited';
```

2. `Tab` 接口:`hasClaudeTitle` 之后加 `status: TabStatus;`(带一行注释 `/** Backend-owned; drives the status dot. */`)。
3. `TabResponse` 接口:加 `status: TabStatus;`。
4. 新增 payload(放在 `TabFlashPayload` 附近):

```ts
export interface TabStatusPayload {
  tab_id: string;
  status: TabStatus;
}
```

5. 删除 `TabExitPayload` 接口(后端已不再发射该事件)。

- [ ] **Step 2: `useTabManager.ts` 改动**

1. import 列表:删 `type TabExitPayload`,加 `type TabStatusPayload`。
2. `toTab` **不用改**(`{ ...response, flashing: false }` 的展开自动携带 `status`)。
3. 删除整个 `listen<TabExitPayload>('tab-exit', ...)` 监听块(含"shell is gone"注释)。`abandonTab` 保留——closeTab 的 TAB_GONE 路径仍用它。
4. 在原 `tab-exit` 监听的位置换成:

```ts
      listen<TabStatusPayload>('tab-status', ({ payload }) => {
        setTabs((prev) =>
          prev.map((tab) =>
            tab.id === payload.tab_id ? { ...tab, status: payload.status } : tab
          )
        );
      }),
```

注意:`switchTab` 只清 `flashing`,不碰 `status`(闪烁与状态点共存,状态由后端独占)。

- [ ] **Step 3: 类型检查**

Run: `npm run typecheck`
Expected: 0 errors。(若有 `TabExitPayload` 残留引用报错,说明 Step 2.3 删漏。)

- [ ] **Step 4: Commit**

```bash
git add src/types.ts src/hooks/useTabManager.ts
git commit -m "feat: renderer tracks backend-owned tab status; drop tab-exit handling"
```

---

### Task 7: 状态点渲染(`TabItem.tsx` / `App.css`)

**Files:**
- Modify: `src/components/TabItem.tsx`
- Modify: `src/App.css`

**Interfaces:**
- Consumes: Task 6 的 `Tab.status` / `TabStatus`。
- Produces: 纯 UI,无下游。

- [ ] **Step 1: `TabItem.tsx` 改动**

1. import 行改为 `import { Tab, TabStatus } from '../types';`
2. `CHROME_WIDTH` 常量改为 79,注释同步:

```ts
/** Space reserved for the non-text parts of a row: 3px border-left +
 *  16px/4px padding + 16px badge + 8px status dot + 3×4px gaps +
 *  20px close button. */
const CHROME_WIDTH = 79;
```

3. 组件外(常量区)加 tooltip 文案表:

```ts
/** Tooltip per status; idle renders no dot and needs no label. */
const STATUS_LABEL: Record<TabStatus, string> = {
  idle: '',
  running: 'Claude is working',
  waiting_input: 'Claude needs input',
  turn_done: 'Turn complete',
  exited: 'Session exited',
};
```

4. 根 div 的 className 追加 exited 类:

```tsx
className={`tab-item ${isActive ? 'active' : ''} ${tab.flashing ? 'flashing' : ''} ${
  tab.status === 'exited' ? 'exited' : ''
}`}
```

5. JSX:在 `tab-index` span 之后、`tab-text` div 之前插入(与 badge 相同的 a11y 处理——装饰性、`aria-hidden`,语义走 `title`):

```tsx
      {/* Persistent activity dot. Always rendered (transparent when idle) so
          the title's available width never jumps when a status appears. */}
      <span
        className="tab-status"
        data-status={tab.status}
        aria-hidden="true"
        title={STATUS_LABEL[tab.status] || undefined}
      />
```

- [ ] **Step 2: `App.css` 改动**

在 `.tab-index` 规则块之后(`.tab-text` 之前)插入:

```css
/* Persistent per-tab activity dot: blue pulsing = Claude working, amber
   pulsing = needs input, green = turn complete, red = shell exited.
   Catppuccin Mocha accents, matching the rest of the tab strip. */
.tab-status {
  flex-shrink: 0;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: transparent;
}

.tab-status[data-status='running'] {
  background: #89b4fa;
  animation: status-pulse 1.2s ease-in-out infinite;
}

.tab-status[data-status='waiting_input'] {
  background: #fab387;
  animation: status-pulse 1.2s ease-in-out infinite;
}

.tab-status[data-status='turn_done'] {
  background: #a6e3a1;
}

.tab-status[data-status='exited'] {
  background: #f38ba8;
}

@keyframes status-pulse {
  0%, 100% {
    opacity: 1;
  }
  50% {
    opacity: 0.35;
  }
}

/* An exited tab stays for scrollback but must read as dead at a glance. */
.tab-item.exited {
  opacity: 0.55;
}
```

- [ ] **Step 3: 类型检查 + 构建**

Run: `npm run typecheck && npm run build`
Expected: 均通过(vite build 顺带验证 CSS 无语法错误)。

- [ ] **Step 4: Commit**

```bash
git add src/components/TabItem.tsx src/App.css
git commit -m "feat: render the persistent tab status dot"
```

---

### Task 8: 文档(CLAUDE_HOOKS.md / README×2 / CLAUDE.md)

**Files:**
- Modify: `CLAUDE_HOOKS.md`
- Modify: `README.md`、`README.zh-CN.md`(两份必须同步)
- Modify: `CLAUDE.md`

**Interfaces:**
- Consumes: 前序任务落地的最终行为(事件名、hook OSC payload、降级语义)。
- Produces: 无代码接口。

- [ ] **Step 1: CLAUDE_HOOKS.md**

1. Setup 一节的 Notification hook 命令改为 `printf '\\a' > /dev/tty`,并在 "How it works" 追加一条:hook 进程的 stdout 可能被 Claude Code 捕获,`> /dev/tty` 直写控制终端才可靠。
2. 新增 "Turn Completion (Stop hook)" 一节(放在 "How it works" 之后),内容:配置以下 hook 后,每个助手回合结束时 Claude Code 向终端写 `OSC 9 claude-turn-done`,clitab 据此把标签状态点点成绿色(回合完成);不配置则回合结束由 2 秒静默启发式近似为"等待输入"(琥珀),功能不报错、只是精度降级:

```json
{
  "hooks": {
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "printf '\\e]9;claude-turn-done\\e\\\\' > /dev/tty"
          }
        ]
      }
    ]
  }
}
```

3. 删除 "Alternative: Using OSC 9" 一节(其示例让 Notification hook 发 `claude-done`,在新协议下语义错误——`claude-done` 专属 shell integration 的"claude 进程已退出")。
4. 删除文末 "Note"(`Stop 事件可能不支持 command hooks` 的过时注记,与新增章节矛盾)。
5. 新增简短 "Status lights" 一节,列出四种状态点含义(蓝=运行中、琥珀=等待输入、绿=回合完成、红/置灰=已退出,已退出标签保留到手动关闭)。

- [ ] **Step 2: README.md / README.zh-CN.md**

两份 README 的 Features 一节,在 "Attention flashing / 关注闪烁" 条目之后各加一条(内容对应、语言各自):

英文:

```markdown
- **Persistent status lights** — every tab carries a status dot: blue (pulsing)
  while Claude is working, amber when it needs your input (permission prompts,
  questions), green when a turn completes, red on a dimmed tab once the shell
  exits. Exited tabs stay put — last screen and scrollback included — until you
  close them. Full precision needs the one-time hook setup in `CLAUDE_HOOKS.md`;
  without hooks the dot degrades to the same 2s-silence heuristic as flashing.
```

中文:

```markdown
- **持久状态灯** — 每个标签常驻一个状态点:Claude 运行时蓝色脉动、等待输入
  (权限确认、提问)时琥珀色、回合完成绿色、shell 退出后标签置灰红点。
  已退出的标签保留最后一屏与滚动缓冲,直到手动关闭。完整精度需要
  `CLAUDE_HOOKS.md` 中的一次性 hook 配置;未配置时状态点退化为与闪烁相同的
  2 秒静默启发式。
```

(以两份 README 中该小节现有条目的实际措辞/缩进风格为准做等价融入,不得只改一份。)

- [ ] **Step 3: CLAUDE.md**

1. Architecture → Rust backend → `lib.rs` 条目:事件相关描述中删去 `tab-exit` 句子。
2. `pty/session.rs` 条目:补一句——三个线程分别驱动状态机信号(reader:OSC;attention watcher:Idle2s/Output 含 turn_done settle;exit watcher:Exit),退出的 session 惰性留在 map 供回看。
3. `pty/registry.rs` 条目:补充 Registry 同时持有 per-tab `status`(状态机在 `pty/status.rs`)。
4. Data flow / invariants → Events 行改为:`pty-output`, `tab-title`, `tab-cwd`, `tab-flash`, `prompt-ready`, `tab-status`, `menu-shortcut`(删 `tab-exit` 及其 Rust 侧监听说明),并新增一条 invariant:**`tab-status` 在 Registry 锁内发射**(与 pty-output 的 stream 锁同款顺序保证,改动 attach/status 代码时保持)。

- [ ] **Step 4: 一致性检查 + Commit**

检查:两份 README 新条目语义一致;CLAUDE_HOOKS.md 中 OSC payload 逐字为 `claude-turn-done`;CLAUDE.md 事件列表与 `git grep -n "tab-exit" src src-tauri` 的实际结果一致(应无命中)。

```bash
git add CLAUDE_HOOKS.md README.md README.zh-CN.md CLAUDE.md
git commit -m "docs: status lights — Stop hook setup, READMEs, architecture notes"
```

---

### Task 9: 端到端手动验证

**Files:**
- 无新改动(发现问题回对应 Task 修复并补测试)。

**Interfaces:**
- Consumes: 全部前序任务。
- Produces: 验证记录(勾选 spec 的手动清单)。

- [ ] **Step 1: 配置 hooks**

按更新后的 `CLAUDE_HOOKS.md` 在 `~/.claude/settings.json` 配好 Notification(`printf '\a' > /dev/tty`)与 Stop(`printf '\e]9;claude-turn-done\e\\' > /dev/tty`)两个 hook。

- [ ] **Step 2: 起 dev 跑清单**

Run: `npm run dev:log`(日志落 /tmp/clitab-dev.log)

逐项验证(对应 spec 手动清单,即 Review Focus 的 #1/#3/#5 钉点):

1. 新建 tab 启动 `claude` → 蓝点脉动;
2. 触发权限确认(让它跑一个需要批准的工具)→ 琥珀;临时移除 Notification hook 重试 → ~2s 后琥珀(降级路径);
3. 允许权限 → 回蓝;
4. 等一个回合结束 → 绿;结束后 3s 内观察不因重绘翻蓝(settle);
5. 发下一条消息 → 蓝;
6. 在 claude 里 `/exit` → 灯灭(idle)、标题回落为目录;
7. shell 里 `exit` → 标签置灰红点;画面保留、可滚动回看;⌘R 重载 webview → 状态点与最后一屏均恢复;⌘W 关闭 → 不弹确认框、标签消失;
8. 全程:非活跃 tab 的 BEL/回合结束仍闪烁、活跃 tab 不闪、切 tab 清闪烁——与改动前一致;
9. 普通 shell 里跑 `vim` 并触发响铃(如 Normal 态乱按)→ 标签闪烁但**不**出现状态点;
10. 开 3 个 tab 并行跑 claude,快速切换 → 各 tab 状态互不串台(事件按 tab_id 路由)。

- [ ] **Step 3: 收尾**

全绿后:`cargo test --manifest-path src-tauri/Cargo.toml --lib && npm run typecheck` 最后跑一遍;确认 `git status` 无本计划之外的意外改动。
