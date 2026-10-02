# 设计:Timeline 摘要与导航(右侧时间轴面板)

日期:2026-10-02
状态:已与用户逐节确认

## 目标

在界面右侧常驻一个时间轴面板,按时间顺序展示**当前活跃 tab** 的关键事件:
回合开始、请求确认(Notification,含消息文本)、回合结束(含耗时)。点击
事件把终端滚动到事件发生时的输出位置,并短暂高亮该行;已被 scrollback
淘汰的事件置灰不可点。

事件全部来自现有 OSC 7777 钩子协议(见 `CLAUDE_HOOKS.md` /
`2026-10-01-osc-status-dashboard-design.md`),**零后端改动**:`tab-status`
事件已携带完整状态与后端权威时间戳,时间轴只是把"瞬时状态"扩展成
"事件历史"。

已确认的决策:
- **粒度**:只记关键事件(prompt / notify / stop);工具调用(PreToolUse)
  不上时间轴。"stop"与"完成"是同一事件(Stop 钩子)。
- **展示**:常驻右侧面板(非可切换、非悬浮)。
- **存储**:仅渲染端。webview 重载后时间轴清空(生产中重载罕见,接受)。
- **导航**:滚动 + 短暂高亮;焦点留在面板,支持连续跳转。
- **绑定机制**:xterm Marker(方案 A)。事件到达时 `registerMarker(0)`
  在当前光标行打标记;marker 自动跟随 scrollback 裁剪,行被淘汰时
  `onDispose` 触发 → 事件自然置灰。不手工簿记行号,不用后端字节 seq。

## 第 1 节:数据模型与事件推导(`src/lib/timeline.ts`)

```ts
export type TimelineEventKind = 'turn-start' | 'notice' | 'turn-end';

export interface TimelineEvent {
  id: number;                  // tab 内递增,唯一
  kind: TimelineEventKind;
  at: number;                  // epoch ms,取自 tab-status 载荷(后端权威)
  msg?: string | null;         // notice 消息文本
  duration?: number | null;    // turn-end 耗时 ms
}
```

**转换检测**:`tab-status` 载荷是完整状态替换(replacement)。timeline 模块
为每个 tab 记住上一次载荷,对比后追加事件:

| 转换 | 追加事件 | 时间戳来源 |
|---|---|---|
| status 变为 `thinking`(此前非 thinking) | `turn-start` | `status.since` |
| notice 从无到有,或 `notice.at` 变化 | `notice`(带 `msg`) | `notice.at` |
| status 变为 `done`(此前非 done) | `turn-end`(带 `duration`) | `status.at` |
| status 在 `tool` 间变化 | 不追加 | — |

规则细节:
- 时间戳一律用载荷里的后端值,不用 `Date.now()`(与 Registry 的 epoch-ms
  惯例一致)。
- 每 tab 事件上限 **500 条**,超出丢最旧的(有界内存)。
- 容忍钩子部分安装:只有 stop 没有 prompt 时,`turn-end` 照常追加,不要求
  事件配对。
- 同一回合多条 notify(每条 `at` 不同)各追加一条。
- notice 的追加信号是 `at` 字段变化而非 null→非 null 转换:ack 只改本地
  tab 状态,不影响 timeline 对载荷序列的检测。

## 第 2 节:终端实例注册表与 Marker 绑定

**`src/lib/termRegistry.ts`**:模块级 `Map<tabId, XTerm>`。`Terminal.tsx`
在 xterm 创建后注册、`dispose` 前注销(挂载/卸载各一行改动)。这是渲染端
唯一一处从 tabId 拿 xterm 实例的地方。

**Marker 创建**:`useTabManager` 现有的 `tab-status` 监听器里,推导出新事件
的同时:

```ts
const marker = termRegistry.get(tabId)?.registerMarker(0);
if (marker) markers.set(eventId, marker);
```

marker 存于独立的每 tab `Map<tabId, Map<eventId, IMarker>>`(eventId 只在
tab 内唯一;非序列化数据,不进 React state——事件列表进 state,marker
查表即可)。

- 终端尚未挂载(attach 完成前)→ 无 marker:事件仍显示,但不可导航。
- 行被 scrollback(5000 行上限)裁剪 → xterm 自动 dispose marker →
  UI 通过 `marker.isDisposed` 置灰该事件。
