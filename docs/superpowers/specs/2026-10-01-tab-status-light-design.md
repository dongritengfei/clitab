# 标签持久状态灯(Tab Status Light)设计

日期:2026-10-01
状态:已与用户对齐(Stop hook、退出标签保留、与闪烁共存、仅 Claude 会话、方案 A)

## 背景与目标

现有的关注闪烁(tab-flash)是瞬时的:闪过了没看见就丢了。本设计给每个标签
加一个**常驻状态点**,把已经存在于后端的信号(程序标题、BEL、OSC 9、2 秒
静默启发式、子进程退出)收敛为一个 per-tab 状态机,扫一眼标签栏即可区分:
哪个 tab 在跑、哪个卡住等我、哪个跑完了、哪个死了。

成功标准:

1. 状态点按下方状态机实时变化,四种可见状态(运行中/等待输入/回合完成/已退出)可区分;
2. webview 重载后状态不丢(`list_tabs` 携带 status);
3. shell 退出后标签保留、画面可回看,直到用户手动关闭;
4. 关注闪烁行为与现在**完全一致**(共存,不被状态点取代);
5. 未配置任何 Claude hook 的用户得到降级体验(≈今天的闪烁精度),不报错、不误导。

前置认知(核对代码后的事实,与"原料已全部具备"的原始设想不同):

- OSC 9 `claude-done` 由 shell integration 在 **claude 进程退出、回到 shell
  prompt** 时发出,交互式会话内部每回合结束没有任何信号 → 需新增 Stop hook;
- `tab-exit` 目前导致前端立即移除标签、后端删除 Registry 记录 → "已退出"
  状态要可见,必须改为保留;
- 精确的"需要输入"信号只有 Notification hook 的 BEL;2 秒静默是启发式近似。

## 已确认的决策

| 问题 | 决策 |
|---|---|
| 回合完成检测 | 扩展 CLAUDE_HOOKS.md,新增 Stop hook 发专用 OSC `claude-turn-done` |
| 已退出标签 | 保留直到手动 ⌘W(类 iTerm "close on exit: never"),replay ring 可回看 |
| 与闪烁关系 | 共存:闪烁=瞬时注意力(覆盖任意 BEL),状态点=常驻状态 |
| 覆盖面 | 仅 Claude 会话;普通 shell 命令不改变状态点 |
| 架构 | 方案 A:Registry 持有状态,session 各线程直写,新增 `tab-status` 事件 |

## 状态机

新文件 `src-tauri/src/pty/status.rs`:`TabStatus` 枚举 + 纯函数转换,全表可单测。

状态(serde 序列化为小写下划线字符串,与 TS 联合类型一一对应):

| 状态 | 含义 | 状态点 |
|---|---|---|
| `idle` | shell prompt,Claude 未运行(**初始态**) | 不显示(透明占位) |
| `running` | Claude 会话进行中、有输出流动 | 蓝色,脉动 |
| `waiting_input` | 等权限确认/提问/输入 | 琥珀色,脉动 |
| `turn_done` | 一回合结束(Stop hook),停在 composer | 绿色,常亮 |
| `exited` | shell 已退出(**终态**) | 红色,标签整体置灰 |

信号与转换表(未列出的 状态×信号 组合一律忽略):

| 信号 | 来源 | 转换 |
|---|---|---|
| `ProgramTitle` | OSC 0/1/2 程序标题(claude 启动/回合开始时改名) | idle / waiting_input / turn_done → running |
| `Bell` | BEL(Notification hook) | running / turn_done → waiting_input;**idle 态忽略状态**(vim 等响铃只闪不点灯) |
| `TurnDone` | OSC 9 `claude-turn-done`(Stop hook,新增) | running / waiting_input → turn_done |
| `Idle2s` | watcher:program_active 且 ≥2s 无输出 | **仅** running → waiting_input |
| `Output` | watcher:idle <2s | waiting_input → running;turn_done → running 仅当进入 turn_done 已超过 settle 窗口(3s) |
| `ClaudeDone` | OSC 9 `claude-done`(claude 进程退出) | running / waiting_input / turn_done → idle |
| `Exit` | 子进程退出(exit watcher) | 任意非 exited 态 → exited |

设计要点:

- **`Idle2s` 只从 running 出发**:Stop hook 用户回合结束落在 turn_done 后,
  2 秒静默不会把绿灯降级成琥珀灯。未配 Stop hook 的用户永远不会出现
  turn_done,回合结束由 `Idle2s` 落到 waiting_input —— 即今天的近似精度。
