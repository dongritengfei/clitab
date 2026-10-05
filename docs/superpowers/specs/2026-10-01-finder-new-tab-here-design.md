# Finder "New clitab Tab Here" 设计

日期:2026-10-01
状态:已与用户对齐(方案 A、延迟初始 tab、英文文案)

## 背景与目标

在 Finder 中右键文件夹(或文件、文件夹窗口空白处),菜单中出现
"New clitab Tab Here"(位于 服务/Services 子菜单,与 Ghostty / iTerm2 的实现
机制一致),点击后:

- clitab 已在运行 → 前置并聚焦主窗口,在目标目录新建标签并激活;
- clitab 未运行 → 启动 app,首个标签直接开在目标目录(不产生多余的 home 标签)。

成功标准:打包安装后,上述两种情形 + 右键文件 + 右键窗口空白处,四种入口
都得到聚焦的目标目录新标签;冷启动时**只有**这一个标签。

前置条件(已具备):工作区未提交的改动已让 `create_tab(cwd)` 支持指定目录
(路径失效回退 home,含单测)。本设计建立在其上。

## 机制选型

采用 **NSServices**(Ghostty 同款):`Info.plist` 声明服务 + 运行时注册
services provider。已核实:

- Ghostty.app 的 `Info.plist` 注册 `openTab` / `openWindow`,
  `NSRequiredContext.NSTextContent = FilePath`;
- Tauri 2 自动合并 `src-tauri/Info.plist`(tauri-build codegen 有
  rerun-if-changed 支持,bundler 负责合并);
- `objc2 0.6` / `objc2-app-kit 0.3` / `objc2-foundation 0.3` 已在 Cargo.lock
  (tao/wry 传递依赖),支持 `define_class!`,可纯 Rust 定义 ObjC 类;
- Services 只在打包后的 .app 生效(裸二进制无 Info.plist 注册),
  `npm run tauri dev` 下不可用属预期。

排除的方案:Finder Sync 扩展(需 appex + 正式签名,ad-hoc 签名基本加载
不了);仅 odoc/`open -a` 处理(无原生右键入口);ObjC 胶水 + cc(引入第二
语言,收益为零)。

## 架构与数据流

```
Finder 右键 → 服务 → "New clitab Tab Here"
  → macOS 向 clitab 发服务消息(Apple Event,事件循环启动后才送达)
  → ClitabServices.openTab:(NSPasteboard)userData:error:  读出文件路径
      ├─ settled(启动已完成):聚焦主窗口 → emit "open-tab-at" {cwd}
      │    → useTabManager 监听 → create_tab(cwd) → 激活新标签
      └─ 未 settled(冷启动窗口期):写入 pending 槽
           → 延迟启动线程醒来,取 pending 作为 cwd 建首个 tab
```

竞态由单一状态对象消除,handler 与启动线程都持锁:

```rust
struct ServiceState {
    pending: Option<PathBuf>, // 冷启动窗口期收到的路径,后到覆盖先到
    settled: bool,            // 初始 tab 决策是否已完成
}
// Arc<Mutex<ServiceState>>,经 pty::lock() 同款防中毒方式加锁
```

- 服务 handler:lock → 若 `!settled`,存 `pending` 并返回;若 `settled`,
  解锁后聚焦窗口 + emit `open-tab-at`。
- 启动线程:setup 中 spawn,sleep 400ms → lock → `settled = true`、
  `take()` pending → 解锁 → 若 `registry.list()` 为空则
  `create_tab(pending)`(非空说明极端情况下用户已 ⌘T,跳过,避免双 tab)。
  失败时保留现有 eprintln + dialog 行为。

400ms 的依据:服务消息只能在事件循环运行后送达,因此等待必须在后台线程
(阻塞 setup 会卡住事件循环,永远等不到消息);400ms 足够覆盖冷启动时
Apple Event 的送达延迟,人眼不可感知。

## 组件设计

### 1. `src-tauri/Info.plist`(新增)

只含 `NSServices` 键,其余由 Tauri 生成后合并。字段照抄 Ghostty 结构:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>NSServices</key>
  <array>
    <dict>
      <key>NSMessage</key>
      <string>openTab</string>
      <key>NSMenuItem</key>
      <dict>
        <key>default</key>
        <string>New clitab Tab Here</string>
      </dict>
      <key>NSRequiredContext</key>
      <dict>
        <key>NSTextContent</key>
        <string>FilePath</string>
      </dict>
      <key>NSSendTypes</key>
      <array>
        <string>NSFilenamesPboardType</string>
        <string>public.plain-text</string>
      </array>
    </dict>
  </array>
