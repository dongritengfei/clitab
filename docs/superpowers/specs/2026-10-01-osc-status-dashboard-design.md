# 设计:Hook → 自定义 OSC 结构化上报(标签仪表盘)

日期:2026-10-01
状态:已与用户逐节确认

## 目标

把现有 `OSC 9;claude-done` 单点信号扩展成一个小型结构化协议:Claude Code 的
hooks(UserPromptSubmit / PreToolUse / Stop / Notification)向 PTY printf 带
JSON 的 OSC 序列;clitab 解析后把状态送到前端,标签页从"标题 + 目录"进化成
"仪表盘":当前正在执行什么、本回合耗时、需要注意的通知消息。

协议只有 clitab 认识;iTerm2 等其他终端对未知 OSC 码静默丢弃,这是协议层的
独占优势。

**v1 范围**:状态 + 耗时。不含 token/费用(hook stdin 不提供,需要解析
transcript JSONL,留给 v2;本协议的 JSON 载荷天然向前兼容)。

## 第 1 节:协议规范

帧格式:`ESC ] 7777 ; <JSON> ESC \`(ST 结尾;沿用现有解析器对 BEL 结尾的
支持)。

- OSC 码 **7777**:未被主流终端注册,不与 OSC 9(iTerm2 通知)撞车。
- 载荷是单个 JSON 对象,UTF-8,受现有 `MAX_OSC_LEN`(4096)保护。
- **分号安全**:现有解析器按 `;` 切分参数,JSON 字符串值里可能含 `;`。
  规则:对 7777 码,第一个 `;` 之后的所有参数用 `;` 重新拼回作为完整
  JSON。只改 `interpret()` 一处。

事件集(`e` 字段判别):

| 事件 | hook 来源 | JSON | 语义 |
|---|---|---|---|
| `prompt` | UserPromptSubmit | `{"e":"prompt"}` | 回合开始,Rust 记 `turn_start` |
| `tool` | PreToolUse | `{"e":"tool","tool":"Bash"}` | 正在执行某工具 |
| `stop` | Stop | `{"e":"stop"}` | 回合结束,耗时 = now − turn_start(Rust 计算,hook 不传数字) |
| `notify` | Notification | `{"e":"notify","msg":"…"}` | 需要用户关注;`msg` **可选**(jq 缺失时降级为无 msg,仍闪烁,状态行显示通用"需要关注") |

兼容性规则:未知 `e` 值、JSON 解析失败、缺必需字段(`tool` 缺 `tool`)→
静默忽略,无任何副作用。

明确不改的:`OSC 9;claude-done` 保持原语义(claude 进程退出 → 标题回退
cwd)。`stop` 事件**不**触发标题回退、不碰 `program_active`——回合结束时
Claude Code 还活着。

## 第 2 节:Rust 后端

分层原则:`osc.rs` 只管字节帧,JSON 语义放新模块。

1. **`osc.rs`**:`OscEvent` 加变体 `Clitab(String)` 携带原始 JSON 文本;
   `interpret()` 对 code `"7777"` 把 `params[1..]` 用 `;` 拼回。不引入
   serde,保持纯传输层。

2. **新模块 `status.rs`**:
   - `StatusEvent`(协议解码结果):`Prompt` / `Tool { name }` / `Stop` /
     `Notify { msg: Option<String> }`;解码失败返回 `None`。
   - `TabStatus`(registry 存储的展示状态),时间戳一律 **epoch 毫秒**
     (不用 `Instant`:要跨 webview 重载后仍能算耗时):
     `Idle | Thinking { since } | Tool { name, since } | Done { duration: Option<u64>, at }`。

3. **`registry.rs`**:`TabRecord` 加 `status: TabStatus`、
   `notice: Option<Notice { msg: Option<String>, at }>`、
   `turn_start: Option<u64>`。放 registry 的理由与标题相同:reader 线程
   直写、锁序简单、`list_tabs` 重载后仪表盘不丢。
   迁移方法:`begin_turn` / `set_tool` / `end_turn`(算 duration;
   turn_start 缺失则 None;算完把 turn_start 清回 None)/ `set_notice` /
   `clear_notice`。
   **notice 与 status 正交**(设计修正):通知不覆盖回合状态,notice 清除
   后回到原状态。`prompt`/`tool`/`stop` 任一事件到达都清 notice。

4. **`session.rs` `handle_osc`**:`Clitab(json)` → `status.rs` 解码 →
   registry 迁移 → 发新事件 **`tab-status`**(payload `{tab_id, status,
   notice}`,序列化为扁平 JSON,如 `{kind:"tool", tool:"Bash", since:…}`)。
   - `Stop` → 立即发 `tab-flash`(不等 2 秒静默 watcher);watcher 线程的
     `flashed` 标志改为共享 `Arc<AtomicBool>`,Stop 置位防重复闪烁。
   - `Notify` → 发 `tab-flash` + `tab-status`。
   - `Stop` 不碰标题 / `program_active`。

5. **测试**(`cargo test --lib`,纯单元):解析器 rejoin(含 `;` 的 JSON)、
   解码容错(坏 JSON / 未知 e / 缺字段 / notify 无 msg)、registry 迁移 +
   duration 计算(有/无 turn_start)、notice 与 status 正交、`list_tabs`
   序列化含 status。

## 第 3 节:前端

1. **`types.ts`**:`Tab`、`TabResponse` 加 `status`(可空)、`notice`
   (可空);新增 `TabStatusPayload`。
2. **`useTabManager.ts`**:监听 `tab-status` 更新对应 tab;`activateTab`
   时若该 tab 有 notice → 本地清掉 + `invoke('ack_tab_notice')`(唯一新
   IPC 命令,写回 registry,防重载后旧通知复活)。
3. **`TabItem.tsx`**:标题和 cwd 之间插入状态行,**仅在该 tab 有过协议
   状态后渲染**(纯 shell 标签布局不变;一旦渲染固定高度,内容淡出后留
   空行,避免布局抖动):
   - `Tool` → `⚙ Bash · 23s`,每秒跳动(TabItem 本地 `setInterval`,仅
     Thinking/Tool 时挂载);
   - `Thinking` → `⏳ 12s`;
   - `Done` → `✓ 1m 42s`,约 6 秒后 CSS 淡出(定时器在 TabItem,新状态
     到达即取消);
   - `notice` 存在时**优先**显示 `⚠ <消息>`(无 msg 显示通用文案),高亮,
     直到被 ack。
4. **`App.css`**:状态行样式、淡出过渡、notice 高亮色。

不做(YAGNI):tooltip 展开、历史回合列表、`Done` 重载后不重新淡出
(可接受的小瑕疵)。

验证:`npm run typecheck` + `npm run tauri dev` 手动过四个事件的显示
(前端无测试运行器)。

## 第 4 节:Hook 配置与文档

四个 hook 命令,全部写 `/dev/tty`、全部 `2>/dev/null || true` 兜底
(hook 失败绝不阻塞 Claude Code):

| Hook | 命令要点 | jq |
|---|---|---|
| UserPromptSubmit | 静态 `printf '\033]7777;{"e":"prompt"}\033\\' >/dev/tty` | 不需要 |
| Stop | 静态 `{"e":"stop"}`,同上 | 不需要 |
| PreToolUse | `jq -c '{e:"tool",tool:.tool_name}'` 读 stdin,结果塞进 printf;jq 缺失/坏 JSON → 空载荷 → Rust 静默忽略 | 需要,可降级 |
| Notification | `jq -c '{e:"notify",msg:.message}'`;jq 缺失降级发 `{"e":"notify"}`(无 msg) | 需要,可降级 |

新 Notification hook 直接替换旧的 `printf '\a'` 条目;闪烁改由 Rust 收到
notify 时触发。

文档:
- `CLAUDE_HOOKS.md` 重写:完整 settings.json 四 hook 配置块、jq 可选
  说明、`/dev/tty` 排障;明确旧配置(BEL / `claude-done` OSC 9)继续有效。
- `README.md` + `README.zh-CN.md`:特性区加"标签仪表盘"条目,两份同步。

## 风险与早期探针

实现计划第一步是两个探针,失败则回到设计:

1. **`/dev/tty` 在 hook 执行环境可写**——理论上 hook 继承 Claude Code 的
   控制终端,必须实测。备选:stdout(仅对 Notification/Stop 安全,
   PreToolUse 的 stdout 会被 Claude Code 当 hook 决策 JSON 解析)或
   shell 集成注入环境变量指路。
2. **Stop hook 是否真的触发**——现有 `CLAUDE_HOOKS.md` 末尾"Stop 可能不
   支持 command hooks"与官方文档矛盾,需实测。若真不支持:`stop` 退化,
   耗时由 shell 集成 `claude-done` 兜底(会话级而非回合级,价值大减),
   届时重新讨论。

## 明确不碰

- `shell_integration.rs`(rc 包装器与新协议无关)。
- master 工作区未提交的"新标签继承 cwd"改动(留在原 checkout)。
- 重新实现快捷键 / WebGL 渲染器等 CLAUDE.md 记录的既定选择。
