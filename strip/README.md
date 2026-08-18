# strip

`AgentMonitorStrip.app` — 一个**无标题栏、可拖拽、可缩放**、不抢焦点的 `NSPanel`，内嵌 `WKWebView` 加载 daemon 托管的 `http://127.0.0.1:39917/`。

- 没有系统标题栏：它会吃掉纵向空间并遮住页面。**窗口顶部 26pt 就是拖动区** —— 那一条正好是页面自己的 `AGENTS / QUOTA / TODAY / SUMMARIES` 标签行，不覆盖任何内容；顶端中央那颗小横条是唯一的提示
- 四边缩放由 AppKit 原生处理（`.resizable`）
- 位置和尺寸自动记住，下次启动原样恢复
- 无 Dock 图标、不进 Cmd-Tab（`LSUIElement`）
- 窗口层级 `.statusBar`，跨所有 Space 常驻，全屏应用之上
- 点它、拖它都不会激活本 app，终端键盘焦点不受影响
- daemon 没起来时显示内置错误条，每 5 秒自愈重连
- 显示器拔掉导致窗口落在无屏区时，自动挪回竖屏底边

## 构建

```sh
cd strip
xcodegen generate
xcodebuild -scheme AgentMonitorStrip -configuration Release -derivedDataPath build build
```

产物：`build/Build/Products/Release/AgentMonitorStrip.app`（ad-hoc 签名，arm64，部署目标 macOS 14.0，Swift 语言模式 5.0）。

`AgentMonitorStrip.xcodeproj` 和 `build/` 都是生成物，改配置改 `project.yml`，改代码改 `Sources/`，Info.plist 在 `Support/Info.plist`。

### ⚠️ Xcode 在外置卷上

如果 Xcode 或构建产物放在外置卷上，登录项指向该卷时开机可能卷还没挂载，启动会失败 —— 想开机自启，先把 .app 拷到内置盘（`/Applications` 或 `~/Applications`）再注册。

## 运行

```sh
open build/Build/Products/Release/AgentMonitorStrip.app
```

关闭：`pkill -f AgentMonitorStrip`（没有 UI 退出入口，本来就是常驻条）。

### 配置（只有两个环境变量，启动时读一次，没有设置界面）

| 变量 | 默认 | 说明 |
|---|---|---|
| `AGENT_MONITOR_STRIP_URL` | `http://127.0.0.1:39917/` | 加载的地址；要跟 daemon 的 `AGENT_MONITOR_PORT` 对上 |
| `AGENT_MONITOR_STRIP_HEIGHT` | `260` | **仅首次启动**的高度（pt），最小 110。之后以用户拖出来的尺寸为准 |

`open` 不传环境变量，调参时直接跑可执行文件：

```sh
AGENT_MONITOR_STRIP_HEIGHT=200 \
  build/Build/Products/Release/AgentMonitorStrip.app/Contents/MacOS/AgentMonitorStrip
```

页面会跟着窗口尺寸重排，不需要配高度：

| 窗口 | 版式 |
|---|---|
| 高 > 215pt | agents 独占第一行（标题列拿到全宽），quota / today / summaries 排第二行 |
| 高 ≤ 215pt | 退回一行四列的横条版式；窄于 880 / 680pt 时依次丢掉 summaries、today |
| 宽 ≤ 760pt（且高） | 四张卡竖着堆 |

每张卡显示几行是**量出来的**（`strip.js` 的 `fits()` 读 `clientHeight`），不是写死的常数，所以拉高就多几行。

### 状态列与详情弹层

agents 那列的状态不是静态色块：`working` 是三根柱子跑均衡器，`blocked` / `waiting` 单点呼吸，
`done` 扩散三次后停，其余静态。`prefers-reduced-motion: reduce` 下全部退回静态点。

点 summaries 任意一条弹出详情卡片。有两点是被窗口形态逼出来的：