</dict>
</plist>
```

对文件夹、文件、Finder 窗口空白处右键均会出现(空白处 = 当前目录);
`NSTextContent = FilePath` 上下文由系统按选区判定,无需额外代码。

### 2. `src-tauri/src/services.rs`(新增)

- `define_class!` 定义 `ClitabServices : NSObject`,方法
  `openTab:userData:error:`(与 Ghostty selector 一致;返回值 BOOL,
  error 出参传 `NSString` 错误描述)。
- 实例 ivar 持有 `AppHandle` 与 `Arc<Mutex<ServiceState>>`。
  注意:`NSApplication.servicesProvider` 不持有强引用,实例必须同时存入
  Tauri managed state(`AppState` 增加字段)保活。
- 路径提取:`NSPasteboard.readObjectsForClasses([NSURL])`,options 限定
  `NSPasteboardURLReadingFileURLsOnlyKey`;取第一个 URL 的 path。
- 路径归类(纯函数,可单测):目录 → 自身;文件 → 父目录;无有效路径 →
  `None`,handler 返回 `NO`,不建 tab。
- 注册入口:`pub fn register(app: &AppHandle)`,在 setup(主线程)调用,
  `NSApplication::sharedApplication().setServicesProvider(instance)`。
- 聚焦 + emit(settled 分支):`app.get_webview_window("main")` →
  `unminimize()` → `show()` → `set_focus()`,随后
  `app.emit("open-tab-at", payload)`。

`Cargo.toml` 新增(target-gated,版本对齐 Cargo.lock):

```toml
[target.'cfg(target_os = "macos")'.dependencies]
objc2 = "0.6"
objc2-app-kit = "0.3"
objc2-foundation = "0.3"
```

### 3. `src-tauri/src/lib.rs`(修改)

- `AppState` 增加 services provider 保活字段与 `Arc<Mutex<ServiceState>>`。
- setup 中:注册 provider;spawn 延迟初始 tab 线程(见上);删除现有同步
  `create_tab(None)` 调用(其错误提示逻辑移入线程)。
- 其余命令、`TAB_GONE` 约定不动。

### 4. 前端(修改)

- `src/types.ts`:新增事件名 `open-tab-at` 与 payload 类型
  `{ cwd: string }`(Rust 侧 serde camelCase,遵循现有镜像约定;事件名
  常量与 Rust 保持字符串一致)。
- `src/hooks/useTabManager.ts`:监听 `open-tab-at`;将现有 `createTab`
  抽出可接受 cwd 覆盖参数的内部函数(默认仍取当前活动标签 cwd),
  事件到达时以 payload.cwd 建 tab 并设为激活。不新增其他逻辑。

## 错误处理

| 情形 | 行为 |
|---|---|
| pasteboard 无有效文件路径 | handler 返回 `NO`,无副作用 |
| 路径已失效(建 tab 前被删) | 复用 `resolve_cwd` 回退 home |
| 冷启动窗口期收到多条消息 | 后者覆盖前者,只建一个 tab |
| 初始 tab 创建失败 | 现有 eprintln + dialog 原样保留(移入线程) |
| dev 模式(无 bundle) | 服务不注册,功能不可用,预期行为 |

## 测试

**Rust 单测**(纯逻辑,不进 ObjC):

1. 状态机:未 settled → 存 pending、不发事件;已 settled → 走 emit 分支。
2. 路径归类:目录 → 自身;文件 → 父目录;垃圾输入 → `None`。
3. 现有 `resolve_cwd` 测试保持通过。

**静态检查**:`cargo test --lib`、`npm run typecheck` 全绿。

**手动验证协议**(必须打包验证,Services 依赖 bundle 注册):

1. `npm run package:macos` → 拖入 /Applications → 启动一次
   (让 LaunchServices 注册;必要时
   `/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f /Applications/clitab.app`
   强制注册)。
2. app 运行中:右键文件夹 → 服务 → New clitab Tab Here → 窗口前置、
   新标签聚焦、cwd 正确(标签标题即目录名)。
3. 退出 app,重复右键 → 冷启动后只有目标目录一个标签。
4. 右键文件 → 在其父目录开 tab;右键 Finder 窗口空白处 → 在当前目录开 tab。
5. 若菜单项未出现,检查 系统设置 → 键盘 → 键盘快捷键 → 服务。

**文档**:README.md / README.zh-CN.md 同步补充功能说明(两份保持一致)。

## 非目标

- "New Window Here"(clitab 为单窗口应用,无此概念)。
- Services 菜单项中文本地化(需 ServicesMenu.strings,文案已定为英文)。
- Finder Sync 扩展 / 顶层右键菜单项。
- `open -a clitab <dir>`、deep link、Dock 拖放等 odoc 入口。
- 多窗口、多路径批量打开。

## 涉及文件清单

| 文件 | 动作 |
|---|---|
| `src-tauri/Info.plist` | 新增 |
| `src-tauri/src/services.rs` | 新增 |
| `src-tauri/src/lib.rs` | 修改(注册、延迟初始 tab、state 字段) |
| `src-tauri/Cargo.toml` | 修改(objc2 三件套,target-gated) |
| `src/types.ts` | 修改(事件名 + payload) |
| `src/hooks/useTabManager.ts` | 修改(监听 + createTab 参数化) |
| `README.md` / `README.zh-CN.md` | 修改(功能说明) |
