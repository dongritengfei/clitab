# OSC 7777 标签仪表盘 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Claude Code hooks 通过 OSC 7777 + JSON 向 clitab 上报回合状态,标签页第三行显示当前工具/耗时/通知,把标签变成仪表盘。

**Architecture:** `osc.rs` 只做字节帧解析(新变体 `Clitab(String)`,payload = 第一个 `;` 后所有参数重拼);新模块 `status.rs` 负责 JSON 解码与共享类型(`StatusEvent`/`TabStatus`/`Notice`);`registry.rs` 存每 tab 的 `status`/`notice`/`turn_start`(epoch 毫秒,跨 webview 重载存活);`session.rs` 把解码结果写入 registry 并广播新事件 `tab-status`;前端 `useTabManager` 监听事件、`TabItem` 渲染第三行(每秒跳动计时、Done 6 秒淡出、notice 高亮、切标签 ack)。

**Tech Stack:** Rust(Tauri 2、serde/serde_json、portable-pty)、React 18 + TypeScript、xterm.js(不碰)。

**Spec:** `docs/superpowers/specs/2026-10-01-osc-status-dashboard-design.md`

## Global Constraints

- Rust 校验命令:`cargo test --manifest-path src-tauri/Cargo.toml --lib`;前端校验命令:`npm run typecheck`(无 linter、无前端测试运行器)。
- 协议常量:OSC 码 `7777`;载荷上限沿用现有 `MAX_OSC_LEN`(4096);时间戳一律 **epoch 毫秒 u64**(不用 `Instant`,要跨重载)。
- 新事件名 `tab-status`;新命令名 `ack_tab_notice`(参数 `tab_id`,invoke 侧写 `{ tabId }`,Tauri 自动映射)。
- **不碰** `src-tauri/src/pty/shell_integration.rs`;`OSC 9;claude-done` 与 BEL 的现有语义不变;`stop` 事件不得触碰标题/`program_active`/`PromptReady` 逻辑。
- 不在 renderer 重新实现快捷键;不引入 WebGL(项目既定选择,见 CLAUDE.md)。
- `README.md` 与 `README.zh-CN.md` 必须同步修改。
- 文档中每条 hook 命令必须保证退出码为 0(`|| true` / `; true` 收尾),且写 `/dev/tty` 而非 stdout(PreToolUse 的 stdout 会被 Claude Code 当决策 JSON 解析)。
- `ack_tab_notice` 对不存在的 tab 静默成功(不返回 `TAB_GONE`)。
- 提交信息风格沿用仓库现状(如 `feat: ...` / `docs: ...`,首行小写简短)。

## Review Focus

1. **JSON 字符串里含 `;`**(通知消息如 "wait; then retry")→ 解析器按 `;` 切参后必须无损重拼,否则解码失败、事件静默丢失。→ Task 2 的 `semicolons_inside_json_survive_rejoin` 钉死。
2. **用户没装 jq** → PreToolUse 静默失效(可接受),但 Notification 必须降级为无 msg 的 `{"e":"notify"}` 仍然闪烁。→ Task 3 的 `notify_without_msg_still_decodes` + Task 9 的命令模板 `|| printf '{"e":"notify"}'` 钉死。
3. **hooks 只装了一半**(如只有 PreToolUse+Stop,没有 UserPromptSubmit)→ 耗时仍能从第一个 tool 事件起算,Stop 不报错。→ Task 4 的 `tool_without_prompt_starts_the_clock` 钉死。
4. **二进制垃圾/cat 出的伪 OSC 7777** → 坏 JSON 一律静默忽略、不 panic、不无限累积(现有 runaway cap 继续生效)。→ Task 3 的 `malformed_input_is_ignored` 钉死。
5. **回合中途 webview 重载** → `list_tabs` 必须带回 status/notice 且时间戳仍有效(epoch 毫秒),第三行原样恢复。→ Task 6 的 `tab_response_serializes_protocol_state` + Task 10 的手动重载步骤钉死。

---

### Task 1: 环境探针(/dev/tty 可写性 + Stop hook 实测)

**Files:**
- Create: `/tmp/clitab-probe-settings.json`(临时,不入库)
- Test: 无代码;本任务输出是"探针结论",写入执行日志

**Interfaces:**
- Consumes: 无
- Produces: 两条事实结论,后续任务的前提——(a) hook 执行环境 `/dev/tty` 可写;(b) Stop 事件支持 command hook。**任一失败:停止执行,回到设计阶段与用户重新讨论(见 spec"风险与早期探针")。**

- [ ] **Step 1: 确认 jq 与 claude 可用**

```bash
command -v jq && jq --version; command -v claude && claude --version
```

预期:两者都在 PATH。jq 缺失只影响文档措辞(降级路径),不阻塞;claude 缺失则无法探针,改为在真实 clitab 标签里手动验证(跳到 Step 4 的备用方案)。

- [ ] **Step 2: 写探针 hooks 配置**

```bash
cat > /tmp/clitab-probe-settings.json <<'EOF'
{
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [ { "type": "command", "command": "echo prompt-hook >> /tmp/clitab-probe.log; { printf 'PROMPT-TTY\\n' > /dev/tty; } 2>/dev/null || echo no-prompt-tty >> /tmp/clitab-probe.log; true" } ] }
    ],
    "PreToolUse": [
      { "matcher": "", "hooks": [ { "type": "command", "command": "n=$(jq -r .tool_name 2>/dev/null); echo pretool-$n >> /tmp/clitab-probe.log; true" } ] }
    ],
    "Stop": [
      { "hooks": [ { "type": "command", "command": "echo stop-hook >> /tmp/clitab-probe.log; { printf 'STOP-TTY\\n' > /dev/tty; } 2>/dev/null || echo no-stop-tty >> /tmp/clitab-probe.log; true" } ] }
    ]
  }
}
EOF
```

- [ ] **Step 3: 在有控制终端的环境跑一次真实 claude 会话**

用 `script`(BSD 版)分配一个 pty,模拟 clitab 标签里 claude 的运行环境(hook 进程的控制终端即该 pty):

```bash
rm -f /tmp/clitab-probe.log /tmp/clitab-probe.txt
script -q /tmp/clitab-probe.txt claude -p --settings /tmp/clitab-probe-settings.json --allowedTools Bash 'Use the Bash tool to run: echo probe-hello. Then reply done.'
echo '--- probe log ---'; cat /tmp/clitab-probe.log
echo '--- tty writes captured by pty ---'; grep -c 'TTY' /tmp/clitab-probe.txt
```

判定规则(全部满足才继续):
- `/tmp/clitab-probe.log` 含 `stop-hook` → **Stop hook 支持 command hook**;
- `grep -c TTY` ≥ 1(且 log 中**没有** `no-prompt-tty`/`no-stop-tty`)→ **/dev/tty 可写**;
- log 含 `pretool-Bash` → jq 提取 stdin 字段可行。

任一不满足:停止,报告用户,回到 brainstorming(spec 的备选:stdout 仅对 Stop/Notification 安全;Stop 不支持则耗时退化为会话级)。