- 关闭按钮挂在**卡片**右上角，不是窗口右上角 —— 窗口右上角是顶部 26pt 拖拽带和 6pt 边缘缩放带的交叠处，
  按钮放那儿按下去只会拖窗口或缩放。
- 点卡片以外的暗区也关，不让那个 26px 按钮成为唯一出路。

遮罩用**不透明色**。实测 `rgba(8,10,13,.985)` 在这个 WKWebView 里底下的内容仍然清晰可见，
半透明遮罩在这里不可信。

### 首次启动的位置

只有第一次（没有存过尺寸时）才自动选屏：`NSScreen.screens.first { frame.height > frame.width }`，没有竖屏就退回 `NSScreen.main`，取该屏 `frame` 底边、整屏宽。不写死 display id（重新插拔会换 id）。

用 `frame` 而不是 `visibleFrame`：本机竖屏 `visibleFrame` 只在**顶部**留了 30pt，底边两者一致。

之后每次移动 / 缩放都会存进 `UserDefaults` 的 `panelFrame`。想回到默认位置：

```sh
defaults delete local.agentmonitor.strip && pkill -f AgentMonitorStrip
```

## 开机自启（LaunchAgent）

选 LaunchAgent 而不是 `SMAppService.mainApp.register()`：前者能顺带塞环境变量，且不需要往 app 里加注册代码。

写 `~/Library/LaunchAgents/local.agentmonitor.strip.plist`：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>local.agentmonitor.strip</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Applications/AgentMonitorStrip.app/Contents/MacOS/AgentMonitorStrip</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>AGENT_MONITOR_STRIP_HEIGHT</key><string>260</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict>
</plist>
```

```sh
cp -R build/Build/Products/Release/AgentMonitorStrip.app /Applications/   # 别指向外置卷
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/local.agentmonitor.strip.plist
launchctl bootout   gui/$(id -u)/local.agentmonitor.strip                 # 卸载
```

（本仓库不自动装这个 plist，要装自己跑。）

## 抓图 / 排查

`peekaboo window list` 对这个 app 会挑到 WebKit 自带的一个 500×500 离屏窗口（`CGWindowListCopyWindowInfo` 里 `layer=0`、`onscreen` 为空，非本项目代码创建，无害）。要截图直接截竖屏整屏：

```sh
peekaboo image --mode screen --screen-index 1 --path /tmp/strip.png
```

查窗口几何最省事的是 System Events（返回的是 CG 左上原点坐标）：

```sh
osascript -e 'tell application "System Events" to get {position, size} of window 1 of (first process whose name contains "AgentMonitor")'
```

### 三个实测到的坑

`panel.isFloatingPanel = true` 的 setter 会把 `level` 改回 `.floating`(3)。所以**必须先设 `isFloatingPanel` 再设 `level = .statusBar`**，否则实测 `layer=3`，会被其他 status 级窗口压住。代码里已按这个顺序写。

`Info.plist` 里的 `NSAppTransportSecurity.NSAllowsLocalNetworking` 不能删 —— 没有它 ATS 拦掉明文 HTTP 的 localhost 请求，面板一片空白。

**别用 `setFrameAutosaveName` 存窗口尺寸。** 它保存的是「窗口 rect + 当时的屏幕 rect」，恢复时按两者比例重算。实测本机存 1080×260、重启后变成 1080×335 且 x 偏了 6pt。改成自己往 `UserDefaults` 写四个数就精确了。又因为这个 app 没有正常退出路径（用户用 `pkill` 关），写完必须 `synchronize()`，否则 cfprefsd 会把写入合并掉、直接丢失。

**`.resizable` 下边缘缩放是 AppKit 在 hit test 之前就接管的**，事件根本进不到自己的 `NSView`。所以 `ChromeView` 里只留了顶部拖动带；尺寸持久化不能挂在自己的 `mouseUp` 上（那次永远不触发），得挂 `NSWindowDelegate` 的 `windowDidMove` / `windowDidResize`。