- **settle 窗口**:Stop OSC 到达后 ink 会重绘 composer,零星输出不得立刻把
  turn_done 翻回 running。watcher 用 Registry 记录的 `status_at` 判断,超过
  `TURN_DONE_SETTLE`(3s,常量)的输出活动才转换。waiting_input → running
  不受 settle 限制(允许权限后应立即恢复)。
- **`Output` 不由 reader 线程逐 chunk 驱动**(那会让每个输出块都拿 Registry
  锁):reader 线程只更新 `last_activity`(现状),watcher 现有 200ms 轮询
  顺带评估 `Idle2s` / `Output`,不新增定时器或线程。
- **闪烁逻辑原样保留**:watcher 的"每回合闪一次"与 handle_osc 的 Bell/
  PromptReady 闪烁全部不动;状态转换是叠加行为。

## Rust 侧改动

### `osc.rs`

- `OscEvent` 新增 `TurnDone`;`interpret()` 匹配 `"9" if value == "claude-turn-done"`。
- 现有 `claude-done` → `PromptReady` 分支不变(精确匹配,不会误吞)。

### `registry.rs`

- `TabRecord` 新增 `status: TabStatus`(初始 `Idle`)与 `status_at: Instant`
  (`status_at` 不参与序列化,仅供 settle 判断)。
- `set_status(&self, id, status) -> bool`:同值 no-op 返回 false(防事件风暴);
  变更时刷新 `status_at` 并返回 true。

### `status.rs`(新)

- `TabStatus` 枚举(serde `Serialize`,小写下划线)+ `Signal` 枚举 +
  `fn next(current: TabStatus, signal: Signal) -> Option<TabStatus>` 纯函数。
- 唯一对外入口:
  `fn transition(registry: &Registry, app: &AppHandle, tab_id: &str, signal: Signal)`
  —— **在 Registry 锁内完成"变更 + emit `tab-status`"**。reader 线程与
  watcher 并发转换时,锁内发射保证前端收到的事件顺序与状态变更顺序一致
  (与 session.rs 在 stream 锁内 emit `pty-output` 是同一个成文模式,代码
  注释需写明"不要把 emit 移出锁")。Registry 类型本身不依赖 tauri。

### `session.rs`

- `handle_osc`:
  - `TitleChanged`(程序标题)→ `transition(ProgramTitle)`;路径标题不动状态;
  - `Bell` → `transition(Bell)`(idle 态由转换表忽略);flash 照旧;
  - `PromptReady` → `transition(ClaudeDone)`;其余现有行为(clear_program_title、
    flash、prompt-ready 事件)全部保留;
  - 新增 `TurnDone` 分支 → `transition(TurnDone)`。
- attention watcher:`program_active && idle ≥ TURN_IDLE` 时,先
  `transition(Idle2s)`(仅 running 会变化),再走现有 flash-once 逻辑;
  `idle < TURN_IDLE` 时评估 `Output`(waiting_input 直接转;turn_done 需
  `status_at.elapsed() > TURN_DONE_SETTLE`)。`Idle2s` 与 `Output` 都只在
  `program_active` 为真时评估(与 watcher 现有的 continue 分支一致)。
- exit watcher:拿到 Registry 的 Arc,置 `transition(Exit)`;**不再 emit
  `tab-exit`**(事件整体移除,由 `tab-status{exited}` 取代)。

### `manager.rs` / `lib.rs`

- 删除 `remove_session`;删除 lib.rs 中对 `tab-exit` 的 Rust 侧监听与
  `TabExitPayload`。退出的 session **惰性留在 map**:三个线程已自然结束,
  `is_running()==false`,replay ring 保留 → webview 重载后已退出的 tab 仍能
  attach 回看最后一屏。rc 临时文件的清理从"退出即 Drop"推迟到 close_tab
  (Drop 仍会发生,只是时机后移;可接受)。
- `close_tab` 成为唯一删除路径,对已退出 tab 照常工作(session 还在 map,
  kill() 对死进程无害)。`has_active_process` 对已退出 tab 返回 false →
  关闭时不弹确认框(现有逻辑自然满足)。
- `TabResponse` 新增 `status: TabStatus` 字段(camelCase 序列化下字段名即
  `status`,值为小写下划线字符串)。
- 已退出 tab 上的 `pty_input` / `resize_pty`:对死 PTY 的写/缩放返回 Err,
  前端现有 catch 只 console.debug,无需改动。

## IPC 协议变化汇总

- **新增事件** `tab-status`:`{ tab_id: string, status: TabStatus }`,仅在状态
  实际变化时发射。
- **移除事件** `tab-exit`(前端与 Rust 侧消费者同时移除)。
- **变更** `create_tab` / `list_tabs` 返回的 `TabResponse` 增加 `status` 字段。
- 事件全集变为:`pty-output`、`tab-title`、`tab-cwd`、`tab-flash`、
  `prompt-ready`、`tab-status`、`menu-shortcut`。