- [ ] **Step 4: 备用方案(仅当 claude 不在 PATH)**

无法自动探针时,请用户在任意终端手动执行 Step 3 的 `script ...` 命令并粘贴 `/tmp/clitab-probe.log` 与 grep 结果,按同样规则判定。

- [ ] **Step 5: 清理**

```bash
rm -f /tmp/clitab-probe-settings.json /tmp/clitab-probe.log /tmp/clitab-probe.txt
```

无提交(本任务不产生代码)。

---

### Task 2: osc.rs — OSC 7777 帧解析与 payload 重拼

**Files:**
- Modify: `src-tauri/src/osc.rs`(`OscEvent` 枚举、`push_param`、`finish_osc`、`interpret`,及 `InOsc` 状态下 ESC 分支)
- Test: `src-tauri/src/osc.rs` 的 `mod tests`

**Interfaces:**
- Consumes: 无(纯字节层)。
- Produces: `OscEvent::Clitab(String)` —— String 是第一个 `;` 之后**全部**参数用 `;` 重拼的完整 payload(对 7777 是原始 JSON 文本;空 payload 不产生事件)。Task 5 依赖此变体。

背景(执行者需要知道的):现解析器按 `;` 切参数且**跳过空段**,这会把 JSON 里连续的 `;;` 拼丢一个分号;而终结符(BEL/ST)前的 flush 若也推空段,会给重拼结果追加假 `;` 导致 JSON 解析失败。规则定为:**`;` 分隔处总是推段(含空段);序列结束/ESC 处只推非空段**。同时 `interpret` 对所有码统一"payload = params[1..] 重拼"——这让含 `;` 的窗口标题也比现在(只取第一段)更忠实,现有测试不受影响。

- [ ] **Step 1: 写失败测试**

在 `src-tauri/src/osc.rs` 的 `mod tests` 末尾追加:

```rust
    #[test]
    fn osc7777_json_payload() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x1b]7777;{\"e\":\"stop\"}\x1b\\");
        assert_eq!(events, vec![OscEvent::Clitab("{\"e\":\"stop\"}".into())]);
    }

    /// `;` is legal inside JSON strings; the parameter splitter must not eat
    /// it, and consecutive semicolons must survive the rejoin.
    #[test]
    fn semicolons_inside_json_survive_rejoin() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x1b]7777;{\"e\":\"notify\",\"msg\":\"a;b;;c\"}\x07");
        assert_eq!(
            events,
            vec![OscEvent::Clitab("{\"e\":\"notify\",\"msg\":\"a;b;;c\"}".into())]
        );
    }

    #[test]
    fn osc7777_empty_payload_is_ignored() {
        let mut parser = OscParser::new();
        assert!(parser.parse(b"\x1b]7777;\x07").is_empty());
        assert!(parser.parse(b"\x1b]7777;\x1b\\").is_empty());
        // The parser is still healthy afterwards.
        let events = parser.parse(b"\x1b]7777;{\"e\":\"prompt\"}\x07");
        assert_eq!(events, vec![OscEvent::Clitab("{\"e\":\"prompt\"}".into())]);
    }

    #[test]
    fn osc7777_split_across_reads() {
        let mut parser = OscParser::new();
        assert!(parser.parse(b"\x1b]7777;{\"e\":").is_empty());
        let events = parser.parse(b"\"tool\",\"tool\":\"Bash\"}\x1b\\");
        assert_eq!(
            events,
            vec![OscEvent::Clitab("{\"e\":\"tool\",\"tool\":\"Bash\"}".into())]
        );
    }

    /// The rejoin also makes multi-semicolon titles faithful instead of
    /// truncating at the first `;`.
    #[test]
    fn title_with_semicolon_keeps_full_payload() {
        let mut parser = OscParser::new();
        let events = parser.parse(b"\x1b]0;user@host;project\x07");
        assert_eq!(
            events,
            vec![OscEvent::TitleChanged("user@host;project".into())]
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib osc::`
Expected: 新测试编译失败(`no variant or associated item named 'Clitab'`)。

- [ ] **Step 3: 实现**

3a. `OscEvent` 枚举加变体(放在 `PromptReady` 之后):

```rust
    /// OSC 7777: the clitab hook protocol. The payload is the raw text after
    /// the first `;` (parameters rejoined losslessly); JSON semantics live in
    /// `crate::status`, not here.
    Clitab(String),
```

3b. 把 `push_param` 拆成两个方法(替换现有 `push_param`):

```rust
    /// A `;` inside the payload: always record the segment, even an empty
    /// one, so a rejoined payload (title, JSON) keeps every separator.
    fn push_param(&mut self) {
        self.params.push(std::mem::take(&mut self.buffer));
    }

    /// End-of-sequence flush: a trailing empty segment carries no information
    /// and would corrupt a rejoined payload, so drop it.
    fn flush_param(&mut self) {
        if !self.buffer.is_empty() {
            self.push_param();
        }
    }
```

3c. `finish_osc` 首行 `self.push_param();` 改为 `self.flush_param();`。

3d. `State::InOsc` 的 `0x1b` 分支里 `self.push_param();` 改为 `self.flush_param();`(保持与今日行为一致:ESC 处只推非空段)。

3e. `interpret` 整体替换为:

```rust
    fn interpret(params: &[Vec<u8>]) -> Option<OscEvent> {
        if params.len() < 2 {
            return None;
        }
        let code = str::from_utf8(&params[0]).ok()?;
        // The payload is everything after the first `;`, rejoined: `;` is a
        // legal character inside titles, paths and OSC 7777 JSON, so the
        // parameter split is only a framing convenience.
        let parts: Option<Vec<&str>> = params[1..]
            .iter()
            .map(|p| str::from_utf8(p).ok())
            .collect();
        let value = parts?.join(";");

        match code {
            "0" | "1" | "2" => Some(OscEvent::TitleChanged(value)),
            "7" => Some(OscEvent::CwdChanged(parse_osc7_path(&value))),
            "9" if value == "claude-done" => Some(OscEvent::PromptReady),
            "7777" if !value.is_empty() => Some(OscEvent::Clitab(value)),
            _ => None,
        }
    }
```

- [ ] **Step 4: 跑全部 osc 测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib osc::`
Expected: 全部 PASS(含既有 17 个测试——重拼不改变单段 payload 的语义)。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/osc.rs
git commit -m "feat: parse OSC 7777 hook-protocol frames with lossless payload rejoin"
```

---

### Task 3: status.rs — JSON 解码与共享类型

**Files:**
- Create: `src-tauri/src/status.rs`
- Modify: `src-tauri/src/lib.rs`(仅加一行 `mod status;`)
- Test: `src-tauri/src/status.rs` 的 `mod tests`

