# clitab

[English README](README.md)

为 Claude Code 会话打造的多标签终端:每个标签都是一个运行你 shell 的真实
PTY,以工作目录命名 —— 当 Claude Code 为会话设置标题后,则显示会话名。
Claude Code 需要你关注时标签会闪烁,一排并行 agent 的状态一目了然。

基于 [Tauri 2](https://tauri.app)(Rust + portable-pty)与
[xterm.js](https://xtermjs.org) 构建。

## 特性

- **真实 PTY 标签** — 每个标签通过独立的伪终端启动你的 `$SHELL`;
  隐藏时保活,尺寸保持不变。
- **标签自动命名** — 标题即工作目录(由 shell 集成钩子在每次提示符时上报),
  Claude Code 设置终端标题后切换为会话名;助手回合结束后恢复为目录名。
- **关注闪烁** — Claude Code 请求输入或发送通知时(BEL / OSC 9)标签闪烁。
  一次性钩子配置见 [CLAUDE_HOOKS.md](CLAUDE_HOOKS.md)。
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
`aarch64`,Intel 选 `x64`——把 **clitab** 拖入应用程序。

发布版做了 ad-hoc 签名但**没有 Apple 公证**,而下载来的文件带有隔离属性,
所以全新下载时 macOS 的 Gatekeeper 会拦两道:`.dmg` 一道、应用一道,
各需一次**仍要打开**:

1. 双击 `.dmg`,macOS 弹窗拒绝:提示文件"无法打开"/没有权限打开,
   点**好**;
2. 打开**系统设置 → 隐私与安全性**,向下滚动到**安全性**区域:
   "已阻止'clitab_0.1.0_….dmg'以保护你的 Mac"旁边是**仍要打开**按钮,
   点击并按提示输入密码(或 Touch ID)确认,镜像随即正常挂载;
3. 把 **clitab** 拖入应用程序。首次打开时应用本身会被同样拦截:
   回到**隐私与安全性**,在"已阻止'clitab'…"旁点**仍要打开**。
   均为一次性操作,之后可正常打开。

终端捷径(两道弹窗都跳过):在打开**之前**先清掉下载来的 `.dmg` 的隔离
属性,从它拷贝出去的应用就不带该属性:

```bash
xattr -d com.apple.quarantine ~/Downloads/clitab_0.1.0_*.dmg
```

(macOS 15 Sequoia 起,"右键 → 打开"对这种签名不再能绕过 Gatekeeper,
请以系统设置里的**仍要打开**为准。)

从源码构建(`npm run package:macos`)不需要上述步骤:产物同样是 ad-hoc
签名但从不带隔离属性,可以直接打开。

两种构建均为对应芯片原生运行,无需 Rosetta。

## Claude Code 集成

在任意标签里像普通终端一样运行 `claude` 即可 —— clitab 会从 Claude Code
本来就会发出的转义序列中捕获会话标题。

要启用关注闪烁,在 Claude Code 设置(`~/.claude/settings.json`)里加一个
Notification 钩子,让终端响铃:

```json
{
  "hooks": {
    "Notification": [
      {
        "matcher": ".*",
        "hooks": [{ "type": "command", "command": "printf '\\a'" }]
      }
    ]
  }
}
```

细节与排错见 [CLAUDE_HOOKS.md](CLAUDE_HOOKS.md)。

## 快捷键

定义在原生菜单(`src-tauri/src/menu.rs`)中,终端没有键盘焦点时也生效。

| 按键 | 动作 |
| --- | --- |
| `⌘T` | 新建标签 |
| `⌘W` | 关闭标签(仍有进程运行时先询问) |
| `⌃Tab` / `⌃⇧Tab` | 下一个 / 上一个标签 |
| `⌘1` … `⌘8` | 跳转到第 1–8 个标签 |
| `⌘9` | 跳转到最后一个标签 |
| `⌘C` / `⌘V` / `⌘A` | 复制 / 粘贴 / 全选 |

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
