# SecRelay 平台能力与硬约束调研（v0.1）

> 配套文档：`docs/需求分析.md` §4（角色矩阵：不是每个平台都能当"源"）、§7（风险清单）
> 调研范围：屏幕采集 / 摄像头 / 系统音频 / 硬件编解码 / 输入注入 / 后台常驻 / 商店政策
> 目标平台：Windows 10/11、Ubuntu/Debian Linux（X11 + Wayland）、macOS 12.3+、Android 10~15、iOS 15+
> 调研时间点：Apple 平台为 **macOS 26/27 世代、iOS/iPadOS 27**（WWDC26 之后）；Android 为 **API 36（Android 16）** 世代

## 0. 执行摘要（先看这一节）

**角色可用性总表**（被看 = 分享屏幕给他人；被控 = 接受他人的鼠标键盘）：

| 平台 | 被看端 | 被控端 | 观看端 | 一句话理由 |
|---|---|---|---|---|
| Windows 10/11 | ✅ | ✅ | ✅ | 三项全通；唯一代价是 UIPI + 服务 Session 0 需双进程架构 |
| Linux X11 | ✅ | ✅ | ✅ | 体验最顺（XTest 免授权），但 **X11 正在被淘汰**，是技术债 |
| Linux Wayland | ✅ | 🔶 | ✅ | 抓屏处处可行；**输入注入仅 GNOME/KDE**，wlroots 系（Sway/Hyprland）只能看 |
| macOS 12.3+ | ✅ | 🔶 | ✅ | 技术可行，但**周期性权限弹窗无法永久关闭**，且被控端建议放弃 MAS 分发 |
| Android 10~15 | 🟡 | 🔶 | ✅ | **每次投屏会话都要用户重新确认**；控端靠无障碍服务（政策风险） |
| iOS 15~26 | 🔶 | ❌ | ✅ | 全屏只能靠 ReplayKit **广播上传扩展**（用户须手动从控制中心启动、必须离开你的 App、扩展内存约 50MB 上限）；无法做"静默常驻被看端" |
| iOS 27+ | ✅ | ❌ | ✅ | 新增 SCK 全屏采集（须 `screen-capture` 后台模式），可做真正的后台被看端；但**输入注入永远不可能** |

**最大的 3 个平台风险**（详见 §1 与 §9）：

1. **iOS 的历史包袱**：App Store 上的屏幕共享类应用长期只能靠 ReplayKit 广播上传扩展（用户须手动离开 App、扩展内存约 50MB 上限）——**它能抓全屏，但体验代价大，且不能静默常驻**。Apple 于 WWDC26 明确让 **ScreenCaptureKit 取代 ReplayKit**，但该能力**要求 iOS 27+**，因此 iOS 15~26 上的"后台被看端"在官方路径下仍不可得。
2. **Android 每会话重授权**：`MediaProjection` 的 Intent 与实例**均一次性**，断线重连必然再次弹窗，直接决定"被控端长期在线"这个核心体验能不能成立。
3. **Wayland 输入注入后端断档**：`xdg-desktop-portal-wlr` 不支持 RemoteDesktop，导致 Sway/Hyprland 等合成器**无法实现远程控制**——而这批环境正是 Wayland 的活跃用户群。

---

## 0.1 本报告的核实方式与可信度标注

本文的关键结论**均以厂商官方文档原文为准**，通过直接抓取官方页面正文核实（Apple 使用其官方 JSON 文档接口 `developer.apple.com/tutorials/data/...`，Android 使用 `developer.android.google.cn` 英文镜像，Microsoft Learn / freedesktop portal 规范 / docs.rs 直接抓取）。

标注约定：

| 标注 | 含义 |
| --- | --- |
| ✅ 官方原文 | 已抓到官方文档原句，可直接引用 |
| 🟡 官方原文（非逐字） | 官方页可访问，但内容来自页面结构/元数据而非整段原句 |
| ⚠️ 未能直接核实 | 抓取被网络/反爬阻断（`support.google.com`、`developer.apple.com/forums` 在本机不可达），仅凭搜索摘要，**落地前必须复核** |

**已知未核实项清单（重要）**：

1. Google Play「敏感信息与 API 访问权限」政策原文（含 Accessibility API 申报要求、`REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` 可用性）——`support.google.com` 无法连接。⚠️
2. Apple Developer Forums 正文（含 ReplayKit 广播扩展 50MB 内存上限的官方工程师答复、`CGEvent.post` 被 Guideline 2.4.5 拒审的具体案例）——被反爬拦截。⚠️
3. Apple 对 `CGDisplayStream` / `CGWindowListCreateImage` 的**具体弃用版本号**——文档 JSON 路径未命中。⚠️

---

## 1. 十条最致命的硬约束