**Interfaces:**
- Consumes: 无(输入是 `&str` 原始 JSON)。
- Produces(Task 4/5/6 依赖,签名精确如下):
  - `pub fn decode(json: &str) -> Option<StatusEvent>`
  - `pub enum StatusEvent { Prompt, Tool { name: String }, Stop, Notify { msg: Option<String> } }`(derive `Debug, Clone, PartialEq, Eq`)
  - `pub enum TabStatus { Thinking { since: u64 }, Tool { name: String, since: u64 }, Done { duration: Option<u64>, at: u64 } }`(derive `Debug, Clone, PartialEq, Eq, Serialize`,`#[serde(tag = "kind", rename_all = "camelCase")]`;**没有 Idle 变体——"空闲"就是 `None`**,与前端可空字段对齐)
  - `pub struct Notice { pub msg: Option<String>, pub at: u64 }`(derive `Debug, Clone, PartialEq, Eq, Serialize`,`#[serde(rename_all = "camelCase")]`)

- [ ] **Step 1: 写失败测试**

创建 `src-tauri/src/status.rs`,先只写测试骨架与 `mod tests`(实现留空会让测试编译失败,这正是预期):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_each_event() {
        assert_eq!(decode(r#"{"e":"prompt"}"#), Some(StatusEvent::Prompt));
        assert_eq!(
            decode(r#"{"e":"tool","tool":"Bash"}"#),
            Some(StatusEvent::Tool { name: "Bash".into() })
        );
        assert_eq!(decode(r#"{"e":"stop"}"#), Some(StatusEvent::Stop));
        assert_eq!(
            decode(r#"{"e":"notify","msg":"needs permission"}"#),
            Some(StatusEvent::Notify { msg: Some("needs permission".into()) })
        );
    }

    /// The Notification hook degrades to no-msg when jq is missing; the flash
    /// must still work, so msg is optional.
    #[test]
    fn notify_without_msg_still_decodes() {
        assert_eq!(decode(r#"{"e":"notify"}"#), Some(StatusEvent::Notify { msg: None }));
        assert_eq!(
            decode(r#"{"e":"notify","msg":null}"#),
            Some(StatusEvent::Notify { msg: None })
        );
    }

    /// Forward compatibility: v2 fields (tokens, cost) must not break v1.
    #[test]
    fn unknown_extra_fields_are_ignored() {
        assert_eq!(
            decode(r#"{"e":"stop","session":"x","cost":{"usd":0.1}}"#),
            Some(StatusEvent::Stop)
        );
    }

    #[test]
    fn malformed_input_is_ignored() {
        assert_eq!(decode("not json"), None);
        assert_eq!(decode(""), None);
        assert_eq!(decode("{}"), None); // no event kind
        assert_eq!(decode(r#"{"e":"bogus"}"#), None); // unknown kind
        assert_eq!(decode(r#"{"e":"tool"}"#), None); // tool without name
        assert_eq!(decode(r#"{"e":"tool","tool":""}"#), None); // empty name
        assert_eq!(decode("\u{0}garbage\u{0}"), None); // binary junk
    }

    #[test]
    fn tab_status_serializes_camel_case() {
        let json = serde_json::to_value(TabStatus::Tool { name: "Bash".into(), since: 42 }).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "tool", "name": "Bash", "since": 42}));
        let json = serde_json::to_value(TabStatus::Done { duration: None, at: 7 }).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "done", "duration": null, "at": 7}));
        let json = serde_json::to_value(Notice { msg: Some("hi".into()), at: 7 }).unwrap();
        assert_eq!(json, serde_json::json!({"msg": "hi", "at": 7}));
    }
}
```

- [ ] **Step 2: 注册模块并确认编译失败**

在 `src-tauri/src/lib.rs` 顶部 `mod pty;` 之后加:

```rust
mod status;
```

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib status::`
Expected: 编译失败(`decode`/`StatusEvent`/`TabStatus`/`Notice` 未定义)。

- [ ] **Step 3: 写实现**

在 `status.rs` 测试模块**之前**写入:

```rust
//! The clitab hook protocol: OSC 7777 carrying a small JSON payload.
//!
//! Claude Code hooks (UserPromptSubmit / PreToolUse / Stop / Notification)
//! printf these sequences to the tab's PTY; the OSC parser hands us the raw
//! JSON text and this module gives it meaning. Everything is deliberately
//! tolerant: unknown event kinds, extra fields (v2 will add token/cost) and
//! malformed payloads decode to `None` and are silently dropped — a terminal
//! must never break because a hook emitted something new.

use serde::{Deserialize, Serialize};

/// A decoded protocol event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusEvent {
    /// UserPromptSubmit: an assistant turn began.
    Prompt,
    /// PreToolUse: a tool is about to run.
    Tool { name: String },
    /// Stop: the turn ended. Duration is computed by the receiver, not sent.
    Stop,
    /// Notification: the session wants attention. `msg` is optional because
    /// the hook degrades to a bare notify when jq is unavailable.
    Notify { msg: Option<String> },
}

/// Wire shape. Unknown fields are ignored by serde, which is the forward
/// compatibility guarantee for v2 payloads.
#[derive(Debug, Deserialize)]
struct Wire {
    e: String,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    msg: Option<String>,
}

/// Decode one OSC 7777 payload. Returns `None` for anything we do not
/// understand; callers must treat that as "ignore", never as an error.
pub fn decode(json: &str) -> Option<StatusEvent> {
    let wire: Wire = serde_json::from_str(json).ok()?;
    match wire.e.as_str() {
        "prompt" => Some(StatusEvent::Prompt),
        // An empty tool name would render as a blank dashboard cell; treat it
        // like a missing one.
        "tool" => Some(StatusEvent::Tool {
            name: wire.tool.filter(|name| !name.is_empty())?,
        }),
        "stop" => Some(StatusEvent::Stop),
        "notify" => Some(StatusEvent::Notify { msg: wire.msg }),
        _ => None,
    }
}

/// The turn state shown on a tab's status line. Timestamps are epoch
/// milliseconds (not `Instant`): the state crosses the IPC boundary and must
/// still make sense after a webview reload. "Idle" is represented as `None`
/// at the storage layer — a tab that never spoke the protocol has no status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TabStatus {
    /// Turn in flight, no tool reported yet.
    Thinking { since: u64 },
    Tool { name: String, since: u64 },
    /// Turn finished; `duration` is None when the start was never observed
    /// (partially installed hooks).
    Done { duration: Option<u64>, at: u64 },
}

/// A Notification-hook message awaiting the user. Orthogonal to `TabStatus`:
/// acknowledging it must reveal the turn state underneath, not lose it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub msg: Option<String>,
    pub at: u64,
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib status::`
Expected: 全部 PASS。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/status.rs src-tauri/src/lib.rs
git commit -m "feat: decode the OSC 7777 hook protocol into typed status events"
```

---

### Task 4: registry.rs — 状态存储与迁移

**Files:**
- Modify: `src-tauri/src/pty/registry.rs`(`TabRecord` 加三字段;`insert` 初始化;新增六个方法;新测试)
- Test: `src-tauri/src/pty/registry.rs` 的 `mod tests`

**Interfaces:**
- Consumes: Task 3 的 `crate::status::{Notice, TabStatus}`。
- Produces(Task 5/6 依赖,签名精确如下,全部 `&self`):
  - `pub fn begin_turn(&self, id: &str, now_ms: u64)`
  - `pub fn set_tool(&self, id: &str, name: &str, now_ms: u64)`
  - `pub fn end_turn(&self, id: &str, now_ms: u64)`
  - `pub fn set_notice(&self, id: &str, msg: Option<String>, now_ms: u64)`
  - `pub fn clear_notice(&self, id: &str)`
  - `TabRecord` 新公有字段:`status: Option<TabStatus>`、`notice: Option<Notice>`、`turn_start: Option<u64>`(turn_start 仅后端内部用,不进 TabResponse)
  - 语义:`begin_turn`/`set_tool`/`end_turn` 都清 notice;`set_tool` 在 turn_start 缺失时用 now 兜底(hooks 半装场景);`end_turn` 算完 duration 把 turn_start 清回 None;未知 id 一律静默 no-op。

- [ ] **Step 1: 写失败测试**

在 `src-tauri/src/pty/registry.rs` 的 `mod tests` 末尾追加:

```rust
    use crate::status::{Notice, TabStatus};

    fn status_fixture() -> Registry {
        let registry = Registry::new();
        registry.insert("t1".into(), "/tmp".into());
        registry
    }

    #[test]
    fn turn_lifecycle_thinking_tool_done() {
        let r = status_fixture();
        r.begin_turn("t1", 1000);
        let tab = r.get("t1").unwrap();
        assert_eq!(tab.status, Some(TabStatus::Thinking { since: 1000 }));
        assert_eq!(tab.turn_start, Some(1000));

        r.set_tool("t1", "Bash", 1500);
        let tab = r.get("t1").unwrap();
        assert_eq!(
            tab.status,
            Some(TabStatus::Tool { name: "Bash".into(), since: 1500 })
        );
        assert_eq!(tab.turn_start, Some(1000), "tool must not restart the clock");

        r.end_turn("t1", 4200);
        let tab = r.get("t1").unwrap();
        assert_eq!(
            tab.status,
            Some(TabStatus::Done { duration: Some(3200), at: 4200 })
        );
        assert_eq!(tab.turn_start, None, "end_turn consumes the start mark");
    }

    /// Hooks may be only partially installed: without UserPromptSubmit the
    /// first tool starts the clock, so Stop can still report a duration.
    #[test]
    fn tool_without_prompt_starts_the_clock() {
        let r = status_fixture();
        r.set_tool("t1", "Read", 500);
        r.end_turn("t1", 900);
        assert_eq!(
            r.get("t1").unwrap().status,
            Some(TabStatus::Done { duration: Some(400), at: 900 })
        );
    }

    #[test]
    fn stop_without_any_start_has_no_duration() {
        let r = status_fixture();
        r.end_turn("t1", 900);
        assert_eq!(
            r.get("t1").unwrap().status,
            Some(TabStatus::Done { duration: None, at: 900 })
        );
    }

    #[test]
    fn notice_is_orthogonal_to_status() {
        let r = status_fixture();
        r.set_tool("t1", "Bash", 100);
        r.set_notice("t1", Some("needs permission".into()), 200);
        let tab = r.get("t1").unwrap();
        assert_eq!(
            tab.status,
            Some(TabStatus::Tool { name: "Bash".into(), since: 100 }),
            "a notice must not clobber the turn state"
        );
        assert_eq!(
            tab.notice,
            Some(Notice { msg: Some("needs permission".into()), at: 200 })
        );

        r.clear_notice("t1");
        let tab = r.get("t1").unwrap();
        assert_eq!(tab.notice, None);
        assert!(matches!(tab.status, Some(TabStatus::Tool { .. })));
    }

    #[test]
    fn turn_events_clear_the_notice() {
        let r = status_fixture();
        for apply in [
            |r: &Registry| r.begin_turn("t1", 300),
            |r: &Registry| r.set_tool("t1", "Bash", 300),
            |r: &Registry| r.end_turn("t1", 300),
        ] {
            r.set_notice("t1", Some("stale".into()), 200);
            apply(&r);
            assert_eq!(r.get("t1").unwrap().notice, None);
        }
    }

    #[test]
    fn unknown_tab_status_ops_are_noops() {
        let r = status_fixture();
        r.begin_turn("nope", 1);
        r.set_tool("nope", "Bash", 1);
        r.end_turn("nope", 1);
        r.set_notice("nope", None, 1);
        r.clear_notice("nope");
        let tab = r.get("t1").unwrap();
        assert_eq!(tab.status, None);
        assert_eq!(tab.notice, None);
    }

    #[test]
    fn fresh_tab_has_no_protocol_state() {
        let r = status_fixture();
        let tab = r.get("t1").unwrap();
        assert_eq!(tab.status, None);
        assert_eq!(tab.notice, None);
        assert_eq!(tab.turn_start, None);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib registry::`
Expected: 编译失败(`TabRecord` 无 `status` 字段、方法不存在)。

- [ ] **Step 3: 实现**

3a. 文件头 use 区加:

```rust
use crate::status::{Notice, TabStatus};
```

3b. `TabRecord` 加三个字段(放在 `has_program_title` 之后):

```rust
    /// Turn state reported via the OSC 7777 hook protocol; `None` until the
    /// tab's session first speaks it (plain shell tabs never do).
    pub status: Option<TabStatus>,
    /// A Notification-hook message awaiting the user; orthogonal to `status`.
    pub notice: Option<Notice>,
    /// Epoch ms of the current turn's start, consumed by `end_turn`.
    pub turn_start: Option<u64>,
```

3c. `insert()` 构造 `TabRecord` 时补上 `status: None, notice: None, turn_start: None`。

3d. 在 `clear_program_title` 之后、`list` 之前加迁移方法:

```rust
    /// UserPromptSubmit: a turn began. Any stale notice is by definition
    /// answered — the user just typed.
    pub fn begin_turn(&self, id: &str, now_ms: u64) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            tab.status = Some(TabStatus::Thinking { since: now_ms });
            tab.turn_start = Some(now_ms);
            tab.notice = None;
        }
    }

    /// PreToolUse: a tool is running. When hooks are only partially installed
    /// (no UserPromptSubmit), the first tool starts the clock so `end_turn`
    /// can still report a duration.
    pub fn set_tool(&self, id: &str, name: &str, now_ms: u64) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            tab.status = Some(TabStatus::Tool {
                name: name.to_string(),
                since: now_ms,
            });
            if tab.turn_start.is_none() {
                tab.turn_start = Some(now_ms);
            }
            tab.notice = None;
        }
    }

    /// Stop: the turn ended. Duration is None when no start was ever
    /// observed; the start mark is consumed either way.
    pub fn end_turn(&self, id: &str, now_ms: u64) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            let duration = tab.turn_start.map(|start| now_ms.saturating_sub(start));
            tab.status = Some(TabStatus::Done {
                duration,
                at: now_ms,
            });
            tab.turn_start = None;
            tab.notice = None;
        }
    }

    /// Notification: park a message for the user without touching the turn
    /// state underneath.
    pub fn set_notice(&self, id: &str, msg: Option<String>, now_ms: u64) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            tab.notice = Some(Notice { msg, at: now_ms });
        }
    }

    /// The user switched to the tab and saw the notice.
    pub fn clear_notice(&self, id: &str) {
        let mut tabs = lock(&self.tabs);
        if let Some(tab) = tabs.iter_mut().find(|t| t.id == id) {
            tab.notice = None;
        }
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib registry::`
Expected: 全部 PASS(含既有 6 个测试)。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/pty/registry.rs
git commit -m "feat: store hook-protocol turn status and notices in the tab registry"
```

---

### Task 5: session.rs — 事件接线、共享 flashed 标志、tab-status 广播

**Files:**
- Modify: `src-tauri/src/pty/session.rs`(use 区、`PtySession::new` 中 watcher 线程与 read_loop 启动、`read_loop` 签名、`handle_osc`、新增 `handle_status` 与 `now_ms`)
- Test: 无新单元测试(`handle_osc` 需要 `AppHandle`,项目现状对它只有集成级覆盖);以全量回归 + Task 10 手动验证兜底

**Interfaces:**
- Consumes: `crate::status::{self, StatusEvent}`(Task 3)、registry 六个迁移方法(Task 4)、`OscEvent::Clitab`(Task 2)。
- Produces: Tauri 事件 **`tab-status`**,payload 形状(Task 7 依赖):
  `{ "tab_id": string, "status": TabStatus | null, "notice": { msg, at } | null }`。
  `Stop` 额外发 `tab-flash`;`Notify` 额外发 `tab-flash`。

- [ ] **Step 1: 实现——use 区**

在 `use crate::osc::{self, OscEvent, OscParser};` 之后加:

```rust
use crate::status::{self, StatusEvent};
```

- [ ] **Step 2: 实现——共享 flashed 标志**

2a. 在 `PtySession::new` 里、`let program_active = Arc::new(AtomicBool::new(false));` 之后加:

```rust
        // Set when the tab flashed for the current turn — shared between the
        // idle watcher and the hook protocol so an explicit `stop` event can
        // flash immediately without the watcher repeating it 2s later.
        let flashed = Arc::new(AtomicBool::new(false));
```

2b. reader 线程块:克隆 `flashed` 并传入(与 `program_active` 同样处理):

```rust
            let flashed = Arc::clone(&flashed);
```

`thread::spawn` 里调用改为:

```rust
                Self::read_loop(
                    tab_id,
                    app,
                    registry,
                    reader,
                    stream,
                    running,
                    last_activity,
                    program_active,
                    flashed,
                );
```

2c. attention watcher 线程块:删掉线程内 `let mut flashed = false;`,改为克隆共享标志:

```rust
            let flashed = Arc::clone(&flashed);
```

循环体相应替换(语义与原局部 bool 完全一致,只是换成原子量):

```rust
                while running.load(Ordering::Relaxed) {
                    thread::sleep(WATCHER_POLL);
                    if !program_active.load(Ordering::Relaxed) {
                        flashed.store(false, Ordering::Relaxed);
                        continue;
                    }
                    let idle = lock(&last_activity).elapsed();
                    if idle < TURN_IDLE {
                        continue; // still streaming
                    }
                    // swap: only the first caller of a turn emits the flash.
                    if !flashed.swap(true, Ordering::Relaxed) {
                        let _ = app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
                    }
                }
```

2d. `read_loop` 签名末尾加参数 `flashed: Arc<AtomicBool>`,循环内 `Self::handle_osc(...)` 调用透传 `&flashed`。

- [ ] **Step 3: 实现——handle_osc 新分支与 handle_status**

3a. `handle_osc` 签名加参数 `flashed: &AtomicBool`(放在 `last_activity` 之后);match 末尾(`PromptReady` 分支后)加:

```rust
            OscEvent::Clitab(json) => {
                // Unknown kinds and malformed payloads decode to None and are
                // dropped: a hook emitting something newer than this build
                // must be a no-op, never an error.
                if let Some(event) = status::decode(&json) {
                    Self::handle_status(tab_id, app, registry, flashed, event);
                }
            }
```

3b. `handle_osc` 之后新增两个函数:

```rust
    /// Apply one hook-protocol event: registry transition, then broadcast the
    /// tab's full protocol state so the renderer replaces (not merges) it.
    fn handle_status(
        tab_id: &str,
        app: &AppHandle,
        registry: &Registry,
        flashed: &AtomicBool,
        event: StatusEvent,
    ) {
        let now = now_ms();
        match event {
            StatusEvent::Prompt => registry.begin_turn(tab_id, now),
            StatusEvent::Tool { name } => registry.set_tool(tab_id, &name, now),
            StatusEvent::Stop => {
                registry.end_turn(tab_id, now);
                // An explicit turn-end beats the 2s idle heuristic: flash now
                // and suppress the watcher's duplicate.
                flashed.store(true, Ordering::Relaxed);
                let _ = app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
                // Deliberately does NOT touch program_active or the title:
                // Claude Code is still running between turns. (Title revert
                // stays owned by the shell integration's `claude-done`.)
            }
            StatusEvent::Notify { msg } => {
                registry.set_notice(tab_id, msg, now);
                let _ = app.emit("tab-flash", serde_json::json!({ "tab_id": tab_id }));
            }
        }
        if let Some(tab) = registry.get(tab_id) {
            let _ = app.emit(
                "tab-status",
                serde_json::json!({
                    "tab_id": tab_id,
                    "status": tab.status,
                    "notice": tab.notice,
                }),
            );
        }
    }
```

```rust
/// Epoch milliseconds, the clock the whole protocol speaks: `Instant` would
/// not survive the IPC boundary or a webview reload.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
```

- [ ] **Step 4: 全量回归**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib` 与 `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 全部 PASS、无新 warning(注意 `handle_osc` 已有 `#[allow(clippy::too_many_arguments)]` 在 `read_loop` 上;若 `handle_osc` 因新参数触发同款 lint,在其上方补同样的 allow)。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/pty/session.rs
git commit -m "feat: route hook-protocol events through the registry to a tab-status broadcast"
```

---

### Task 6: lib.rs + manager.rs — IPC 表面(TabResponse 字段、ack_tab_notice)

**Files:**
- Modify: `src-tauri/src/lib.rs`(use 区、`TabResponse`、`From<TabRecord>`、新命令、invoke_handler 列表、测试)
- Modify: `src-tauri/src/pty/manager.rs`(新增 `ack_notice` 方法)
- Test: `src-tauri/src/lib.rs` 的 `mod tests`

**Interfaces:**
- Consumes: `crate::status::{Notice, TabStatus}`(Task 3)、`Registry::clear_notice`(Task 4)、`TabRecord` 新字段(Task 4)。
- Produces(Task 7 依赖):
  - `list_tabs` / `create_tab` 返回的 `TabResponse` JSON 增加 `status`(对象或 null,`kind` 判别)与 `notice`(`{msg, at}` 或 null)字段;camelCase。
  - 新命令 `ack_tab_notice(tab_id: String) -> Result<(), String>`:清 notice,未知 tab 也返回 Ok。

- [ ] **Step 1: 写失败测试**

在 `src-tauri/src/lib.rs` 的 `mod tests` 末尾追加:

```rust
    use crate::status::{Notice, TabStatus};

    /// Pins the IPC contract for `src/types.ts`: camelCase, `kind`-tagged
    /// status, nulls (not absent keys) for missing protocol state, and the
    /// backend-internal `turn_start` never leaks.
    #[test]
    fn tab_response_serializes_protocol_state() {
        let record = TabRecord {
            id: "t1".into(),
            title: "Fix build".into(),
            cwd: "/tmp".into(),
            has_program_title: true,
            status: Some(TabStatus::Tool {
                name: "Bash".into(),
                since: 1700000000000,
            }),
            notice: None,
            turn_start: Some(1700000000000),
        };
        let json = serde_json::to_value(TabResponse::from(record)).unwrap();
        assert_eq!(
            json["status"],
            serde_json::json!({"kind": "tool", "name": "Bash", "since": 1700000000000u64})
        );
        assert_eq!(json["notice"], serde_json::Value::Null);
        assert!(json.get("turnStart").is_none());

        let idle = TabRecord {
            id: "t2".into(),
            title: "/tmp".into(),
            cwd: "/tmp".into(),
            has_program_title: false,
            status: None,
            notice: Some(Notice {
                msg: Some("needs permission".into()),
                at: 5,
            }),
            turn_start: None,
        };
        let json = serde_json::to_value(TabResponse::from(idle)).unwrap();
        assert_eq!(json["status"], serde_json::Value::Null);
        assert_eq!(
            json["notice"],
            serde_json::json!({"msg": "needs permission", "at": 5})
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib tests::tab_response`
Expected: 编译失败(`TabResponse` 无 `status`/`notice` 字段)。

- [ ] **Step 3: 实现**

3a. `lib.rs` use 区加:

```rust
use crate::status::{Notice, TabStatus};
```

3b. `TabResponse` 加字段并更新 `From`:

```rust
pub struct TabResponse {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub has_claude_title: bool,
    /// Hook-protocol turn state; null until the tab's session speaks it.
    pub status: Option<TabStatus>,
    /// Notification awaiting the user; null once acknowledged.
    pub notice: Option<Notice>,
}

impl From<TabRecord> for TabResponse {
    fn from(tab: TabRecord) -> Self {
        Self {
            id: tab.id,
            title: tab.title,
            cwd: tab.cwd,
            has_claude_title: tab.has_program_title,
            status: tab.status,
            notice: tab.notice,
        }
    }
}
```

(`turn_start` 有意不进 response——纯后端内部状态。)

3c. `manager.rs` 在 `list_tabs` 之前加:

```rust
    /// The renderer acknowledges a tab's notice by switching to it. Unknown
    /// tabs are a silent no-op: the notice died with the tab.
    pub fn ack_notice(&self, tab_id: &str) {
        self.registry.clear_notice(tab_id);
    }
```

3d. `lib.rs` 新命令(放在 `has_active_process` 之后):

```rust
/// The user switched to this tab, so its notification has been seen. Never
/// errors on an unknown tab: a stale ack is harmless.
#[tauri::command]
fn ack_tab_notice(state: State<'_, AppState>, tab_id: String) -> Result<(), String> {
    state.tab_manager.ack_notice(&tab_id);
    Ok(())
}
```

3e. `invoke_handler` 列表在 `has_active_process` 后加 `ack_tab_notice`。

- [ ] **Step 4: 跑测试确认通过 + 全量回归**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: 全部 PASS。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/lib.rs src-tauri/src/pty/manager.rs
git commit -m "feat: expose tab status over IPC and add the ack_tab_notice command"
```

---

### Task 7: 前端数据层 — types.ts + useTabManager.ts

**Files:**
- Modify: `src/types.ts`
- Modify: `src/hooks/useTabManager.ts`(import、`switchTab`、事件监听列表)
- Test: `npm run typecheck`(项目无前端测试运行器)

**Interfaces:**
- Consumes: Task 5 的 `tab-status` 事件 payload、Task 6 的 `TabResponse` 新字段与 `ack_tab_notice` 命令。
- Produces(Task 8 依赖):`Tab` 类型带 `status: TabStatus | null` 与 `notice: TabNotice | null`;导出的类型名 `TabStatus`(判别联合,`kind: 'thinking' | 'tool' | 'done'`)与 `TabNotice`。

- [ ] **Step 1: types.ts**

1a. 在 `Tab` 接口**之前**加:

```ts
/**
 * Turn state reported via the OSC 7777 hook protocol. `kind` discriminates;
 * all timestamps are epoch milliseconds from the backend's clock. Mirrors
 * `TabStatus` in `src-tauri/src/status.rs`.
 */
export type TabStatus =
  | { kind: 'thinking'; since: number }
  | { kind: 'tool'; name: string; since: number }
  | { kind: 'done'; duration: number | null; at: number };

/** A Notification-hook message awaiting the user. Mirrors `Notice` in Rust. */
export interface TabNotice {
  msg: string | null;
  at: number;
}
```

1b. `Tab` 与 `TabResponse` 接口各加两个字段(`flashing` 之前 / 末尾):

```ts
  /** Hook-protocol turn state; null until this tab's session speaks it. */
  status: TabStatus | null;
  /** Notification awaiting the user; null once seen or superseded. */
  notice: TabNotice | null;
```

1c. 在 `TabFlashPayload` 附近加:

```ts
/** Full replacement state for one tab's protocol fields. */
export interface TabStatusPayload {
  tab_id: string;
  status: TabStatus | null;
  notice: TabNotice | null;
}
```

注意:`toTab`(`useTabManager.ts` 里的 `{ ...response, flashing: false }`)**无需改动**——response 现在自带 `status`/`notice`,展开即得。

- [ ] **Step 2: useTabManager.ts — import**

`from '../types'` 的 import 列表加 `type TabStatusPayload`。

- [ ] **Step 3: useTabManager.ts — switchTab 带 ack**

整体替换现有 `switchTab`:

```ts
  const switchTab = useCallback((tabId: string) => {
    setActiveTabId(tabId);
    // Seeing the tab acknowledges its notification: clear it here and in the
    // registry, so a webview reload cannot resurrect an already-seen notice.
    if (tabsRef.current.some((tab) => tab.id === tabId && tab.notice)) {
      void invoke('ack_tab_notice', { tabId }).catch(() => {
        /* the tab may already be gone */
      });
    }
    setTabs((prev) =>
      prev.map((tab) => (tab.id === tabId ? { ...tab, flashing: false, notice: null } : tab))
    );
  }, []);
```

(相比现状只多了 ack;原有的 flashing 清零保留。)

- [ ] **Step 4: useTabManager.ts — 监听 tab-status**

在 `listen<TabCwdPayload>('tab-cwd', ...)` 条目之后加:

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
      }),
```

- [ ] **Step 5: typecheck**

Run: `npm run typecheck`
Expected: 0 errors。若 `toTab` 报缺字段,检查 Step 1b 是否两处(Tab、TabResponse)都加了。

- [ ] **Step 6: 提交**

```bash
git add src/types.ts src/hooks/useTabManager.ts
git commit -m "feat: carry hook-protocol status through the tab store and ack notices"
```

---

### Task 8: TabItem 状态行 + CSS

**Files:**
- Modify: `src/components/TabItem.tsx`(新助手函数 `formatDuration`、组件内两个 effect、状态行 JSX)
- Modify: `src/App.css`(`.tab-status` 三条规则,加在 `.tab-cwd` 规则之后)
- Test: `npm run typecheck` + Task 10 手动验证

**Interfaces:**
- Consumes: Task 7 的 `Tab.status` / `Tab.notice`(类型 `TabStatus`/`TabNotice`)。
- Produces: 无(叶子组件)。`TabList.tsx`、`App.tsx` **零改动**。

- [ ] **Step 1: TabItem.tsx — import 与常量**

1a. 首行 React import 加 `useEffect`:

```ts
import React, { useEffect, useLayoutEffect, useRef, useState } from 'react';
```

1b. 常量区(`CHROME_WIDTH` 之后)加:

```ts
/** The done line lingers this long, then fades (see .tab-status.done-hidden). */
const DONE_FADE_MS = 6000;
```

1c. 在 `shortenPath` 之后加(同样 export,与 shortenPath 一致):

```ts
/**
 * Human duration for the status line: `42s`, `1m 05s`, `1h 02m`. Negative
 * input (clock skew between the backend timestamp and Date.now) clamps to 0.
 */
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (hours > 0) return `${hours}h ${String(minutes).padStart(2, '0')}m`;
  if (minutes > 0) return `${minutes}m ${String(seconds).padStart(2, '0')}s`;
  return `${seconds}s`;
}
```

- [ ] **Step 2: TabItem.tsx — 组件内状态与 effect**

在组件体内、现有 `useLayoutEffect`(标题截断)**之后**加:

```tsx
  // Live elapsed timer: only mounted while a turn is actually ticking, so
  // idle tabs cost nothing.
  const ticking = tab.status?.kind === 'thinking' || tab.status?.kind === 'tool';
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!ticking) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [ticking]);

  // The done line fades out a few seconds after the turn ended; any newer
  // status (or a new done with a different `at`) cancels the fade.
  const doneAt = tab.status?.kind === 'done' ? tab.status.at : null;
  const [doneFaded, setDoneFaded] = useState(false);
  useEffect(() => {
    setDoneFaded(false);
    if (doneAt === null) return;
    const timer = setTimeout(() => setDoneFaded(true), DONE_FADE_MS);
    return () => clearTimeout(timer);
  }, [doneAt]);