- `TAB_GONE` 语义不变。

## 前端改动

### `types.ts`

```ts
export type TabStatus = 'idle' | 'running' | 'waiting_input' | 'turn_done' | 'exited';
```

`Tab` 与 `TabResponse` 增加 `status: TabStatus`;新增 `TabStatusPayload`
(`tab_id` + `status`);删除 `TabExitPayload`。

### `useTabManager.ts`

- `toTab` 带上 `status`(不再是纯前端字段,随 `TabResponse` 到达)。
- 新增 `tab-status` 监听 → 更新对应 tab 的 `status`。
- **移除 `tab-exit` 监听**(不再 abandonTab)。`abandonTab` 保留,仍服务于
  closeTab 的 TAB_GONE 路径。
- 闪烁逻辑不动:`switchTab` 只清 `flashing`,不清 `status`。

### `TabItem.tsx` + `App.css`

- 结构:badge 之后、`tab-text` 之前插入
  `<span class="tab-status" data-status={tab.status} aria-hidden="true" title={...} />`。
- `idle` 时透明占位(固定尺寸,避免状态出现/消失时布局跳动)。
- **`CHROME_WIDTH` 常量需加上状态点的宽度**(它参与标题可用宽度计算,漏改
  会导致标题截断位置错误)。
- `exited` 时整个 tab 行降透明度(置灰),状态点红色。
- 配色(初稿,实现时可微调):running 蓝 `#3b82f6` 脉动、waiting_input
  琥珀 `#f59e0b` 脉动、turn_done 绿 `#22c55e` 常亮、exited 红 `#ef4444` 常亮。
- a11y:点为 `aria-hidden` + `title` tooltip(与现有 badge 处理一致);
  状态语义不依赖颜色单独传达(title 文案如 "Claude 等待输入")。

## 文档改动

- **CLAUDE_HOOKS.md**:
  - 新增 Stop hook 配置段:

    ```json
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
    ```

  - hook 命令统一改为 `> /dev/tty` 形式(含现有 Notification 示例):hook 的
    stdout 可能被 Claude Code 捕获,直写控制终端才可靠。
  - 删除文末 "Stop 事件可能不支持 command hooks" 的过时注记(与新章节矛盾);
    注明降级路径:若某环境下 Stop hook 写 tty 不可行,不配置它即可,行为
    回落到 2 秒静默启发式,不阻塞本功能。
- **README.md / README.zh-CN.md**:特性一节各加一条状态灯说明(两份同步,
  含四种状态的含义)。
- **CLAUDE.md**:架构一节的事件列表去掉 `tab-exit`、加上 `tab-status`;
  Registry 描述补充 status 字段;"Data flow / invariants" 补充
  "tab-status 在 Registry 锁内发射"这一顺序保证。

## 测试

Rust 单测(`cargo test --lib`):

- `status.rs`:转换表**全覆盖**——每个 状态×信号 组合(含忽略项)、exited
  终态不可离开、idle 忽略 Bell、Idle2s 不覆盖 turn_done、ClaudeDone 回 idle。
- `osc.rs`:`\x1b]9;claude-turn-done\x1b\\` → `TurnDone`;`claude-done` 仍 →
  `PromptReady`;`claude-turn-done` 跨 read 分割仍能解码。
- `registry.rs`:`set_status` 同值返回 false 且不改 `status_at`;变更返回
  true 且刷新 `status_at`;insert 初始为 idle。

前端无测试运行器:`npm run typecheck` 是唯一检查。

手动验证清单(`npm run tauri dev`,配好 Notification + Stop hook):

1. 启动 claude → 蓝点脉动;
2. 触发权限确认 → 琥珀(有 Notification hook 即时;没有则 ~2s 后);
3. 允许 → 回蓝;
4. 回合结束 → 绿(Stop hook);随后 3s 内的重绘不翻回蓝;
5. 发下一条消息 → 蓝;
6. `exit` 退出 claude → 灯灭(idle);
7. `exit` 退出 shell → 标签置灰红点,画面保留可滚动回看;webview 重载后
   状态与画面均恢复;⌘W 关闭不弹确认框;
8. 全程闪烁行为与改动前一致(BEL 闪、回合结束闪、活跃 tab 不闪);
9. vim 里响铃(普通 shell,idle 态)→ 只闪不点灯。

## 明确不做(YAGNI)

- 不追踪非 Claude 命令的运行状态(preexec → running 的 Warp 式通用状态);
- 不加任何用户设置项(状态灯不可关闭、颜色不可配);
- 不做退出标签的自动清理/超时关闭;
- 不动 Terminal.tsx(replay 分类、渲染器选择均不受影响)。
