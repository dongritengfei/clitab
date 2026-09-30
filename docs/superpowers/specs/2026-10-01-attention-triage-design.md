# 关注分诊(attention triage)工作流 设计

日期:2026-10-01
状态:已与用户对齐(方案 A;输入才清除;仅失焦弹通知、每标签每次进入等待一条;
⌘J 空队列静默;标签栏加 waiting 圆点)

## 背景与目标

现有"关注闪烁"是一次性的:`tab-flash` 点亮标签,切过去即灭,看过没处理的
会话就此消失。目标是把闪烁升级为**持久的"等待输入"队列**,支撑三个出口:

1. **⌘J** 轮转跳到下一个等待中的标签——巡房变分诊;
2. **Dock 角标** 显示"N 个会话在等你";
3. **macOS 通知**(仅 app 不在前台时),点击聚焦窗口并直达对应标签。

成功标准:

- 任一 flash 信号源(BEL / OSC 9 `claude-done` / 回合空闲 watcher)使标签
  进入等待;在该标签敲任意键才离开等待(切换、注视都不清除);
- ⌘J 从当前标签的下一个位置起环形扫描,跳到第一个等待中的标签;队列为空
  时静默不动;
- 角标数 = 等待中标签总数(含当前标签),webview reload 后依然正确;
- 窗口失焦时,每次 false→true 转移弹一条通知;点击通知 → 窗口聚焦 + 切到
  对应标签;标签已关闭则忽略;
- `npm run typecheck`、`cargo test --lib` 全绿。

## 机制选型

- **通知**:直依赖 `mac-notification-sys`(tauri-plugin-notification 的
  macOS 底层)。已核实 plugin v2.5.0 桌面端"scheduling, grouping and
  action related options are ignored"且**无点击回调**,做不到点击直达;
  而 `mac-notification-sys` 的 `wait_for_click(true)` 使 `send_notification`
  在调用线程阻塞(condvar)直到点击/关闭,返回 `NotificationResponse::Click`
  ——每通知一线程即可把点击路由回 `tab_id`。
- **角标**:tauri 2.11.3 自带 `Window::set_badge_count(Option<i64>)`
  (macOS = NSDockTile badgeLabel),纯 Rust,无需 objc2。
- **排除**:UNUserNotificationCenter + objc2 手写(代码量数倍,dev 裸二进制
  不工作,mac-notification-sys 就是这层的成熟封装);纯前端方案(通知无点击
  回调 + reload 丢队列,两个硬伤)。

## 状态模型:Registry 增加 waiting

`TabRecord` 加 `waiting: bool`(后端权威,与 title 同哲学):

- **置位**点 = 现有三个 `tab-flash` 发射点:
  `OscEvent::Bell`、`OscEvent::PromptReady`、回合空闲 watcher;
- **清除**点:`pty_input` 写成功(该标签收到键盘输入 = 已响应);
  标签关闭/退出随记录移除;
- `Registry::set_waiting(id) -> bool` 仅在 false→true 转移时返回 true
  (连续 BEL 不重复通知);`waiting_count() -> usize` 供角标;
- 现有 `flashing`(renderer 侧、切换即灭)保持不变,只驱动动画;
  `waiting` 是队列,两者语义独立。

## 架构与数据流

```
BEL / OSC 9 / 空闲 watcher(session.rs)
  → attention::enter_waiting(app, registry, tab_id)
      ├─ set_waiting 转移成立?否 → 返回
      ├─ emit "tab-waiting" {tab_id, waiting: true}   → renderer 更新镜像
      ├─ update_badge(main 窗口 set_badge_count)
      └─ 主窗口 !is_focused()? → spawn 线程:
           send_notification(title=标签标题, body="Waiting for input",
                             wait_for_click=true)   ← 阻塞至点击/关闭
           Click → window.set_focus() + emit "focus-tab" {tab_id}
                                                    → renderer switchTab

pty_input 成功(manager.rs write_input)
  → attention::respond(app, registry, tab_id)
      ├─ clear_waiting;emit "tab-waiting" {waiting: false};update_badge

⌘J(menu.rs "next-waiting", CmdOrCtrl+J)
  → menu-shortcut 事件 → renderer 从 activeIdx+1 起环形扫描(不含 active),
    第一个 waiting 的标签 switchTab;没有 → 静默
```