```

- [ ] **Step 3: TabItem.tsx — 状态行文本与 JSX**

3a. 在 `badge` 计算之前加:

```tsx
  // Status-line content. A pending notification outranks the turn state; the
  // row itself exists only once this tab has spoken the protocol, so plain
  // shell tabs keep their exact current layout.
  const hasStatusRow = tab.status !== null || tab.notice !== null;
  let statusText = '';
  if (tab.notice) {
    statusText = `⚠ ${tab.notice.msg ?? 'Claude needs attention'}`;
  } else if (tab.status?.kind === 'tool') {
    statusText = `⚙ ${tab.status.name} · ${formatDuration(now - tab.status.since)}`;
  } else if (tab.status?.kind === 'thinking') {
    statusText = `⏳ ${formatDuration(now - tab.status.since)}`;
  } else if (tab.status?.kind === 'done') {
    statusText = `✓ ${tab.status.duration != null ? formatDuration(tab.status.duration) : 'done'}`;
  }
```

3b. JSX:在 `<span className="tab-title" ...>` 之后、`{showCwd && ...}` 之前插入:

```tsx
        {hasStatusRow && (
          <span
            className={`tab-status${tab.notice ? ' notice' : ''}${doneFaded ? ' done-hidden' : ''}`}
          >
            {statusText}
          </span>
        )}
