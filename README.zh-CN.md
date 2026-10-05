# clitab

[English README](README.md) · 📖 [使用手册 (Wiki)](https://github.com/dongritengfei/clitab/wiki/%E6%89%8B%E5%86%8C)

为 Claude Code 会话打造的多标签终端:每个标签都是一个运行你 shell 的真实
PTY,以工作目录命名 —— 当 Claude Code 为会话设置标题后,则显示会话名。
Claude Code 需要你关注时标签会闪烁,一排并行 agent 的状态一目了然。

基于 [Tauri 2](https://tauri.app)(Rust + portable-pty)与
[xterm.js](https://xtermjs.org) 构建。

## 截图

![clitab 整体界面:左侧标签列表、中间运行中的 Claude Code 会话及其仪表盘、右侧会话时间线面板](docs/screenshots/overview.jpg)

左侧标签列表以工作目录或 Claude Code 会话名命名每个标签,回合运行时带实时
计时;中间是会话本身;右侧时间线面板记录你发送的内容。

## 特性

- **真实 PTY 标签** — 每个标签通过独立的伪终端启动你的 `$SHELL`;
  隐藏时保活,尺寸保持不变。新建标签在当前标签的工作目录打开。
- **从 Finder 打开** — 在 Finder 中右键文件夹,选择 服务 → “New clitab Tab Here”,
  即可在该目录打开标签。对文件(打开其所在目录)和 Finder 窗口空白处
  (打开窗口目录)同样有效,clitab 是否已在运行均可。
- **标签自动命名** — 标题即工作目录(由 shell 集成钩子在每次提示符时上报),
  Claude Code 设置终端标题后切换为会话名;助手回合结束后恢复为目录名。
- **关注闪烁** — Claude Code 请求输入或发送通知时(BEL / OSC 9)标签闪烁。
  一次性钩子配置见 [CLAUDE_HOOKS.md](CLAUDE_HOOKS.md)。
- **关注分诊** — 需要你输入的会话进入等待队列:Dock 角标计数,⌘J 跳到
  下一个;clitab 在后台时每次进入等待弹一条 macOS 通知,点击通知直达
  对应标签。在该标签敲键盘才离开队列——仅切换不清除。
- **标签仪表盘** — 配置可选的 Claude Code hooks(见 `CLAUDE_HOOKS.md`)后,
  每个标签会显示会话正在做什么:当前工具与实时计时、上一回合耗时、
  等待处理的通知。基于私有 OSC 7777 协议,其他终端会静默忽略。
- **会话时间轴** — 右侧面板按时间记录你在当前标签 Claude Code 会话中发送的内容:
  每次提交的提示词(最多两行,悬停查看全文)和你对选择题的回答。
  点击条目即把终端滚动到当时的输出位置;已滚出
  5000 行历史缓冲的条目会置灰。与仪表盘共用同一套 hooks。
- **终端内搜索** — ⌘F 在当前标签的屏幕与回滚缓冲中查找文本:输入即高亮
  匹配,Enter / Shift+Enter 循环跳转,Esc 关闭并把光标还给终端。
- **桌面级快捷键** — 原生菜单驱动 ⌘T / ⌘W / ⌃Tab / ⌘1–9,
  即使终端没有键盘焦点也能生效。关闭仍有进程在跑的标签时会先询问。
- **点击标签即刻输入** — 激活标签会把光标送进对应终端,无需再点一次。
- **窗口重载不丢内容** — 每个标签最近 256 KB 输出保存在环形缓冲区,
  重新挂载时回放。重绘式 TUI 输出(Claude Code、vim 等)则干净重启,
  避免鬼影错位。
- **不打扰终端的 shell 集成** — bash 走 `--rcfile` 包装,zsh 走 `ZDOTDIR`
  包装并链接你自己的启动文件,原有配置原样加载。每标签的集成文件放在
  `$TMPDIR` 下,随会话删除。
- 5000 行回滚缓冲、⌘C/⌘V/⌘A 复制粘贴、触控板"轻触后拖动"不会误选中文字。

## 安装 (macOS)

从 [Releases](../../releases) 按芯片下载 `.dmg`——Apple Silicon(M1–M4)选
`aarch64`,Intel 选 `x64`——把 **clitab** 拖入应用程序。发布版为 ad-hoc 签名
但**未公证**,全新下载会被 Gatekeeper 拦两道;[使用手册](https://github.com/dongritengfei/clitab/wiki/%E6%89%8B%E5%86%8C#2-%E5%AE%89%E8%A3%85%E4%B8%8E%E9%A6%96%E6%AC%A1%E5%90%AF%E5%8A%A8)里有两次一次性**仍要打开**的完整步骤,以及跳过两道弹窗的终端捷径。从源码构建(`npm run package:macos`)无需这些步骤。两种构建均为对应芯片原生运行,无需 Rosetta。

## Claude Code 集成

在任意标签里像普通终端一样运行 `claude` 即可 —— clitab 会从 Claude Code
本来就会发出的转义序列中捕获会话标题。

关注闪烁、标签仪表盘与会话时间轴由 Claude Code hooks 驱动。[使用手册](https://github.com/dongritengfei/clitab/wiki/%E6%89%8B%E5%86%8C#hooks-%E5%A2%9E%E5%BC%BA%E7%9A%84%E9%83%A8%E5%88%86)里有一段可整段复制粘贴的提示词,替你把 hooks 合并进 `~/.claude/settings.json` 并完成校验;[CLAUDE_HOOKS.md](CLAUDE_HOOKS.md) 提供原始 JSON、协议原理与排错。配置写入后重启 Claude Code。

## 快捷键

标签快捷键定义在原生菜单(`src-tauri/src/menu.rs`)中,终端没有键盘焦点时也生效。
`⌘C` / `⌘V` / `⌘A` 是 macOS 标准的"编辑"菜单项,作用于当前选中的文本。

| 按键 | 动作 |
| --- | --- |
| `⌘T` | 新建标签 |
| `⌘W` | 关闭标签(仍有进程运行时先询问) |
| `⌃Tab` / `⌃⇧Tab` | 下一个 / 上一个标签 |
| `⌘J` | 跳到下一个等待输入的标签 |
| `⌘1` … `⌘8` | 跳转到第 1–8 个标签 |
| `⌘9` | 跳转到最后一个标签 |
| `⌘F` | 搜索当前终端(Enter / ⇧Enter 循环匹配,Esc 关闭) |
| `⌘C` / `⌘V` / `⌘A` | 复制 / 粘贴 / 全选 |

> 通知仅在 clitab 位于后台时弹出。若从未出现,请检查 系统设置 → 通知 → clitab。

在标签列表内,方向键 / Home / End 可在标签间移动。

## 开发

```bash
npm install
npm run tauri dev     # vite + tauri 开发构建
npm run dev:log       # 同上,输出 tee 到 /tmp/clitab-dev.log 便于报告问题
npm run tauri build   # 发布打包
```

检查与测试:

```bash
npm run typecheck   # tsc --noEmit
npm run build       # typecheck + vite build
npm test            # cargo test --lib:OSC 解析器、注册表、shell 集成
```

## 安全

- **CSP** 配置在 `tauri.conf.json`,只作用于*打包后*的应用:开发模式页面由
  Vite 提供,Tauri 只对自己内嵌的资源应用该策略。两处配置是关键 ——
  `connect-src ipc: http://ipc.localhost` 是 `invoke()` 和事件到达 Rust 的
  通道;`style-src 'unsafe-inline'` 是 xterm.js 运行时创建 `<style>` 元素所
  必需。如果 IPC 失效,先检查前者。
- **Shell 集成文件**放在 `$TMPDIR` 下每标签独立的目录
  (`clitab-<tab-id>`),绝不使用固定共享路径,并随会话删除。
- **Capabilities** 在 `src-tauri/capabilities/default.json`;其中的 `windows`
  列表必须与配置里的 `app.windows[].label` 一致。

## 许可

MIT,见 [LICENSE](LICENSE)。
