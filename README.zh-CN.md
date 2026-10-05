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

- **真实 PTY 标签**,以工作目录命名 —— 或其中运行的 Claude Code 会话名。
- **关注闪烁与分诊** —— 标签闪烁、Dock 角标、⌘J 与 macOS 通知,指向等你输入的会话。
- **标签仪表盘与会话时间线** —— 实时工具计时、回合耗时与你发送内容的记录,由可选 hooks 提供。
- **从 Finder 打开** —— 服务 → "New clitab Tab Here"。
- **终端搜索、桌面级快捷键、5000 行回滚缓冲。**

各项用法见[使用手册](https://github.com/dongritengfei/clitab/wiki/%E6%89%8B%E5%86%8C)。

## 安装 (macOS)

从 [Releases](../../releases) 按芯片下载 `.dmg`——Apple Silicon(M1–M4)选
`aarch64`,Intel 选 `x64`——把 **clitab** 拖入应用程序。发布版为 ad-hoc 签名
但**未公证**,全新下载会被 Gatekeeper 拦两道;[使用手册](https://github.com/dongritengfei/clitab/wiki/%E6%89%8B%E5%86%8C#2-%E5%AE%89%E8%A3%85%E4%B8%8E%E9%A6%96%E6%AC%A1%E5%90%AF%E5%8A%A8)里是两次一次性**仍要打开**的完整步骤。

## Claude Code 集成

在任意标签里像普通终端一样运行 `claude` 即可 —— clitab 会从 Claude Code
本来就会发出的转义序列中捕获会话标题。驱动闪烁、仪表盘与时间线的 hooks
(含一段可整段复制粘贴的配置提示词)见[使用手册](https://github.com/dongritengfei/clitab/wiki/%E6%89%8B%E5%86%8C#hooks-%E5%A2%9E%E5%BC%BA%E7%9A%84%E9%83%A8%E5%88%86)。

## 快捷键

⌘T / W / Tab / ⌘J / ⌘1–9 / ⌘F / ⌘C / V / A —— 完整表格及其无需
终端焦点即可生效的原因,见[使用手册](https://github.com/dongritengfei/clitab/wiki/%E6%89%8B%E5%86%8C#11-%E9%94%AE%E7%9B%98%E5%BF%AB%E6%8D%B7%E9%94%AE)。

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

## 许可

MIT,见 [LICENSE](LICENSE)。