```

(行容器一旦出现就常驻——`doneFaded` 只把 opacity 降到 0,保留行高,避免下方 cwd 行跳动。)

- [ ] **Step 4: App.css**

在 `.tab-cwd` 规则块之后加:

```css
/* Third line: the Claude turn dashboard (OSC 7777 hook protocol). Rendered
   only once a tab reports protocol state; fixed height, so the done line's
   fade-out never shifts the rows below it. */
.tab-status {
  font-size: 11px;
  line-height: 14px;
  min-height: 14px;
  color: #a6adc8;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  min-width: 0;
  font-variant-numeric: tabular-nums;
  transition: opacity 0.6s ease;
}

.tab-status.done-hidden {
  opacity: 0;
}

/* A pending notification outranks the turn state and asks for attention. */
.tab-status.notice {
  color: #f9e2af;
}
```

- [ ] **Step 5: typecheck**

Run: `npm run typecheck`
Expected: 0 errors。

- [ ] **Step 6: 提交**

```bash
git add src/components/TabItem.tsx src/App.css
git commit -m "feat: render the turn dashboard line on each tab"
```

---

### Task 9: 文档 — CLAUDE_HOOKS.md 重写 + 双 README 特性条目

**Files:**
- Rewrite: `CLAUDE_HOOKS.md`
- Modify: `README.md`(Features 列表,"Attention flashing" 条目之后)
- Modify: `README.zh-CN.md`(特性列表,"关注闪烁" 条目之后,与英文同步)
- Test: 人工读一遍;JSON 块用 `jq . <<< '...'` 验证可解析

**Interfaces:**
- Consumes: Task 1 探针结论(若探针显示 jq 不可用场景有出入,以探针为准调整措辞)。
- Produces: 用户可复制粘贴的最终 hooks 配置(下面给出全文,逐字使用)。

- [ ] **Step 1: 用以下全文替换 CLAUDE_HOOKS.md**

````markdown
# Claude Code ↔ clitab integration

clitab understands a private OSC protocol (**OSC 7777**) that Claude Code hooks
use to turn each tab into a small dashboard: the tool currently running with a
live timer, the last turn's duration, and notification messages waiting for
you. The same hooks also drive the attention flash. Other terminals (iTerm2,
Terminal.app) silently ignore these sequences, so this config is safe to keep
in your global settings.

## Setup

[jq](https://jqlang.github.io/jq/) is recommended: it powers tool names and
notification text. Without jq everything degrades gracefully — turn start/stop
and flashing still work, only the detail text is missing.

Add this to `~/.claude/settings.json` (or a project's `.claude/settings.json`):

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "printf '\\033]7777;{\"e\":\"prompt\"}\\033\\\\' >/dev/tty 2>/dev/null || true"
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "j=$(jq -c '{e:\"tool\",tool:.tool_name}' 2>/dev/null); [ -n \"$j\" ] && printf '\\033]7777;%s\\033\\\\' \"$j\" >/dev/tty 2>/dev/null; true"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "printf '\\033]7777;{\"e\":\"stop\"}\\033\\\\' >/dev/tty 2>/dev/null || true"
          }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "j=$(jq -c '{e:\"notify\",msg:.message}' 2>/dev/null || printf '{\"e\":\"notify\"}'); printf '\\033]7777;%s\\033\\\\' \"$j\" >/dev/tty 2>/dev/null || true"
          }
        ]
      }
    ]
  }
}
```

