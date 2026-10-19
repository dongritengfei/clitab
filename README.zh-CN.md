# clitab

[English README](README.md) · 📖 [使用手册 (Wiki)](https://github.com/dongritengfei/clitab/wiki)

为 agent CLI 会话(Claude Code、Qoder CLI)打造的多标签终端:每个标签都是
一个运行你 shell 的真实 PTY,以工作目录命名 —— 当 agent 为会话设置标题后,
则显示会话名。会话需要你关注时标签会闪烁,一排并行 agent 的状态一目了然。

![clitab 整体界面:左侧标签列表、中间运行中的 Claude Code 会话及其仪表盘、右侧会话时间线面板](docs/screenshots/overview.jpg)

基于 [Tauri 2](https://tauri.app)(Rust + portable-pty)与
[xterm.js](https://xtermjs.org) 构建。

## 特性

- **真实 PTY 标签**,以工作目录命名 —— 或其中运行的 agent 会话名
  (Claude Code / Qoder CLI)。`⌘T` 新建标签,`⌘W` 关闭,拖拽排序。
- **标签列表实时展示 agent 状态** —— 哪个会话在工作一眼可见:回合运行时
  标签上带实时计时。
- **直达需要你处理的会话** —— `⌘J`(跳到下一个等待输入的标签);标签闪烁、
  Dock 角标与 macOS 通知为你指路。
- **快速切换** —— `⌃Tab` 循环,`⌘1–9` 直达对应标签。
- **标签仪表盘** —— 实时工具计时、上一回合耗时与待处理通知,由可选的
  agent hooks 提供。
- **会话时间线** —— 记录你提交的每条 prompt 与选择的每个答案,点击条目
  即可让终端滚回当时位置;甚至跨同一标签内的多个 agent 会话存续。
- **从 Finder 打开** —— 服务 → "New clitab Tab Here"。
- **可调布局与字号** —— 拖拽三栏之间的分割线调整宽度(双击复位);
  `⌘=` / `⌘-` / `⌘0` 缩放终端文字。
- **终端搜索** `⌘F`、桌面级快捷键、5000 行回滚缓冲。

## 安装 (macOS)

从 [Releases](../../releases) 按芯片下载 `.dmg` —— Apple Silicon(M1–M4)选
`aarch64`,Intel 选 `x64` —— 把 **clitab** 拖入应用程序。发布版为 ad-hoc
签名但**未公证**,全新下载会被 Gatekeeper 拦两道;完整的一次性**仍要打开**
步骤见[快速上手](https://github.com/dongritengfei/clitab/wiki/Getting-Started.zh)。

## Agent 集成

在任意标签里像普通终端一样运行 `claude` 或 `qodercli` 即可 —— 零配置就能
获得会话标题与关注闪烁。仪表盘、时间线、通知与等待队列由 agent hooks 驱动:
[Agent 配置](https://github.com/dongritengfei/clitab/wiki/Agent-Setup.zh)
提供两个 agent(Claude Code 与 Qoder CLI)各自可整段粘贴的安装提示词。
协议细节(开发者向)见 [AGENT_HOOKS.md](AGENT_HOOKS.md)。

## 文档

[快速上手](https://github.com/dongritengfei/clitab/wiki/Getting-Started.zh) ·
[日常使用](https://github.com/dongritengfei/clitab/wiki/Everyday-Use.zh) ·
[Agent 配置](https://github.com/dongritengfei/clitab/wiki/Agent-Setup.zh) ·
[Agent 功能](https://github.com/dongritengfei/clitab/wiki/Agent-Features.zh) ·
[快捷键](https://github.com/dongritengfei/clitab/wiki/Shortcuts.zh) ·
[故障排除](https://github.com/dongritengfei/clitab/wiki/Troubleshooting.zh)

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
npm test            # cargo test --lib:OSC 解析器、注册表、shell 集成、
                    # 回放环、菜单 id
```

## 许可

MIT,见 [LICENSE](LICENSE)。