新模块 `src-tauri/src/attention.rs`,公开三个函数:
`enter_waiting` / `respond` / `update_badge`。session.rs 的三个 flash 点
调用 `enter_waiting`(它已持有 app/registry/tab_id);`TabManager::write_input`
成功后调用 `respond`;`close_tab` / `remove_session` 后调用 `update_badge`。

角标计数**含 active 标签**:只有输入才清除,盯着它不回话它就是在等你。

## Renderer / 类型镜像

- `types.ts`:`Tab.waiting`、`TabResponse.waiting`(Rust 侧 camelCase serde
  已对齐)、`TabWaitingPayload {tab_id, waiting}`、`FocusTabPayload {tab_id}`;
- `useTabManager.ts`:
  - `toTab` 带上 waiting;
  - 监听 `tab-waiting` 更新镜像;
  - 监听 `focus-tab` → 镜像中存在才 `switchTab`(标签可能已关);
  - `menu-shortcut` 加 `next-waiting` 分支;
  - `switchTab` 仍只清 flashing,不动 waiting。

## 标签栏 waiting 视觉

`.tab-item.waiting` 在标题前显示**静止的琥珀色小圆点**(约 10 行 CSS +
一个 className);闪烁动画照旧负责"新到"。没有它,⌘J 巡过一圈后"看过没
处理"的标签与普通标签无法区分,队列不可见。

## 依赖与配置

- `Cargo.toml`:`[target.'cfg(target_os = "macos")'.dependencies]
  mac-notification-sys = "0.6"`;
- 非 tauri 插件:`capabilities/default.json`、`Info.plist` 均无需改动;
- 已知限制(与 Finder 服务 spec 同先例):dev 裸二进制下通知归属可能落到
  Finder(bundle 解析回退),完整验证以打包 app 为准;角标与 ⌘J 在 dev 下
  完全可用。

## 测试

Rust 单测(`cargo test --lib`):

- registry:`set_waiting` 二次置位不转移;`clear_waiting` 后可再转移;
  `waiting_count`;remove 清账;
- menu:`is_tab_action("next-waiting")`;
- attention:角标映射纯函数(0 → None,n → Some(n))。

前端无测试跑器:`npm run typecheck` + 手动验收清单:

1. dev:双标签,后台标签 `printf '\a'` → 圆点亮、⌘J 跳过去、角标 1;
2. 在该标签敲一键 → 圆点灭、角标 0;仅切换不敲键 → 圆点仍在;
3. ⌘J 空队列 → 无动作;
4. 打包 app:窗口失焦后触发 → 通知出现;点击 → 聚焦 + 直达标签;
5. reload webview → 角标与圆点状态不丢。

## 文档

- README.md / README.zh-CN.md:特性一节加"关注分诊",快捷键表加 ⌘J,
  通知不显示时到 系统设置 > 通知 检查 clitab 的一句话排查;
- CLAUDE.md:架构节补 `attention.rs`、waiting 不变量(输入才清除)与
  `tab-waiting` / `focus-tab` 事件;
- CLAUDE_HOOKS.md:不变(信号源不变)。

## 边界与风险

- **通知线程生命周期**:每通知一线程阻塞在 condvar,点击/关闭/自动消失
  都会解除;标签关闭后点击 → `focus-tab` 指向不存在的标签 → renderer
  守卫忽略;
- **NSUserNotification 已废弃**(macOS 11+):当前仍工作(tauri 官方插件
  同底层);若未来失效,升级路径是 objc2 + UNUserNotificationCenter;
- **通知授权**:legacy 路径无授权弹窗;用户在系统设置里关掉 clitab 通知
  则不显示,README 排查句覆盖;
- **锁序**:attention 只拿 registry 锁,不进 session map,维持"两锁不同持"
  的既有不变量。