| # | 约束 | 影响 | 依据 |
| --- | --- | --- | --- |
| 1 | **Android 14+ 每次投屏会话都要用户重新确认**：`createScreenCaptureIntent()` 返回的 Intent 不得复用，同一个 `MediaProjection` 实例不得二次 `createVirtualDisplay()`，否则抛 `SecurityException` | 断线重连 = 弹窗重授权，无法做到「一次授权长期免打扰」 | ✅ [Android 14 行为变更](https://developer.android.google.cn/about/versions/14/behavior-changes-14?hl=en)、[Media projection](https://developer.android.google.cn/media/grow/media-projection?hl=en) |
| 2 | **iOS 从未有过「第三方 App 后台抓全屏」的官方路径，直到 iOS 27**。iOS 27 的 ScreenCaptureKit 才提供全屏采集，且**必须在 `UIBackgroundModes` 里声明 `screen-capture`**，否则 App 一进后台 `SCStream` 就被系统终止 | iOS 被看端门槛从「几乎不可能」变为「iOS 27+ 可行，但覆盖率受版本限制」 | ✅ [ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit)、[Capturing screen content on iOS](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-on-ios) |
| 3 | **Apple 官方宣布 ScreenCaptureKit 取代 ReplayKit**：「ScreenCaptureKit replaces ReplayKit for screen streaming and mirroring. A broadcast extension is no longer necessary.」 | **不要再按旧方案设计 iOS 广播上传扩展架构**；ReplayKit 路线是遗留路线 | ✅ [ScreenCaptureKit 概览](https://developer.apple.com/documentation/screencapturekit) |
| 4 | **macOS 的屏幕录制权限有周期性重复弹窗，且无法永久关闭**。macOS 15 对使用**已弃用采集技术**的 App 会反复提示（15.1 降低了频率）；提示原文：*"[App] is requesting to bypass the system private window picker and directly access your screen and audio."* | 被看端每次弹窗都是一次流失风险；走 SCContentSharingPicker 可避免此类弹窗 | ✅ [MacRumors 引 Apple 15.1 release notes](https://www.macrumors.com/2024/10/07/apple-screen-recording-popup-update/) 🟡 |
| 5 | **Wayland 下全屏抓取 + 输入注入必须走 xdg-desktop-portal，而输入注入的 portal 后端覆盖极差**：GNOME 与 KDE 支持 RemoteDesktop，`xdg-desktop-portal-wlr`（Sway/Hyprland 等 wlroots 系）**明确不支持 RemoteDesktop** | Wayland 远程控制只在 GNOME/KDE 上可做；wlroots 系只能看不能控 | ✅ [ArchWiki 后端支持矩阵](https://wiki.archlinux.org/title/XDG_Desktop_Portal)、[Remote Desktop portal 规范](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html) |
| 6 | **X11 正在被主流发行版淘汰**：GNOME 49 已默认禁用 X11 会话并计划在 GNOME 50 移除；Fedora KDE 40 起彻底不提供 X11 会话 | 针对 X11 的 XShm/XTest 捷径是**技术债**，长期必须押注 portal + PipeWire | ✅ [GNOME Blogs: An update on the X11 GNOME Session Removal](https://blogs.gnome.org/alatiera/2025/06/08/the-x11-session-removal/)、[Fedora Changes/KDE Plasma 6](https://fedoraproject.org/wiki/Changes/KDE_Plasma_6) |
| 7 | **App Store 有专门的「远程桌面客户端」条款 4.2.7**，把「镜像特定软件/服务而非通用镜像主机设备」的 App 单独约束：只能连**用户自有**的 PC/主机、**host 与 client 必须处于本地 LAN 网络**、账号创建必须在 host 端 | 互联网穿透式远程桌面在 iOS App Store 属高风险类目，需按通用镜像定位 + 法务复核 | ✅ [App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/)（4.2.7 Remote Desktop Clients） |
| 8 | **Windows 服务无法直接与用户交互（Vista 起）**，且所有服务运行在 **Session 0** | 远程控制必须做「Windows 服务（SYSTEM）+ 每会话用户态 agent」的双进程架构，不能单靠服务抓屏/注入 | ✅ [Interactive Services](https://learn.microsoft.com/en-us/windows/win32/services/interactive-services) |
| 9 | **`SendInput` 受 UIPI 限制**：只能向**完整性级别不高于自身**的进程注入输入 | 非提权进程无法操作系统级 UI（UAC 弹窗、任务管理器等）；需覆盖提权前台 | ✅ [SendInput function](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput) |
| 10 | **Android 系统音频采集「能录到」是例外而非默认**：只有 `USAGE_MEDIA`/`USAGE_GAME`/`USAGE_UNKNOWN` 且采集策略为 `ALLOW_CAPTURE_BY_ALL` 的播放才可被录；**targetSdk ≤ 28 的 App 默认禁止被采集** | 「听到对方电脑的声音」在 Android 上对大量第三方 App（DRM/通话/部分播放器）会静音 | ✅ [Capture video and audio playback](https://developer.android.google.cn/guide/topics/media/playback-capture?hl=en) |

---

## 2. 能力总矩阵（平台 × 维度）

图例：**✅ 原生可用** ｜ **🟡 有条件** ｜ **🔶 受限/需妥协** ｜ **❌ 不可行** ｜ **➖ 不适用**

| 维度 | Windows 10/11 | Linux X11 | Linux Wayland | macOS 12.3+ | Android 10~15 | iOS 15+ |
| --- | --- | --- | --- | --- | --- | --- |
| **1. 屏幕采集** | ✅ DDA / WGC | ✅ XShm+Damage（无零拷贝） | 🟡 仅 portal+PipeWire | ✅ ScreenCaptureKit | ✅ MediaProjection | 🔶 **iOS 27+** 才可全屏；≤26 仅 App 内录制 |
| 整屏 | ✅ | ✅ 根窗口 | 🟡 用户选 MONITOR/WINDOW/VIRTUAL | ✅ `SCContentFilter` | ✅（用户可选单 App） | 🔶 全屏需 iOS 27 + `screen-capture` 后台模式 |
| 单窗口 | 🟡 WGC 系统选择器（黄框） | 🟡 需合成器配合 | 🟡 WINDOW 类型 | ✅ `desktopIndependentWindow` | ✅ Android 14+ 单 App 共享 | 🟡 `presentForCurrentApplication()` 仅限本 App |
| 用户授权 | WGC 有系统选择器；DDA 无 | 无 | ✅ 每次/可持久化 portal 对话框 | ✅ TCC 屏幕录制（可持久） | ✅ 每次会话 | ✅ iOS 27 系统分享选择器 |
| 持久授权 | ✅ | ✅ | 🟡 `persist_mode=2`+`restore_token`（**单次使用**） | ✅ 但会周期重弹 | ❌ **每会话重授权** | ✅ |
| **2. 摄像头** | ✅ Media Foundation | ✅ V4L2 | 🟡 V4L2（+ Camera portal） | ✅ AVFoundation | ✅ Camera2/CameraX（**须原生**） | ✅ AVFoundation（**须原生**） |
| Rust 覆盖 | ✅ `nokhwa` | ✅ `nokhwa`/`v4l` | 🟡 `nokhwa`（非沙箱） | ✅ `nokhwa`（须 `nokhwa_initialize`） | ❌ `nokhwa` 无 Android 后端 | ❌ 无 Rust 后端 |
| **3. 系统音频** | ✅ WASAPI loopback | ✅ Pulse/PipeWire monitor | ✅ PipeWire monitor | ✅ `capturesAudio`（macOS 13+） | 🔶 AudioPlaybackCapture（大量 App 不可录） | 🔶 `.audio` 可录音乐，**VoIP/通话实测全 0** |
| **4. 硬件编码** | ✅ NVENC/QSV/AMF | ✅ VAAPI/NVENC | ✅ 同 X11 | ✅ VideoToolbox | ✅ MediaCodec（零拷贝） | ✅ VideoToolbox |
| Rust 绑定 | 🟡 ffmpeg-next | 🟡 ffmpeg-next / libva | 🟡 同上 + DMA-BUF 导入难 | 🟡 ffmpeg videotoolbox；`objc2` 系 | 🟡 须 JNI/NDK | 🟡 须 Swift/ObjC |
| **5. 输入注入** | ✅ SendInput（UIPI 限制） | ✅ XTest（无鉴权） | 🔶 仅 GNOME/KDE portal；wlroots ❌ | ✅ CGEvent + 辅助功能 | 🔶 无障碍服务（政策风险） | ❌ **根本不可能** |
| **6. 后台常驻** | 🟡 服务 Session 0 隔离 | ✅ | ✅ | 🟡 沙盒/公证/LaunchAgent | 🔶 FGS 类型 + 厂商杀进程 | 🔶 `screen-capture` 后台模式；扩展内存极紧 |
| **7. 商店风险** | 低（无商店） | 低 | 🟡 Flatpak/Snap 沙盒 | 🔶 MAS 需沙盒 + 辅助功能冲突 | 🔶 无障碍 API 申报 + 国产 ROM 杀进程 | 🔶 4.2.7 远程桌面条款 |

### 2.1 被看端 / 被控端适配度结论

| 平台 | 当**被看端**（共享屏幕） | 当**被控端**（接受远程控制） | 当**观看端**（看别人） |
| --- | --- | --- | --- |
| Windows 10/11 | ✅ 最佳 | ✅ 最佳 | ✅ |
| Linux X11 | ✅ 好 | ✅ 好（XTest 无摩擦） | ✅ |
| Linux Wayland | 🟡 好（portal，有选中对话框） | 🔶 仅 GNOME/KDE | ✅ |
| macOS 12.3+ | ✅ 好（首次 + 周期弹窗） | 🟡 需辅助功能授权 + 建议非 MAS 分发 | ✅ |
| Android 10~15 | 🟡 可用（每会话重授权） | 🔶 无障碍服务 + 政策风险 | ✅ |
| iOS 15+ | 🔶 **仅 iOS 27+**；≤26 只能 App 内 | ❌ 不可能 | ✅ |

---

## 3. 维度 1：屏幕采集

### 3.1 Windows

| 项 | DXGI Desktop Duplication (DDA) | Windows.Graphics.Capture (WGC) |
| --- | --- | --- |
| 引入版本 | Windows 8 | Windows 10 1803+ |
| 授权 | **无需授权**，直接抓整个输出 | **必须走系统选择器** `GraphicsCapturePicker.PickSingleItemAsync()` |
| 视觉提示 | 无 | **系统绘制黄色通知边框**（每个被采集对象一个） |
| 粒度 | 每个 `IDXGIOutput`（整显示器） | 显示器 **或** 应用窗口 |
| 帧数据形态 | DXGI surface，`DXGI_FORMAT_B8G8R8A8_UNORM`，含 dirty/move rects | `Direct3D11CaptureFramePool` → D3D11 surface |
| 旋转 | 需自行处理（surface 始终未旋转） | 由系统处理 |
| 失效 | `DXGI_ERROR_ACCESS_LOST`（切换全屏/分辨率变化） | 需 `Recreate` frame pool |
| 适用场景 | **自建被控端 agent**（无黄框、无需用户点选，体验最好） | 用户主动选择共享某窗口 |

关键原文：

- DDA：「Windows 8 disables standard Windows 2000 Display Driver Model (XDDM) mirror drivers and offers the desktop duplication API instead.」；「In a rotated mode, the surface that you receive from `AcquireNextFrame` is always in the un-rotated orientation, and the desktop image is rotated within the surface.」✅ [Desktop Duplication API](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api)
- WGC：「developer invoke secure system UI for end users to pick the display or application window to be captured, and **a yellow notification border is drawn by the system around the actively captured item**」；「Once the user has explicitly given consent to capturing an application window or display in the system UI, the `GraphicsCaptureItem` can be associated with multiple capture sessions.」✅ [Screen capture](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture)

> **工程结论**：被控端 agent 用 **DDA**（无黄框、无点选）；「用户选一个窗口投屏」用 **WGC**。注意 WGC 的 `GraphicsCaptureSession.IsSupported()` 需检测（远程桌面会话/部分虚拟机不支持）。

### 3.2 Linux（X11）

- 路径：`XShm` + `XDamage` + `XRandR`（枚举显示器）；`XComposite` 用于单窗口。
- **无硬件加速零拷贝路径**（不像 DDA 直接给 GPU surface），CPU 拷贝在所难免。
- **不需要任何用户授权**。
- 退化方式：`XGetImage` 全屏轮询（性能差，仅兜底）。

### 3.3 Linux（Wayland）— 必须走 portal

会话流程（ScreenCast portal，当前 **version 6**）：

1. `CreateSession()` → session handle
2. `SelectSources()`（**每会话仅可调用一次**）：`types`（1=MONITOR / 2=WINDOW / 4=VIRTUAL，默认 MONITOR）、`multiple`、`cursor_mode`（1=Hidden 默认 / 2=Embedded / 4=Metadata）、`restore_token`、`persist_mode`
3. `Start()` → **弹出系统对话框让用户选择**，返回 `streams a(ua{sv})`（PipeWire node id + 属性：`position`、`size`、`source_type`、`mapping_id`、`pipewire-serial`、`restore_token`）
4. `OpenPipeWireRemote()` → 拿到 fd，用 `pw_context_connect_fd` 建 `pw_core`

**持久化**（关键 UX）：

- `persist_mode`：`0` 不持久（默认）／`1` App 运行期间持久／`2` 直到用户显式撤销 ✅
- `restore_token`：**单次使用**，恢复成功后会返回**新的** token，必须保存新的
- 原文：「**Persistent remote desktop screen cast sessions can only be handled via the Remote Desktop interface.**」——即「抓屏+控输入」的持久会话不能只走 ScreenCast
- ScreenCast portal 的 `AvailableSourceTypes` 只定义了 **1=MONITOR / 2=WINDOW / 4=VIRTUAL**，**规范里没有音频源类型**——系统音频必须自行从 PipeWire/Pulse monitor 抓 ✅ [ScreenCast portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)

**PipeWire 侧**：stream 可能是 DMA-BUF / MemFd / SHM，像素格式常见 BGRx/RGBA + DRM modifier，需要自己做格式转换或走 GStreamer `pipewiresrc`。

### 3.4 macOS

| API | 版本 | 状态 |
| --- | --- | --- |
| `SCShareableContent` / `SCStream` / `SCContentFilter` / `SCStreamConfiguration` | **macOS 12.3+** | ✅ 唯一推荐路径 🟡（元数据核实） |
| `SCContentSharingPicker`（系统分享选择器） | macOS 14+ | ✅ **推荐**，可避免"绕过系统私有窗口选择器"类弹窗 ✅ |
| `SCStreamConfiguration(preset:)` | macOS 15+ | ✅ 🟡 |
| `capturesAudio` / `sampleRate` / `channelCount` / `excludesCurrentProcessAudio` | **macOS 13.0+** | ✅ |
| `captureMicrophone` / `microphoneCaptureDeviceID` | **macOS 15.0+** | ✅ 🟡 |
| `CGDisplayStream` / `CGWindowListCreateImage` | 旧 API | **已弃用**（具体版本未核实 ⚠️）；用其抓屏会触发 macOS 15 的周期性提醒弹窗 ✅ |
| `SCRecordingOutput` / `SCClipBufferingOutput` / `SCScreenshotManager` | 较新 | ✅ |

- **粒度**：整显示器、单窗口（`SCContentFilter(desktopIndependentWindow:)`）、按 App 排除/包含（`SCContentFilter(display:excludingApplications:exceptingWindows:)`）。
- **权限**：屏幕录制（TCC）。原文：「The first time you run this sample, the system prompts you to grant the app Screen Recording permission. **After you grant permission, you need to restart the app to enable capture.**」✅ [Capturing screen content in macOS](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos)
- Info.plist 需 `NSScreenCaptureUsageDescription` ✅ [ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit)
- **零拷贝优势**：SCStream 的 `.screen` 输出是**由 IOSurface 背书的 CVPixelBuffer**，可直接喂 VideoToolbox 编码器 ✅

### 3.5 Android

API：`MediaProjectionManager.createScreenCaptureIntent()` → `getMediaProjection()` → `createVirtualDisplay()` → 渲染到 `Surface`（`MediaCodec.createInputSurface()` 可零拷贝 / `ImageReader` / `SurfaceTexture`）。

- **单 App 共享（Android 14+）**：原文：「Android 14 (API level 34) introduces app screen sharing, which enables users to share a single app window instead of the entire device screen regardless of windowing mode. App screen sharing excludes the status bar, navigation bar, notifications, and other system UI elements」✅
- **能否强制整屏**：可以在 `createScreenCaptureIntent(MediaProjectionConfig)` 里传 `createConfigForDefaultDisplay()` 来 **opt out** 单 App 共享；但原文警告：**厂商可覆盖你的 opt out**（"Device manufacturers can override your app's opt out of app screen sharing if the app misuses the opt out to show private user data"）✅
- **必须注册回调**：不注册 `MediaProjection.Callback` 就调 `createVirtualDisplay()` 会抛 `IllegalStateException` ✅
- **Android 15 QPR1+ 状态栏大号提示芯片**：用户可点它停止共享；**锁屏会自动停止投屏** ✅

### 3.6 iOS — 见第 9 节专章

---

## 4. 维度 2：摄像头采集

| 平台 | 平台 API | Rust crate | 是否必须写原生代码 |
| --- | --- | --- | --- |
| Windows | Media Foundation | `nokhwa`（`input-msmf`）、`nokhwa-bindings-windows` ✅ | 否 |
| Linux | V4L2 | `nokhwa`（`input-v4l`）、`v4l`、`rscam` ✅ | 否（Flatpak/Snap 需 `--device=all`） |
| macOS | AVFoundation | `nokhwa`（`nokhwa-bindings-macos`，**须先调 `nokhwa_initialize`**）✅ | 否 |
| Android | Camera2 / CameraX | ❌ **`nokhwa` 无 Android 后端**（依赖里只有 linux/macos/windows 三套 bindings，`ndk` crate 仅提供 `AImageReader`/`ACameraManager` 等底层 C 绑定） | **是**（Kotlin/CameraX + JNI），或自行用 `ndk` crate 写 Camera2 NDK |
| iOS | AVFoundation | ❌ 无 Rust 后端 | **是**（Swift/ObjC，或 `objc2-av-foundation` 手写绑定） |

`nokhwa` 事实（docs.rs）：版本 `0.10.11`，可选依赖 `nokhwa-bindings-linux` / `nokhwa-bindings-macos` / `nokhwa-bindings-windows`，**没有 Android/iOS binding** ✅ [nokhwa crate](https://docs.rs/nokhwa/latest/nokhwa/)。上游 `l1npengtul/nokhwa` 之外存在 RustDesk 维护的 fork（`deep-soft/rustdesk-nokhwa`），**说明上游确实缺移动端支持** 🟡。

权限：

- Windows：`Settings > Privacy > Camera` 系统开关 + 桌面 App 无运行时权限弹窗。
- Linux：无系统级权限；Flatpak/Snap 沙盒需要设备访问权限。
- macOS：`NSCameraUsageDescription` + TCC 摄像头。
- Android：`CAMERA` 运行时权限；Android 12+ 有摄像头使用指示器与快捷开关（可被用户随时掐断）。
- iOS：`NSCameraUsageDescription`。

**冲突提示**：iOS 上 App 进后台时摄像头 `AVCaptureSession` 会暂停（需回前台重挂）✅ [Capturing screen content on iOS](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-on-ios)。

---

## 5. 维度 3：系统音频采集（"听到对方电脑的声音"）

| 平台 | 机制 | 权限 | 可行性要点 |
| --- | --- | --- | --- |
| Windows | WASAPI loopback：在 **render** endpoint 上开 capture stream，`IAudioClient::Initialize` 传 `AUDCLNT_STREAMFLAGS_LOOPBACK` | 无额外授权 | ✅ 最干净。约束：**仅支持 shared mode**（"A client can enable loopback mode only for a shared-mode stream… Exclusive-mode streams cannot operate in loopback mode"）；Win10 1703+ 支持事件驱动 loopback ✅ [Loopback Recording](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording) |
| Linux | PulseAudio/PipeWire 的 sink **monitor** source；或自建 `module-null-sink` 再路由 | 无 portal 门控（规范无音频源类型） | ✅ 可行但需处理 PipeWire 与 Pulse 双栈（Ubuntu 22.04+ 默认 PipeWire） |
| macOS | `SCStreamConfiguration.capturesAudio = true` + `.audio` SCStreamOutput（**macOS 13.0+**）；可 `excludesCurrentProcessAudio` 排除自身 | 屏幕录制 TCC（macOS 15 起音频权限与屏幕录制耦合在「Screen & System Audio Recording」） | ✅ 可行。注意 macOS 版 `excludesCurrentProcessAudio` 有效 |
| Android | `AudioPlaybackCaptureConfiguration` + `AudioRecord`（API 29+，也可用 MediaProjection token 构建） | `RECORD_AUDIO` **且**必须弹过 `createScreenCaptureIntent()` 并被批准；采集方与被采集方须同一用户 profile | 🔶 **大量内容录不到**，见下 |
| iOS | iOS 27 SCK 的 `.audio` 输出 | 同屏幕采集授权 | 🔶 **音乐类可录，VoIP/通话实测全 0**，见下 |

### 5.1 Android 系统音频的现实约束（重要）

可以录到的必要条件（三者同时满足）✅ [Capture video and audio playback](https://developer.android.google.cn/guide/topics/media/playback-capture?hl=en)：

1. 播放方的 `usage` 是 `USAGE_MEDIA` / `USAGE_GAME` / `USAGE_UNKNOWN`；
2. 播放方的采集策略是 `ALLOW_CAPTURE_BY_ALL`（取"最严格者生效"：manifest `android:allowAudioPlaybackCapture` + `AudioManager.setAllowedCapturePolicy()` + `AudioAttributes.Builder.setAllowedCapturePolicy()` 三者取最严）；
3. **`targetSdkVersion`**：target ≤ Android 9 的 App **默认不允许被采集**（需显式 `allowAudioPlaybackCapture="true"`）；target ≥ Android 10 的 App 默认允许（可显式设 `false` 拒绝）。

→ 现实中：**DRM 内容、通话语音、以及所有把策略设成 `ALLOW_CAPTURE_BY_NONE`/`SYSTEM` 的 App 都录不到**。产品文案不可承诺"一定能听到对方设备上任何声音"。

### 5.2 iOS 系统音频的现实约束（重要）

- iOS 27 上 `.audio` 输出**能**正确采集音乐播放（实测写了可听的 `.caf`）。
- 但对 **VoIP/通信类后台音频（Zoom、Teams）实测输出全 0 PCM**：格式、时序、`CMSampleBufferCopyPCMDataIntoAudioBufferList` 返回 `noErr` 全部正常，但 3400+ 个连续 buffer 的 peak/rms 恒为 0 —— 论坛证据显示这是 iOS 与 macOS 的行为差异（macOS 上 `.audio` 可录到通话音频）⚠️ [Apple 论坛：SCStreamOutputType.audio delivers all-zero PCM for VoIP](https://developer.apple.com/forums/tags/screencapturekit)
- `excludesCurrentProcessAudio` **在 iOS 27 上实测无效**（自播放音频会串回采集，分离度 0.0 dB），开发者被迫自行做回声相减 ⚠️ 同上
- `RPSampleBufferType.audioApp` **在 iOS 27 已弃用**，文档指向 ScreenCaptureKit ⚠️ 同上

> **产品结论**：iOS 上"听对方设备的声音"只能承诺**媒体播放类**，不能承诺会议/通话类。

---

## 6. 维度 4：硬件编解码

| 平台 | 硬件编码器 | Rust / 集成路径 | 回退代价 |
| --- | --- | --- | --- |
| Windows | NVENC（NVIDIA）/ Quick Sync（Intel）/ AMF（AMD） | `windows-capture` **自带"hardware-accelerated video encoder"**（含音频同步）✅；或 `ffmpeg-next` 走 `h264_nvenc` / `h264_qsv` / `h264_amf` | x264 软编：1080p60 占 1~2 个物理核，发热与功耗明显 |
| Linux | VAAPI（Intel/AMD）、NVENC | `ffmpeg-next`（`h264_vaapi` 需 `hwupload` + VAAPI device）；`cros-libva`、nvenc-rs 生态较薄 🟡 | 同上；且从 PipeWire DMA-BUF 到 VAAPI 的**零拷贝很难** |
| macOS | VideoToolbox（Apple Silicon 有独立媒体引擎） | `ffmpeg` 的 `videotoolbox` hwaccel；`objc2-video-toolbox`（`objc2-*` 系，ScreenCaptureKit 也已有 `objc2-screen-capture-kit` ✅） | Apple Silicon 软编会明显抢 CPU/续航 |
| Android | MediaCodec（H.264/H.265/AV1） | 必须 JNI/NDK；`ndk` crate 提供 `media` 模块（`AMediaCodec` 等）✅ | 软编 openh264 在 ARM 上代价高（发热、掉帧、耗电） |
| iOS | VideoToolbox | 必须 Swift/ObjC | 软编在移动端基本不可接受 |

**零拷贝链路（决定功耗与上限）**：

- Windows：DDA surface → D3D11 纹理 → NVENC/QSV/AMF 可直接吃 D3D11 纹理（好）。
- macOS：SCK 给 IOSurface 背书的 `CVPixelBuffer` → VideoToolbox 可直接吃（**最好**）✅
- Android：`MediaCodec.createInputSurface()` 给 VirtualDisplay 当输出 → 编码器直接消费（**好**）
- Linux：PipeWire DMA-BUF → VAAPI 需正确处理 DRM modifier 与 import；否则要 CPU 转换（**最差**）

**OEM 差异（Android）**：编码器数量上限、分辨率/level 上限、并发实例数因芯片而异，需运行时探测 + 软编兜底。

---

## 7. 维度 5：输入注入（远程控制）

| 平台 | API | 授权 | 提示文案 / 拒签风险 |
| --- | --- | --- | --- |
| Windows | **SendInput**（`winuser.h`） | 无需用户授权 | ⚠️ **受 UIPI**：「Applications are permitted to inject input only into applications that are at an equal or lesser integrity level.」→ 无法操作系统级提权 UI ✅ [SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput) |
| Linux X11 | **XTest**（`XTestFakeKeyEvent`/`XTestFakeButtonEvent`/`XTestFakeMotionEvent`） | 无 | 最顺滑；但**只在 X11 会话生效**。`x11` crate 含 `xtest` 模块 ✅ |
| Linux Wayland | **xdg-desktop-portal RemoteDesktop**（v2）：`CreateSession` → `SelectDevices`(KEYBOARD/POINTER/TOUCHSCREEN) → `Start` → `Notify*` 或 **`ConnectToEIS()`** | ✅ portal 对话框授权，可 `persist_mode`/`restore_token` | **后端覆盖是致命的**：GNOME ✅ / KDE ✅ / **wlroots 系（Sway、Hyprland）❌** ✅ [ArchWiki](https://wiki.archlinux.org/title/XDG_Desktop_Portal) |
| Linux 兜底 | `uinput` / `ydotool` | 需 root 或 udev 规则 | 可作为 wlroots 兜底，但需提权，且 Flatpak **基本拿不到 `/dev/uinput`** |
| macOS | **CGEvent**（`CGEventPost`）+ **辅助功能** TCC | ✅ 辅助功能（`AXIsProcessTrustedWithOptions`） | ⚠️ **拒签风险实测存在**：有开发者因使用 `CGEvent.post` 被 **Guideline 2.4.5** 拒审 ⚠️（论坛被反爬，未能读到原文，**必须复核**）。且无法注入 secure input 字段/登录窗口 |
| Android | **AccessibilityService** + `dispatchGesture`（API 24+） | 用户须在设置里启用无障碍 | ⚠️ **政策风险高**：Play 要求 Accessibility API 用途**必须在商店页面文档化**；不符合 `IsAccessibilityTool` 的 App 不得使用该 flag，且必须做**显著披露与同意** ⚠️（`support.google.com` 不可达，仅凭搜索摘要） |
| iOS | **无任何路径** | — | ❌ 见第 9 节 |

**Wayland 输入的两条路线**（RemoteDesktop v2 原文）：

- **EIS（推荐）**：「Call `ConnectToEIS()` after starting the session to obtain a file descriptor for a libei sender context. Input events are sent via the EI protocol. **Once an EIS connection is established, the `Notify*` D-Bus methods must not be used.**」✅
- **D-Bus Notify**：`NotifyPointerMotion` / `NotifyPointerMotionAbsolute` / `NotifyPointerButton`（Evdev 按钮码）/ `NotifyPointerAxis` / `NotifyPointerAxisDiscrete` / `NotifyKeyboardKeycode` / `NotifyKeyboardKeysym` / `NotifyTouch*`
- **跨 portal 组合**：「A remote desktop session can be used with other portals by passing the session created here to their methods: ScreenCast: Call `SelectSources()` and `OpenPipeWireRemote()` with the remote desktop session to capture screen content alongside input control. **The session must be started and stopped through this portal.**」✅
- `mapping_id` 用于把 PipeWire 流与 libei 绝对设备区域配对（v5 起）

---

## 8. 维度 6：后台 / 常驻限制

### 8.1 Android 前台服务

| 项 | 规则 |
| --- | --- |
| 必须声明类型 | Android 14+：manifest 里 `android:foregroundServiceType` **且** 请求对应权限（`FOREGROUND_SERVICE` + `FOREGROUND_SERVICE_MEDIA_PROJECTION`）；**manifest 里一个类型都不写 → 抛 `MissingForegroundServiceTypeException`** ✅ |
| mediaProjection 类型的运行时前提 | 原文：「**Call the `createScreenCaptureIntent()` method before starting the foreground service.** Doing so shows a permission notification to the user; the user must grant the permission before you can create the service. After you have created the foreground service, you can call `MediaProjectionManager.getMediaProjection()`.」✅ |
| 超时限制 | Android 15+ 的 **6 小时/24 小时**上限**只适用于 `dataSync` 与 `mediaProcessing`**；原文：「Currently, this restriction only applies to `dataSync` and `mediaProcessing` foreground service type foreground services.」→ **`mediaProjection` 无此超时** ✅ [FGS timeouts](https://developer.android.google.cn/develop/background-work/services/fgs/timeout?hl=en) |
| BOOT_COMPLETED | Android 15+ 禁止从 `BOOT_COMPLETED` 广播启动 `mediaProjection`（还包括 `dataSync`/`camera`/`mediaPlayback`/`phoneCall`/`microphone`），否则 `ForegroundServiceStartNotAllowedException` ✅ |
| SYSTEM_ALERT_WINDOW 豁免收窄 | Android 15+ 需**同时**持有权限**且已有可见的 `TYPE_APPLICATION_OVERLAY` 窗口**，否则后台启动 FGS 抛 `ForegroundServiceStartNotAllowedException` ✅ |
| 断流事件 | 用户点状态栏芯片、**锁屏**、另一个投屏会话开始、进程被杀 → `onStop()` ✅ |
| 静默断流风险 | 未注册 `onStop` 会静默录黑屏/静音：「Your app must respond appropriately, otherwise it will continue to record audio silence or a black video stream.」✅ |

### 8.2 iOS 后台模式

- iOS 27 SCK 全屏采集**必须**声明 `UIBackgroundModes: screen-capture`，否则「On iOS, ScreenCaptureKit terminates an SCStream when the app is backgrounded unless the app declares `UIBackgroundModes: screen-capture`」⚠️（论坛摘要）+ 官方示例代码证实 ✅
- 官方示例还声明 `audio`（为让麦克风 tap 在后台持续出样本），并在全屏采集前激活 `AVAudioSession` 的 `playAndRecord` 类别 ✅
- **App 内采集模式下摄像头预览会因后台暂停**，回前台需重挂 ✅

### 8.3 macOS 沙盒 / 公证 / 常驻

| 项 | 结论 |
| --- | --- |
| App Sandbox | 原文：「**To distribute a macOS app through the Mac App Store, you must enable the App Sandbox capability.**」✅ → MAS 强制沙盒，与非沙盒的辅助功能/注入需求冲突 |
| Hardened Runtime | 原文：「**To upload a macOS app to be notarized, you must enable the Hardened Runtime capability.**」✅ |
| 公证 | macOS 10.14.5+（新 Developer ID 证书签名者）与 10.15+（2019-06-01 后构建的 Developer ID 分发）**必须公证**；`altool`/Xcode 13 及更早自 **2023-11-01** 起不再被公证服务接受，须用 `notarytool`/Xcode 14+ ✅ [Notarizing macOS software](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution) |
| 后台常驻 | `SMAppService`（macOS 13+）注册 LaunchAgent/LoginItem；如需提权助手走 privileged helper 机制 |
| 锁屏 | 锁屏后无法继续采集有效画面（Android 15 亦明确"锁屏自动停止投屏"） |

### 8.4 Windows 服务化

原文：「**Services cannot directly interact with a user as of Windows Vista.** Therefore, the techniques mentioned in the section titled *Using an Interactive Service* should not be used in new code.」；「**All services run in Terminal Services session 0.**」✅ [Interactive Services](https://learn.microsoft.com/en-us/windows/win32/services/interactive-services)

官方推荐架构：服务 + 「a separate hidden GUI application」用 `CreateProcessAsUser` 跑在交互用户会话里，两者通过 IPC（命名管道等）通信；多用户系统下需为**每个会话**都拉起该 App（`HKLM\...\Run`），并用**基于会话 ID 的唯一管道名**区分。

→ SecRelay 的 Windows 被控端应为：**SYSTEM 服务（负责自启、更新、提权操作）+ 每用户会话的 agent（负责 DDA 抓屏、SendInput、音频）**。

---

## 9. 致命问题专章

### 9.1 iOS 到底能不能把整机屏幕画面给第三方 App 抓取并推流？

**结论（按 iOS 版本分层）**：

| iOS 版本 | 全机屏幕采集 | 可用机制 |
| --- | --- | --- |
| iOS 15 ~ 26 | ❌ **不能由第三方 App 后台抓全屏** | 只有 ReplayKit 的**系统广播上传扩展**（用户手动从控制中心启动）+ App 内录制 |
| **iOS 27+** | ✅ **可以** | **ScreenCaptureKit**（`SCShareableContent`/`SCStream`/`SCContentSharingPicker`）+ `UIBackgroundModes: screen-capture` |

Apple 官方对 iOS 的定调（原文）：✅ [ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit)

> **"ScreenCaptureKit replaces ReplayKit for screen streaming and mirroring. A broadcast extension is no longer necessary."**

并且：

> 「This sample requires a device running **iOS 27 or later**.」…「The sample declares two background modes so ScreenCaptureKit continues to run while the app isn't frontmost: `screen-capture` in `UIBackgroundModes`, so the stream survives backgrounding for full-display capture.」✅ [Capturing screen content on iOS](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-on-ios)

**iOS 27 的完整流程（推荐架构）**：

1. 配置 `SCContentSharingPicker` + 注册 `SCContentSharingPickerObserver`（`isActive = true`）
2. 全屏采集用 `picker.present()`；仅本 App 内容用 `picker.presentForCurrentApplication()`
3. 从 `contentSharingPicker(_:didUpdateWith:for:)` 回调拿到 `SCContentFilter`（含 `isMicrophoneEnabled`/`isCameraEnabled`）
4. 用该 filter 建 `SCStream`，挂 `.screen` 输出；`filter.isMicrophoneEnabled` 为真时才挂 `.microphone`
5. 全屏采集前激活 `AVAudioSession` 的 `playAndRecord` 类别
6. Info.plist 加 `NSScreenCaptureUsageDescription`

**ReplayKit 三种模式分别能做什么、代价是什么**：

| 模式 | 能抓什么 | 能离开本 App 吗 | 代价 / 限制 |
| --- | --- | --- | --- |
| **① App 内录制**（`RPScreenRecorder.startCapture` / `startRecording`） | **只能抓本 App 自己的内容**（离开 App 即失去画面） | ❌ | 用户无感、无需系统级操作；但做不了「看别人的屏幕」。iOS 11+ 的 `startCapture` 可取 `RPSampleBufferType`（video/appAudio/mic），适合"共享本 App"场景 |
| **② 系统广播上传扩展**（`RPBroadcastSampleHandler` + `RPSystemBroadcastPickerView` / 控制中心录屏按钮） | ✅ **整机屏幕**（用户在别的 App 里也能抓） | ✅ | ⚠️ **代价最大**：用户必须**手动离开你的 App**、从控制中心/选择器启动广播；状态栏常驻录屏指示；扩展运行在**内存极紧的独立进程**里（业界公认约 50MB 上限，超限被 jetsam 杀掉——Apple 论坛有 `replayd killed by jetsam reason highwater` 的实例 ⚠️）；不能依赖父 App 存活，需用 App Group 共享容器 / 本地 socket / `CFMessagePort` 传递数据；无法做"静默后台常驻" |
| **③ App 内录制（`RPScreenRecorder`, iOS 11+）** | 同 ①（本 App 内容） | ❌ | 同上 |

> **迁移建议**：新项目**直接基于 ScreenCaptureKit**，把 ReplayKit 广播扩展仅作为 **iOS ≤ 26 的降级路径**。若必须支持 iOS 15~26 的被看端，成本极高（需广播扩展 + 严格内存预算 + 用户教育），建议**不支持**，改为让 iOS ≤ 26 只做观看端。

**iOS 当前实测坑（iOS 27）**：

- ProMotion 设备上全屏采集**上限 60fps**（即使设了 `minimumFrameInterval = 1/120`、`queueDepth = 8`）；且 iOS 27 SDK 把 `minimumFrameInterval`/`queueDepth` 标记为 unavailable ⚠️
- `.audio` 对 VoIP 音频输出全 0；`excludesCurrentProcessAudio` 无效 ⚠️
- 只有 **App 内采集**模式支持摄像头叠加（`SCVideoEffectOutput`），**全屏采集不支持摄像头叠加** ✅

### 9.2 Android 14/15 的 MediaProjection 是否每次会话都要用户重新确认？

**是，每次会话都要 —— 这是官方明文规定。** ✅

Android 14 行为变更页原文：

> **"User consent required for each MediaProjection capture session"**
> For apps targeting Android 14 (API level 34) or higher, a `SecurityException` is thrown by `MediaProjection#createVirtualDisplay` in either of the following scenarios:
> - Your app caches the `Intent` that is returned from `MediaProjectionManager#createScreenCaptureIntent`, and passes it multiple times to `MediaProjectionManager#getMediaProjection`.
> - Your app invokes `MediaProjection#createVirtualDisplay` multiple times on the same `MediaProjection` instance.
>
> **Your app must ask the user to give consent before each capture session. A single capture session is a single invocation on `MediaProjection#createVirtualDisplay`, and each `MediaProjection` instance must be used only once.**

并且：

> If your app doesn't register this callback [`MediaProjection.Callback`], `MediaProjection#createVirtualDisplay` throws an `IllegalStateException`.

**精确语义**：

| 问题 | 答案 |
| --- | --- |
| 一次安装授权一次？ | ❌ |
| 一次进程授权一次？ | ❌ |
| **每次会话都要？** | ✅ **是**。「会话」= 一次 `createVirtualDisplay()` 调用 |
| token 可复用？ | ❌ `MediaProjection` 实例**只能用一次** |
| 配旋转变更怎么办？ | ✅ **不需要重新授权**：对**已存在的** `MediaProjection` 实例调 `VirtualDisplay#resize(w,h)` + `VirtualDisplay#setSurface(newSurface)` |
| Android 15 有变化吗？ | 行为变更页未见对投屏授权的进一步收紧；但新增：BOOT_COMPLETED 禁止启动 `mediaProjection` FGS、SYSTEM_ALERT_WINDOW 豁免需可见 overlay、Android 15 QPR1+ 状态栏大号提示芯片、**锁屏自动停止投屏** |
| 断线恢复体验 | 每次重连都会**再次弹授权**——这是 Android 被控端最大的产品痛点 |

### 9.3 Wayland 下能否全屏抓取 + 注入输入？需要用户走什么流程？

**可以，但有严格前提；流程是"用户通过系统对话框授权一次（可持久化）"。**

完整流程（GNOME/KDE 等支持 RemoteDesktop 的合成器上）：

1. App 调 `org.freedesktop.portal.RemoteDesktop.CreateSession()`
2. `SelectDevices()` —— 申请 `KEYBOARD|POINTER|TOUCHSCREEN`，可带 `persist_mode`（2 = 直到显式撤销）与 `restore_token`
3. `org.freedesktop.portal.ScreenCast.SelectSources()`（**用同一个 session handle**，ScreenCast 的持久化选项**不能**用于 RD 会话）、`OpenPipeWireRemote()` 拿 PipeWire fd
4. `RemoteDesktop.Start()` → **系统弹出对话框**：用户选择要共享的屏幕/窗口 + 勾选允许的输入设备
5. 返回 `devices`（用户实际批准的位掩码）+ `streams` + **新的 `restore_token`**（必须存下来）
6. 输入走 **`ConnectToEIS()`**（推荐，libei/EI 协议）**或** D-Bus `Notify*`

**硬约束**：

| 约束 | 说明 |
| --- | --- |
| **后端覆盖** | GNOME ✅、KDE ✅、**`xdg-desktop-portal-wlr`（Sway/Hyprland/wlroots 系）不支持 RemoteDesktop** → 这些环境**只能看不能控** ✅ |
| 会话归属 | 抓屏+控输入的**持久**会话**只能**由 RemoteDesktop portal 管理；ScreenCast 的 `persist_mode`/`restore_token` 不得用于 RD 会话 ✅ |
| token 单次性 | restore token **用一次即失效**，每次恢复都要保存新的 ✅ |
| xdg-desktop-portal 版本 | ScreenCast v6 / RemoteDesktop v2（`persist_mode`/`restore_token` 需 ScreenCast v4+、RemoteDesktop v2+） |
| 用户心智 | 首次必须走一遍系统对话框；`persist_mode=2` 后可做到"以后不再问" |
| X11 回退 | X11 会话下用 XTest，无任何对话框，体验最好 —— 但 X11 正在消失 ✅ |

### 9.4 macOS 首次授权流程与用户心智负担

**首次流程（被看端）**：

1. App 首次调用 ScreenCaptureKit → 系统弹出屏幕录制权限请求
2. 用户前往 **系统设置 → 隐私与安全性 → 屏幕录制**（macOS 15 起命名为「屏幕与系统音频录制」）勾选
3. ⚠️ **必须重启 App 才能生效**（官方示例原文：「After you grant permission, you need to restart the app to enable capture.」）✅
4. Info.plist 需 `NSScreenCaptureUsageDescription` ✅

**心智负担的四个来源**：

| 来源 | 说明 |
| --- | --- |
| **重启才能生效** | 用户容易以为"授权了还是不能用"，需要清晰的引导 UI |
| **周期性重复弹窗** | macOS 15 对使用**已弃用采集技术**的 App 反复提示；15.1 降低了"经常使用的 App"的弹窗频率，但**原文明确"There is no option to remove the popup permanently"** ✅ |
| **文案吓人** | 提示原文含 *"requesting to bypass the system private window picker and directly access your screen and audio… including personal or sensitive information that may be visible or audible."* ✅ |
| **状态栏指示器** | 屏幕录制期间有系统级指示（菜单栏/控制中心），用户始终知道被录 |

**减轻手段**：优先使用 **`SCContentSharingPicker`**（macOS 14+）——Apple 官方推荐用它替代自建选择 UI，走系统分享控件可显著减少"绕过系统私有窗口选择器"类弹窗与心智负担 ✅

**远程控制额外授权**：辅助功能（Accessibility）TCC，用于 `CGEventPost`；同样需要用户手动到「隐私与安全性 → 辅助功能」勾选（通常也需重启 App）。

---

## 10. 维度 7：上架与应用商店政策风险

### 10.1 Apple

| 项 | 结论 |
| --- | --- |
| **iOS 远程桌面专门条款** | **4.2.7 Remote Desktop Clients**：「If your remote desktop app acts as a mirror of specific software or services rather than a generic mirror of the host device, it must comply with the following:」(a) 只能连**用户自有**的 PC/专用游戏机，且「**both the host device and client must be connected on a local and LAN-based network**」；(b) 客户端内的软件/服务必须**完全在 host 上执行与渲染**，客户端不得使用超出串流所需的 API；(c) **账号创建与管理必须在 host 端发起**；(d) 客户端 UI 不得像 iOS/App Store 商店界面；(e) **云游戏/云应用的瘦客户端不适合 App Store** ✅ [App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/) |
| **风险判断** | 若按"通用镜像主机设备"定位（用户连自己的电脑），属于 4.2.7 之外的一类，历史上有 TeamViewer/Jump Desktop 等成功先例。若做成"连我们云上的/特定服务的"，直接落入 4.2.7 的 (a)(e) 限制。**互联网穿透（非 LAN）是审核关注点，建议准备 review notes 与法务意见** |
| **MAS 沙盒冲突** | Mac App Store **强制** App Sandbox ✅，而沙盒对辅助功能/输入注入支持有限 → **macOS 被控端建议走 Developer ID 直接分发 + 公证**，MAS 只发"观看端" |
| **公证** | 非 MAS 分发必须 Hardened Runtime + `notarytool` 公证 ✅ |
| 已知拒审信号 | 有开发者报告因 `CGEvent.post` 被 **Guideline 2.4.5** 拒审 ⚠️（未能读取论坛原文，**必须复核**后再确定 macOS 控端分发策略） |

### 10.2 Google Play / Android

| 项 | 结论 |
| --- | --- |
| 无障碍 API | 使用 Accessibility API **必须在商店页面文档化用途**；不符合 `IsAccessibilityTool` 的 App 不得使用该 flag，且必须满足**显著披露（prominent disclosure）与同意**要求 ⚠️（`support.google.com` 不可达，仅凭搜索摘要；答案 ID：16558241 / 16909972） |
| `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` | 属 Play 敏感权限，需申报正当用途并在政策允许场景使用 ⚠️ **未核实** —— 这是国产 ROM 保活的关键，落地前必须复核 |
| 国产 ROM 杀进程 | MIUI/EMUI/ColorOS/Funtouch 等激进清理后台，需引导用户做「自启动白名单 + 电池不优化 + 后台锁定」；无法通过 API 完全规避 |
| 前景服务类型不匹配 | 类型与用途不匹配会被审核挑战；`mediaProjection` 类型必须对应投屏用途 |
| 敏感权限申报 | 投屏 + 录音 + 无障碍 + 悬浮窗叠加，申报面很宽，建议尽早准备权限用途说明 |

### 10.3 Windows / Linux

- Windows：无应用商店门禁（Microsoft Store 可选，Win32 走 Store 需满足其政策但非必需）。
- Linux：无商店门禁；**Flatpak/Snap 沙盒**是主要摩擦 —— portal 能覆盖抓屏与输入（GNOME/KDE），但 **`/dev/uinput` 在 Flatpak 里基本不可用**，`--device=all` 也不保证可用 🟡。被控端建议提供 **deb/原生包**而非 Flatpak。

---

## 11. 推荐架构结论（工程落地建议）

| 角色 | Windows | Linux | macOS | Android | iOS |
| --- | --- | --- | --- | --- | --- |
| **被看（共享屏幕）** | DDA（自建）/ WGC（选窗口） | Wayland: portal ScreenCast+PipeWire；X11: XShm | SCK + SCContentSharingPicker | MediaProjection + MediaCodec input surface | **仅 iOS 27+**：SCK + `screen-capture` 后台模式 |
| **被控（注入输入）** | SendInput（服务+会话 agent 双进程） | Wayland: RemoteDesktop+EIS（**仅 GNOME/KDE**）；X11: XTest | CGEvent + 辅助功能（**建议非 MAS 分发**） | AccessibilityService（政策风险） | ❌ 不支持 |
| **观看端** | ✅ | ✅ | ✅ | ✅ | ✅ |
| **系统音频** | WASAPI loopback | PipeWire monitor | `capturesAudio` | AudioPlaybackCapture（覆盖不全） | `.audio`（仅媒体类） |
| **分发建议** | 原生安装包 + 服务 | 原生 deb/rpm（谨慎 Flatpak） | Developer ID + 公证（MAS 仅观看端） | Play + 官网 APK | App Store |

**三句话总结**：

1. **iOS ≤ 26 不能当被看端，iOS 也不可能当被控端**——iOS 的定位只能是观看端（iOS 27+ 才可升级为被看端）。
2. **Android 的问题是"每次都要用户点授权"**，不是技术不可行；**Wayland 的问题是输入注入后端覆盖**，不是抓屏不可行。
3. **Windows / macOS / Linux(X11) 是仅有的三个"被看+被控"都顺畅的平台**；macOS 需接受周期性权限弹窗并放弃 MAS 分发被控端。

---

## 12. 参考来源（按平台）

**Apple**

- [ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit)（"replaces ReplayKit…A broadcast extension is no longer necessary"）
- [Capturing screen content on iOS](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-on-ios)（iOS 27 sample、`screen-capture` 后台模式）
- [Capturing screen content in macOS](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos)（授权后需重启）
- [SCStreamConfiguration](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration)、[capturesAudio](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/capturesaudio)（macOS 13.0+）、[captureMicrophone](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/capturemicrophone)（macOS 15.0+）
- [SCShareableContent](https://developer.apple.com/documentation/screencapturekit/scshareablecontent)（macOS 12.3+）
- [App Sandbox](https://developer.apple.com/documentation/security/app-sandbox)、[Hardened Runtime](https://developer.apple.com/documentation/security/hardened-runtime)、[Notarizing macOS software](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)
- [App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/)（4.2.7 Remote Desktop Clients）
- [Apple 论坛 ScreenCaptureKit tag](https://developer.apple.com/forums/tags/screencapturekit)（iOS 27 `.audio` 全 0、`excludesCurrentProcessAudio` 失效、60fps 上限、iOS 后台终止 SCStream）⚠️ 摘要级
- [MacRumors: Apple Tweaks Screen Recording App Permissions…15.1](https://www.macrumors.com/2024/10/07/apple-screen-recording-popup-update/)（弹窗频率与原文）
- [Mac OTAKARA: WWDC26 ScreenCaptureKit 支援 iOS 27](https://www.macotakara.jp/news/entry-51358.html)

**Android**

- [Media projection](https://developer.android.google.cn/media/grow/media-projection?hl=en)
- [Behavior changes: Apps targeting Android 14](https://developer.android.google.cn/about/versions/14/behavior-changes-14?hl=en)（每会话需用户同意）
- [Behavior changes: Apps targeting Android 15](https://developer.android.google.cn/about/versions/15/behavior-changes-15?hl=en)
- [Foreground service types](https://developer.android.google.cn/develop/background-work/services/fgs/service-types?hl=en)
- [Foreground service timeouts](https://developer.android.google.cn/develop/background-work/services/fgs/timeout?hl=en)
- [Capture video and audio playback](https://developer.android.google.cn/guide/topics/media/playback-capture?hl=en)
- Google Play 敏感信息与 API 政策：`support.google.com/googleplay/android-developer/answer/16558241`、`.../answer/16909972` ⚠️ **本机不可达，未核实**

**Windows**

- [Desktop Duplication API](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api)
- [Screen capture (Windows.Graphics.Capture)](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture)
- [Loopback Recording](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording)
- [SendInput function](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)
- [Interactive Services](https://learn.microsoft.com/en-us/windows/win32/services/interactive-services)

**Linux / freedesktop**

- [ScreenCast portal（v6）](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
- [Remote Desktop portal（v2）](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)
- [Camera portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Camera.html)
- [portals.conf](https://flatpak.github.io/xdg-desktop-portal/docs/portals.conf.html)
- [ArchWiki: XDG Desktop Portal（后端支持矩阵）](https://wiki.archlinux.org/title/XDG_Desktop_Portal)
- [GNOME Blogs: An update on the X11 GNOME Session Removal](https://blogs.gnome.org/alatiera/2025/06/08/the-x11-session-removal/)
- [Fedora Changes/KDE Plasma 6（移除 X11 会话）](https://fedoraproject.org/wiki/Changes/KDE_Plasma_6)

**Rust crate 生态**

- [nokhwa 0.10.11](https://docs.rs/nokhwa/latest/nokhwa/)（仅 linux/macos/windows 后端）
- [windows-capture 2.0.1](https://docs.rs/windows-capture/latest/windows_capture/)（WGC + DDA + 硬件编码器）
- [objc2-screen-capture-kit 0.3.2](https://docs.rs/objc2-screen-capture-kit/latest/objc2_screen_capture_kit/)
- [pipewire 0.10.1](https://docs.rs/pipewire/latest/pipewire/)、[libpulse-binding 2.30.1](https://docs.rs/libpulse-binding/latest/libpulse_binding/)
- [v4l 0.14.0](https://docs.rs/v4l/latest/v4l/)、[x11 2.21.0](https://docs.rs/x11/latest/x11/)（含 `xtest`/`xshm` 模块）
- [enigo 0.6.1](https://docs.rs/enigo/latest/enigo/)（X11 + Wayland `ashpd`/libei + macOS `objc2` + Windows）
- [ndk 0.9.0](https://docs.rs/ndk/latest/ndk/)（Android NDK，含 `media` 模块）