## How it works

- Each hook `printf`s one JSON payload wrapped in `ESC ] 7777 ; … ESC \` to
  **`/dev/tty`** — the tab's PTY. Not stdout: Claude Code parses some hooks'
  stdout as decision JSON, and dashboard bytes there would be noise at best.
- The PTY itself routes the event to the right tab, so hooks need no tab id.
- Events: `{"e":"prompt"}` turn start · `{"e":"tool","tool":"Bash"}` running a
  tool · `{"e":"stop"}` turn end (clitab computes the duration) ·
  `{"e":"notify","msg":"…"}` needs attention — flashes the tab and shows the
  message until you switch to it.
- Every command ends in `|| true` / `; true`: a failing hook must never block
  Claude Code. Missing jq produces an empty payload, which clitab ignores.

## Legacy configs keep working

The older setup — a `Notification` hook running `printf '\a'` (BEL flash) — is
still supported, as is the shell-integration `OSC 9;claude-done` title revert.
The new `Notification` hook above **replaces** the BEL one (clitab flashes on
`notify` itself); remove the old entry to avoid a double flash.

## Troubleshooting

1. **Dashboard never appears** — test the protocol directly, without Claude:
   in a clitab tab run
   `printf '\033]7777;{"e":"tool","tool":"Test"}\033\\'`
   The tab's third line should read `⚙ Test · 0s`. If it does, the protocol
   works and the hook config is the problem — validate your settings JSON
   (`jq . ~/.claude/settings.json`).
2. **Timer/duration works but no tool names or notice text** — jq is missing;
   install it (`brew install jq`) or accept the degraded display.
3. **Manual printf works but hooks don't** — Claude Code may be older than
   v1.0.24 (no hooks), or your setup detaches hooks from the controlling
   terminal. Check `claude --version` and file an issue with it.
````

- [ ] **Step 2: 验证文档里的 JSON 块可解析**

把 Step 1 的 json 代码块内容存到临时文件后:

```bash
jq -e '.hooks | keys' /tmp/hooks-block.json
```

Expected: 输出含 `"Notification"`, `"PreToolUse"`, `"Stop"`, `"UserPromptSubmit"` 四个键;再把每条 `command` 字符串拷进 shell 跑一次(`echo '{"tool_name":"Bash"}' | <command>` 形式),确认退出码 0。

- [ ] **Step 3: README.md 特性条目**

在 "Attention flashing" 条目之后插入:

```markdown
- **Tab dashboard** — with the optional Claude Code hooks (`CLAUDE_HOOKS.md`),
  each tab shows what its session is doing: the running tool with a live
  timer, the last turn's duration, and notifications waiting for you. Built on
  a private OSC 7777 protocol other terminals simply ignore.