- **精度说明(设计内误差)**:marker 打在 `tab-status` 到达渲染端那一刻的
  光标行。对应的 `pty-output` 块可能还在写入队列中,位置可能偏差数行。
  对"导航到大致位置"足够;不为此引入字节 seq 映射(复杂度不成比例)。

## 第 3 节:导航与高亮

`navigateToEvent(tabId, eventId)`:

1. 取 term 与 marker;marker 缺失或 `isDisposed` → no-op(UI 已置灰)。
2. `term.scrollToLine(marker.line)` —— marker.line 是当前有效的绝对缓冲
   行号,xterm 自动换算裁剪偏移。
3. 高亮:`term.registerDecoration({ marker, width: term.cols, height: 1,
   backgroundColor: '#585b7066' })`(与现有 selectionBackground 同色,
   Catppuccin surface2 半透明),约 **1.2s** 后 dispose
   (setTimeout;组件卸载/连续点击时清理旧 decoration 与定时器)。
4. 焦点**不**移回终端,留在面板,支持连续跳转。

## 第 4 节:UI(`src/components/TimelinePanel.tsx`)与布局

- `app-container` 变三列:`TabList | terminal-area | TimelinePanel`。
  面板固定宽约 **230px**,常驻。面板是静态布局,挂载后 ResizeObserver
  自动触发一次 PTY resize,无需特殊处理。
- 面板显示**活跃 tab** 的事件;切 tab 即切换列表。
- 时间正序(旧→新),新事件到达时自动滚到底部(仅当用户本来就在底部附近,
  避免打断回看)。
- 每行:`HH:MM:SS`(本地时区)+ 类型色点 + 标签:
  - `turn-start` → "Turn started"
  - `notice` → 消息文本;无消息时降级为 "Needs attention"
  - `turn-end` → "Turn finished · 2m 13s"(耗时格式化;无耗时则省略)
- UI 文案英文,与现有界面一致。
- 不可导航事件:整行置灰(降低不透明度),无点击手势。
- 空状态:"No events yet",附一行指向钩子配置的提示(CLAUDE_HOOKS.md)。
- 样式加在 `App.css`,沿用现有深色主题配色(Catppuccin 系)。

## 第 5 节:错误处理与边界

| 场景 | 行为 |
|---|---|
| tab 关闭 | 丢弃该 tab 的事件列表与全部 marker(marker 随 term.dispose 自动失效) |
| webview 重载 | 时间轴清空(replay ring 重建的缓冲区行号无意义,不尝试恢复) |
| 钩子未安装 | 面板恒为空状态,不影响任何现有功能 |
| `tab-status` 早于终端挂载 | 事件记录、无 marker、置灰 |
| 连续快速点击多个事件 | 每次点击先清理上一个 decoration/定时器再建新的 |

## 第 6 节:测试与文档

- **无前端测试框架**(项目现状),验证手段:
  1. `npm run typecheck`
  2. `npm run tauri dev` + 手动注入:`printf '\033]7777;{"e":"prompt"}\033\\'`
     等序列(见 CLAUDE_HOOKS.md 故障排查节),验证事件出现、时间戳正确、
     点击导航到位、高亮闪现。
  3. 裁剪置灰:注入事件后用大量输出(`seq 1 9000`)撑爆 scrollback,确认
     旧事件置灰。
- Rust 无改动,`cargo test` 不受影响。
- 文档更新:
  - `README.md` / `README.zh-CN.md`:功能说明各加一节(两份同步)。
  - `CLAUDE.md`:前端架构一节补 `timeline.ts` / `termRegistry.ts` /
    `TimelinePanel.tsx` 条目。

## 涉及文件清单

| 文件 | 改动 |
|---|---|
| `src/lib/timeline.ts` | 新增:事件类型、转换检测、每 tab 事件存储 |
| `src/lib/termRegistry.ts` | 新增:tabId → XTerm 注册表 |
| `src/components/TimelinePanel.tsx` | 新增:右侧面板 UI |
| `src/hooks/useTabManager.ts` | tab-status 监听器接入 timeline;暴露 timelines 与 navigateToEvent |
| `src/components/Terminal.tsx` | 挂载/卸载时注册/注销 termRegistry(约 2 行) |
| `src/App.tsx` | 布局加第三列 |
| `src/App.css` | 面板样式 |
| `src/types.ts` | (如需)导出 TimelineEvent 相关类型 |
| `README.md` / `README.zh-CN.md` / `CLAUDE.md` | 文档同步 |