```

- [ ] **Step 4: README.zh-CN.md 同步条目**

在 "关注闪烁" 条目之后插入:

```markdown
- **标签仪表盘** — 配置可选的 Claude Code hooks(见 `CLAUDE_HOOKS.md`)后,
  每个标签会显示会话正在做什么:当前工具与实时计时、上一回合耗时、
  等待处理的通知。基于私有 OSC 7777 协议,其他终端会静默忽略。
```

- [ ] **Step 5: 提交**

```bash
git add CLAUDE_HOOKS.md README.md README.zh-CN.md
git commit -m "docs: hook protocol setup for the tab dashboard"
```

---

### Task 10: 端到端手动验证

**Files:** 无新增;发现问题就地修复并补提交。

**Interfaces:**
- Consumes: 前九个任务的全部产出。
- Produces: 验证记录(每条协议事件 → 预期 UI 行为)。

- [ ] **Step 1: 全量自动检查**

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib
npm run typecheck
npm run build
```

Expected: 三项全部通过。

- [ ] **Step 2: 启动 dev 应用**

```bash
npm run dev:log   # tee 到 /tmp/clitab-dev.log,便于回报 bug
```

- [ ] **Step 3: 协议层直测(不依赖 claude)**

在 dev 应用的一个标签里逐条运行,对照预期:

```bash
printf '\033]7777;{"e":"prompt"}\033\\'
# 预期:第三行出现「⏳ 0s」并每秒跳动
sleep 3; printf '\033]7777;{"e":"tool","tool":"Bash"}\033\\'
# 预期:第三行变「⚙ Bash · 0s」,重新计时、持续跳动
sleep 5; printf '\033]7777;{"e":"stop"}\033\\'
# 预期:第三行变「✓ 8s」,约 6 秒后淡出(行高保留);非活动标签会闪烁
printf '\033]7777;{"e":"notify","msg":"Claude needs your permission"}\033\\'
# 预期:第三行变黄色「⚠ Claude needs your permission」;标签闪烁
```

- [ ] **Step 4: 交叉行为**

- 在**标签 B** 里 `(sleep 5; printf '\033]7777;{"e":"notify"}\033\\') &` 后停留在标签 A → 5 秒后 B 闪烁、B 第三行显示通用「⚠ Claude needs attention」(无 msg 降级路径);切到 B → 通知立即消失,且不再复发。
- 在标签 B 重跑 Step 3 的 prompt+tool,停在「⚙ Bash · Ns」时按 **⌘R 重载 webview** → 重载后第三行恢复为 tool 状态且耗时基于 epoch 时间戳继续正确增长(不从头计)。
- 纯 shell 标签(从未发过协议事件)→ 布局与改动前完全一致,只有标题/cwd 两行。
- `printf '\033]7777;{"e":"bogus"}\033\\'` 与 `printf '\033]7777;garbage\033\\'` → 无任何 UI 变化、应用不崩。

- [ ] **Step 5: 真实 Claude Code 会话**

把 CLAUDE_HOOKS.md 的 hooks 配置写入一个临时项目的 `.claude/settings.json`,在 clitab 标签里 `cd` 过去跑 `claude`,让它执行一个 Bash 命令:观察 ⏳ → ⚙ Bash → ✓ 耗时 全链路;触发一次权限询问观察 notify。结束后删除临时配置。

- [ ] **Step 6: 回归旧行为**

- 标签里跑 `claude` 后正常退出 → 标题仍从会话名回退到目录(`claude-done` 路径未破坏)。
- 未配置新 hooks、仅旧 `printf '\a'` Notification hook → 闪烁照旧。

- [ ] **Step 7: 收尾提交(如有修复)**

```bash
git add -A && git commit -m "fix: <具体问题>"
```

全绿后报告用户,等待分支收尾指示(finishing-a-development-branch)。
