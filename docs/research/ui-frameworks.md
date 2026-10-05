# SecRelay 跨平台 UI 技术选型调研

- **调研时间**：2026-10-06（所有"今天"均指该日期）
- **目标项目**：SecRelay —— 跨设备实时连接软件（远程桌面、远程摄像头、投屏、文件传输、语音/文字消息）
- **硬性约束**：Rust + 跨平台 UI；覆盖 Windows / Ubuntu-Debian Linux / macOS / Android / iOS；项目协议 GPLv3；需要实时显示解码后的视频帧（目标 60fps、1080p+，理想 4K）；很可能要上 Google Play 与 Apple App Store
- **调研方法**：全部数据来自 crates.io API、项目官方文档站、GitHub REST API（发行版/仓库活跃度/issue 搜索）与官方仓库 raw 文件；每条关键结论附证据链接
- **已知环境说明**：本机 DNS 将 `github.com` 解析到 127.0.0.1，`web_fetch` 无法直接抓取 `github.com` 页面正文；因此 GitHub issue/PR 的**标题、状态、日期、编号**通过 GitHub REST API 核实（属权威 API），但正文细节与部分官网页面未能逐字抓取，文中已标注"未核实"

---

## 0. 一句话结论

**没有任何一个方案能"纯 Rust 一行不改"地覆盖五平台 + 60fps 零拷贝视频。** 现实的三个梯队是：

1. **Slint**（纯 Rust、GPLv3 原生契合、官方覆盖 Android/iOS、有官方 wgpu/OpenGL 纹理导入与 ffmpeg 播放器示例）——**纯 Rust 路线的第一选择**，但移动端 IME 尚有一批未闭合 issue，且 D3D11/Metal 借用纹理没有公开 API。
2. **Flutter（Dart UI）+ flutter_rust_bridge（Rust 核心）**——**成熟度最高的现实答案**，五平台全绿、视频零拷贝有成熟基建（`irondash_texture`：Android `ANativeWindow`/`SurfaceTexture`、iOS/macOS `IOSurface`、Windows `D3D11Texture2D`/`DXGI shared handle`、Linux `GLTexture`），代价是 UI 层不是 Rust。
3. **Dioxus + WebView** 与 **Tauri v2 + WebView**——生态与打包链路最成熟，但视频帧要穿过 webview，实时低延迟是主要痛点。

**egui/iced/Freya/Xilem/vizia 的共同硬伤是移动端**：egui/eframe 只有 Android（无 iOS），iced 官方定位 Win/macOS/Linux/Web，其余更弱。

---

## 1. 总览对比表

| 候选 | 最新版本 / 发布日期 | UI 语言 | Win | macOS | Linux(Wayland) | Android | iOS | 视频帧零拷贝 | 许可证 | 最近活跃度 |
|---|---|---|---|---|---|---|---|---|---|---|
| **Slint** | 1.18.1 / 2026-09-21 | 纯 Rust + `.slint` DSL | ✅ | ✅ | ✅（winit，wayland 默认 feature） | ✅ 官方文档 | ✅ 官方文档 | ◐ 可（wgpu/GL 纹理导入示例；无 D3D11/Metal 借用 API） | GPLv3 / Royalty-Free / 商业 三选一 | 高（2026-10-05 push，24k★） |
| **Dioxus** | 0.7.10 稳定 / 2026-07-30（0.8.0-alpha.1） | Rust（rsx! 宏） | ✅ | ✅ | ✅ | ◐ 官方支持但有成堆未修 bug | ◐ 同上 | ✖（WebView 路线）/ ◐（dioxus-native 太年轻） | MIT OR Apache-2.0 | 很高（39k★） |
| **Makepad / Robius** | crates.io 1.0.0 / 2025-05-13；仓库持续开发 | 纯 Rust + DSL | ✅ D3D11 | ✅ Metal | ✅ OpenGL | ✅ 官方 `cargo makepad android` | ◐ 有工具链，iOS 文本输入崩溃未修 | ✖ **公开 API 缺失**（内部已支持，见 issue #1153） | MIT OR Apache-2.0 | 高（2026-10-05 push） |
| **Ribir** | 0.3.0 稳定；0.4.0-alpha.65 / 2026-04-21 | 纯 Rust | ✅ | ✅ | ✅（wgpu） | ✖ 无证据 | ✖ 无证据 | ✖（无公开纹理导入 API） | MIT | **停滞**（最后 push 2026-04-21） |
| **egui / eframe** | egui 0.36.2 / 2026-09-08；eframe 0.36.2 | 纯 Rust | ✅ | ✅ | ✅ | ◐ `android-game-activity`/`android-native-activity` feature（官方） | ✖ 无官方支持 | ✅✅ **最方便**：`egui_wgpu::Renderer::register_native_texture(wgpu::TextureView)`、`egui_glow::Painter::register_native_texture` | MIT OR Apache-2.0 | 很高（30.8k★） |
| **iced** | 0.14.0 / 2025-12-07 | 纯 Rust | ✅ | ✅ | ✅ | ✖（官方定位 Win/macOS/Linux/Web） | ✖ | ◐ 自定义 shader/primitive 可行，但 image 只有 CPU 路径 | MIT | 高（31.7k★，2026-10-05 push） |
| **Freya** | 0.4.3 稳定；0.5.0-rc.9 / 2026-10-04 | 纯 Rust | ✅ | ✅ | ✅ | ✖ | ✖ | ✖ 未核实 | MIT | 中高（3.2k★） |
| **Xilem** | 0.4.0 / 2025-10-29 | 纯 Rust | ✅ | ✅ | ✅ | ◐ 仅社区脚手架 | ◐ 仅社区脚手架 | ✖ 未核实 | Apache-2.0 | 中（作者自称 experimental） |
| **vizia** | 0.4.0 / 2026-04-23 | 纯 Rust | ✅ | ✅ | ✅ | ✖ | ✖ | ✖ | MIT | 偏低（2.3k★） |
| **Tauri v2** | 2.12.1 / 2026-10-01 | Rust 后端 + Web UI | ✅ | ✅ | ✅ | ✅ 官方 | ✅ 官方 | ✖ 帧须过 webview/IPC | MIT OR Apache-2.0 | 极高（111.6k★） |
| **Flutter + flutter_rust_bridge** | FRB 2.13.0 / 2026-09-12 | Dart UI + Rust 核心 | ✅ | ✅ | ✅ | ✅ 生产级 | ✅ 生产级 | ✅✅ `Texture` widget + 纹理注册表；桌面用 `irondash_texture`（IOSurface/GLTexture/D3D11/DXGI/ANativeWindow） | Flutter BSD-3；FRB MIT | 极高 |
| **平台原生 UI + Rust 核心** | — | Swift/Kotlin/C#/C | ✅ WinUI3 | ✅ SwiftUI | ✅ GTK4 | ✅ Compose | ✅ SwiftUI | ✅✅ 原生最优（AVSampleBufferDisplayLayer / SurfaceView / DirectComposition） | 各自（GTK 为 LGPLv3） | — |

图例：✅ 可用　◐ 部分/有坑　✖ 不具备或未支持

---

## 2. 逐候选详解

### 2.1 Slint

**定位**：Rust 原生声明式 UI（`.slint` DSL 编译为原生代码），核心用 Rust 写成，另有 C++/JS/Python 绑定。

- **桌面三平台**：官方支持 Windows / macOS / Linux。后端是 `winit`，`backend-winit-wayland` 与 `backend-winit-x11` 都是正式 feature，默认启用 wayland；默认渲染器 `renderer-femtovg`（OpenGL）+ `renderer-software`，另可选 `renderer-skia`、`renderer-vello`（1.18 新增，实验性）。
- **Android / iOS**：**官方支持**。Android 通过 `backend-android-activity-06`（`android-activity` 0.6 / NativeActivity）接入，官方文档有 `slint::android` 模块与"Building and deploying"章节，构建工具推荐 `cargo apk2`（`cargo-apk2` 1.4.2, 2026-09-29；旧的 `cargo-apk` 已由它取代）。iOS 在仓库 `docs/ios.md` 有专门说明，官方 sidebar 同时列出 Android 与 iOS 的 Mobile Development 章节。**已知坑（Open 状态，均为 IME/输入系统）**：
  - `Keyboard/Input problems on Android`（2026-05-20，open）
  - `Android TextInput issues with Microsoft SwiftKey Keyboard`（2025-08-23，open）
  - `Soft keyboard flashes on startup on Android TV`（2026-03-29，open）
  - `request_redraw isn't working for me on Android`（2025-06-13，open）
  - `Android: Window::is_visible() always returns true`（2025-03-21，open）
- **是否纯 Rust 写 UI**：是（UI 用 `.slint` DSL，业务逻辑 Rust；不是 Rust 之外的语言）。
- **许可证**：三选一 —— GPLv3 / Royalty-Free 2.0 / 商业。
  - **对本项目最关键的一点**：SecRelay 本来就是 GPLv3，**可以名正言顺地用 Slint 的 GPLv3 档位**，零费用、零归因义务，且按官方说法"你自己的文件可以保持 MIT/Apache-2.0"（链接方式见下）。
  - Royalty-Free 档位（免费、可用于专有桌面/移动/Web 应用，**不含嵌入式**）要求归因：要么在 About 对话框里放 `AboutSlint` widget，要么显示 Slint 徽标。**它与 GPLv3 是并列选项（OR），不是叠加义务**，但对一个坚持 GPLv3 的项目没有理由放弃 GPLv3 档位。
- **实时视频帧 —— 关键结论：可做到 GPU 纹理导入，但平台不对称**
  - `slint::Image` 提供 `to_wgpu_29_texture` / `to_wgpu_30_texture`，以及 `unsafe fn from_borrowed_gl_2d_rgba_texture(texture_id, size, origin)`（GL 纹理 ID 借用，零拷贝）。**没有** `from_borrowed_d3d11_texture` / `from_borrowed_metal_texture`（我逐版本核对了 1.16/1.17/1.18 的 `Image` 方法列表：三版都只有 `from_borrowed_gl_2d_rgba_texture`）。
  - `Window::set_rendering_notifier(callback: impl FnMut(RenderingState, GraphicsAPI))`，其中 `GraphicsAPI` 有 `NativeOpenGL` / `WebGL` / `WGPU29` / `WGPU30` 四个变体 —— 也就是**可以在 Slint 的渲染上下文里直接跑你自己的 wgpu/OpenGL 绘制**，把外部纹理画进来。
  - 官方仓库有两个**恰好对口的示例**：
    - `examples/wgpu_texture` —— "WGPU Texture Import Example"：用 wgpu 把效果渲染到纹理，再 `set_rendering_notifier` 在 `BeforeRendering` 阶段导入 `slint::Image`。
    - `examples/ffmpeg` —— 真实的 ffmpeg 播放器示例，README 明确给出 Linux/macOS/Windows/Android 的构建步骤（Android 用 `cargo apk2 build --target aarch64-linux-android --lib`）。
  - **实操判断**：Windows 上 Slint 默认走 OpenGL（femtovg），而 D3D11VA 解码产出的是 D3D11 纹理 —— 两者不能零拷贝互通；要靠 `renderer-femtovg-wgpu` / `unstable-wgpu-30` 走 wgpu 路径，再用 `wgpu-hal` 从 DXGI shared handle 造 `wgpu::Texture`（wgpu `Texture::from_hal` 系列，需 unsafe，属进阶工作）。macOS 上 VideoToolbox 产出 `CVPixelBuffer`/`IOSurface`，走 Metal 侧同样需要自建桥。**Android 反而是最舒服的**：MediaCodec → `SurfaceTexture` → `GL_TEXTURE_EXTERNAL_OES`，`from_borrowed_gl_2d_rgba_texture` 直接吃。
  - **退路**：每帧 `SharedPixelBuffer<Rgba<u8>>` / `Image::from_rgba8` 走 CPU 上传。1080p60 ≈ 498 MB/s，桌面可用但不优雅；**4K60 ≈ 2 GB/s**，会明显吃 CPU 和带宽。这是 Slint 方案最大的性能风险点。
- **打包与构建链路**：Android `cargo apk2`（官方示例已用，活维护）；iOS 官方在仓库 `docs/ios.md` 有流程说明，需 Xcode 工程 + 签名，`cargo-xcode`（1.11.1，2026-06-21）可用于生成工程。桌面三平台直接用 Cargo + 常规打包工具。
- **生态与维护**：24,066★、822 个 open issue、2026-10-05 有 push、2026-09-21 发 1.18.1、crates.io 累计 183 万次下载（近 90 天 54.8 万）。**三家中唯一同时具备"纯 Rust + 官方移动端 + GPLv3 契合"的成熟框架**。有商业公司（SixtyFPS GmbH）持续投入，且是 LibrePCB 2.0 的正式迁移目标（桌面成熟度被真实项目验证）。
- **主要风险**：① 移动端 IME/软键盘 issue 未闭合（对"文字消息"功能是直接命中）；② Windows 上缺少 D3D11 借用纹理公开 API，4K60 视频需要自研 wgpu 桥；③ 无 D3D11/Metal 借用纹理意味着"零拷贝"要写 unsafe 图形胶水代码；④ 生态组件（图表、富文本编辑器等）比 Flutter/Web 弱。

**证据**：[Slint 1.18 发布说明](https://slint.dev/blog/slint-1.18-released.html) · [1.13 发布说明](https://slint.dev/blog/slint-1.13-released) · [crates.io slint（features 与 license 字段）](https://crates.io/crates/slint) · [Image API](https://slint.dev/releases/1.18.0/docs/rust/slint/struct.Image.html) · [Window::set_rendering_notifier](https://slint.dev/releases/1.18.0/docs/rust/slint/struct.Window.html) · [GraphicsAPI](https://slint.dev/releases/1.18.0/docs/rust/slint/enum.GraphicsAPI.html) · [BorrowedOpenGLTextureBuilder](https://slint.dev/releases/1.18.0/docs/rust/slint/struct.BorrowedOpenGLTextureBuilder.html) · [examples/wgpu_texture](https://github.com/slint-ui/slint/tree/master/examples/wgpu_texture) · [examples/ffmpeg](https://github.com/slint-ui/slint/tree/master/examples/ffmpeg) · [LICENSE.md](https://raw.githubusercontent.com/slint-ui/slint/master/LICENSE.md) · [Royalty-Free 2.0 全文](https://raw.githubusercontent.com/slint-ui/slint/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md) · ["Making Slint Desktop-Ready"（含三档授权表）](https://slint.dev/blog/making-slint-desktop-ready.html)

---

### 2.2 Dioxus（0.7 及之后）

**定位**：类 React 的 Rust 全栈框架（`rsx!` 宏 + signals），桌面/移动走 WebView（`wry`），另有实验性的 GPU 原生渲染器。

- **桌面三平台**：✅（`dioxus-desktop` = wry/webview）；Linux 走 WebKitGTK，Wayland 下取决于 WebKitGTK，通常可用（未逐项核实 WebKitGTK + Wayland 的具体 bug 列表）。
- **Android / iOS**：**官方支持，但质量参差**。CLI 提供 `dx bundle --platform android|ios`、自动打开模拟器、可通过 `dioxus.toml` 自定义 `AndroidManifest.xml` 与 `Info.plist`（0.7 发布说明明确列出）。但 open 的移动端 bug 很具体且不少（2026-08~10 仍在新增）：
  - `Meta-issue: Android and IOS build`（2026-08-28，open）
  - `Android app shows a white screen due to Wry activity initialization race`（2026-10-02，open）
  - `A webview that loses its page shows a blank or frozen window instead of the app (Android, iOS, macOS, Windows, Linux)`（2026-10-03，open）
  - `dx fails to link for Android on Windows hosts: backslashes stripped from linker response file`（2026-09-17，open）
  - `Android: dx serve --android fails with "Failed to encode RUSTFLAGS" when NDK/linker path contains a space`（2026-09-12，open）
  - Android 相关 open issue 共 26 个。
- **是否纯 Rust 写 UI**：是（`rsx!` 宏，Rust 语法），但**渲染靠系统 WebView**，实际像素由 HTML/CSS 引擎绘制 —— 这与"纯 Rust UI"在架构上有本质区别，也决定了视频帧的处理方式。
- **许可证**：MIT OR Apache-2.0，无 copyleft 冲突，与 GPLv3 完全兼容。
- **实时视频帧 —— 关键结论：WebView 路线是硬伤，原生路线太年轻**
  - WebView 路线：视频帧要么由 JS 侧用 WebCodecs 解码（那 Rust 核心就不在解码链路上），要么从 Rust 传进页面。传法只有 IPC/自定义协议 → data URL/blob → `<canvas>`/`<video>`，每一步都是拷贝 + JS 解析，**1080p60 基本不现实**，4K 更不用谈。`webview` 本身也无法高效消费"Rust 侧已解码好的 GPU 纹理"。
  - `dioxus-native` + Blitz（Stylo + Taffy + Vello，纯 Rust HTML/CSS 渲染器）：0.7 首发，官方明确写着"**Blitz 仍被视为 work in progress，我们还没有专注性能**"。`dioxus-native` 目前只有 0.8.0-alpha.1。作为实验方向值得关注，但**今天不能作为 60fps 视频显示的依赖**。
  - 另有 `third-party-renderer` feature 与自定义渲染器接口，理论上是出路，但成熟度同样不足（未逐项核实 API 细节）。
- **打包与构建链路**：`dioxus-cli`（0.7.10）统一处理桌面/移动/Web 打包，`dx bundle` 一条命令；移动端仍需 Xcode / Android SDK+NDK。链路对新手友好，但 Windows 主机上的 Android 链接问题（见上）会拖慢 CI。
- **生态与维护**：39,322★、815 open issue、2026-10-03 push、稳定版 0.7.10（2026-07-30）、crates.io 累计 295 万次下载（近 90 天 110 万）。**社区热度最高的 Rust UI 项目**，`dx` 工具链（含 Rust 热补丁 Subsecond）体验最好。
- **主要风险**：① 视频帧路径与"实时远程桌面/投屏"的核心需求正面冲突；② 移动端仍有白屏、链接失败等阻断级 bug；③ 若为视频改用 `dioxus-native`，则同时承担"框架 alpha + 渲染器不追性能"的双重风险。

**证据**：[Dioxus 0.7 发布说明](https://dioxuslabs.com/blog/release-070/) · [crates.io dioxus](https://crates.io/crates/dioxus) · [release v0.7.10 / v0.8.0-alpha.1](https://newreleases.io/project/github/DioxusLabs/dioxus/release/v0.7.4) · [Blitz CSS 支持矩阵](https://blitz.is/status/css)

---

### 2.3 Makepad / Project Robius

**定位**：Rik Arends 的纯 Rust UI 运行时 + 可实时编辑的 DSL，自绘（Windows D3D11 / macOS Metal / Linux OpenGL / WebGL）。

- **桌面三平台**：✅，且**这是极少数直接写 D3D11/Metal 的 Rust UI 框架**（`libs/render`、`libs/vulkan`、`libs/raytrace`），性能上限高。Linux 依赖 GStreamer/GL 一堆系统库（README 列出 apt 依赖清单），发行版打包会比 Slint 麻烦。
- **Android / iOS**：✅ 官方工具链 —— `cargo install --path=./tools/cargo_makepad`，然后 `cargo makepad android --abi=all install-toolchain` / `cargo makepad apple ios install-toolchain`。仓库里有专门的 `apps/wm-android` 与 `apps/wm-android/launch`，说明 Android 是**被实际出货验证的**（且 issue #1177 讨论的是"让 Android APK 能在 16KB page 设备上被 Google Play 接受"，属于上架级细节，说明确实有人在用它上架）。**iOS 有一个阻断级 open bug**：`Bug: iOS TextInput crashes when focusing any field`（2026-06-16，open）—— 对"文字消息"功能是硬伤。
- **是否纯 Rust 写 UI**：是（Rust 为主 + 自有 DSL）。
- **许可证**：MIT OR Apache-2.0（`makepad-widgets` / `makepad-platform` crate 元数据），无冲突。注意仓库的 GitHub license 字段显示 MIT。
- **实时视频帧 —— 关键结论：内部已经零拷贝，但公开 API 还没有**
  - 这是全表最讽刺的一条。仓库 open issue **`Proposal: public "adopt external GPU texture" API for Makepad Texture`（#1153，2026-07-30）** 的原文写着：Makepad **内部今天已经在做这件事** —— "`VideoExternal` on Windows, `OES` / blit on Android, `CVMetal` / `SharedBGRAu8` / `IOSurface` on Apple"，**缺的只是一个稳定的、面向应用的公开 API**，让应用自有的解码器/摄像头/GPU 特效把外部 GPU surface 借给 `DrawVars::set_texture` 采样而不做 CPU 往返。issue 至今 **0 条评论**，仍 open。
  - 同一份材料也说明：Makepad 官方示例里的视频播放是**内部实现**（`libs/video_flow`、`libs/mp4_index`、`libs/audio_decode`、`apps/video`），第三方应用要复用需要改 Makepad 源码或等这个 API 落地。
  - 退路是 `Texture` 的像素上传路径（可行但不是零拷贝）。
- **打包与构建链路**：`cargo makepad`（仓库内工具，crates.io 上的 `cargo-makepad` 停在 0.4.0/2023，**不要用那个**）统一处理 Android/iOS/tvOS/wasm 工具链；Android 已支持 AAB 与静态链接 std 以满足 Play 的 16KB page 要求。Project Robius 组织下的 `cargo-mobile2` 由 tauri-apps 接管并仍在维护（0.22.5，2026-08-17，近 90 天 36.9 万下载），可作通用 Rust-on-mobile 脚手架。
- **生态与维护**：7,141★、仅 33 open issue、2026-10-05 push —— **维护很活跃**，issue 少说明用户基数小而非质量高。crates.io 上的 `makepad-widgets` 停在 1.0.0（2025-05-13），**意味着 crate 发布严重滞后于仓库**，实际使用需要 `git` 依赖，这对 CI 可复现性是负面因素。近期仓库方向明显偏向 AI/3D/Studio（`libs/ai/*`、`libs/csg/*`），UI 成熟度投入不确定。
- **主要风险**：① 外部 GPU 纹理公开 API 缺失且有 0 评论的 open 提案 —— 视频需求要么等上游，要么 fork；② iOS 文本输入崩溃未修；③ crate 发布滞后，需 git 依赖；④ 生态/文档/示例量远小于 Slint 与 Dioxus；⑤ Windows CI 上 Android 链接问题（issue #1177 已修）。

**证据**：[makepad ISSUE #1153 "adopt external GPU texture"](https://github.com/makepad/makepad/issues/1153)（正文经 GitHub API 核实）· [makepad ISSUE #1177 Android 16KB page / Play 上架](https://github.com/makepad/makepad/issues/1177) · [makepad README（工具链）](https://github.com/makepad/makepad) · [crates.io makepad-widgets](https://crates.io/crates/makepad-widgets) · [crates.io cargo-mobile2](https://crates.io/crates/cargo-mobile2)

---

### 2.4 Ribir

- **状态**：**基本停滞**。GitHub 最后 push **2026-04-21**（距今约 5.5 个月），1,733★、35 open issue。crates.io 稳定版 `0.3.0`，最新是 `0.4.0-alpha.65`（2026-04-21），近 90 天下载仅 **403 次**。
- **桌面**：Windows/macOS/Linux 理论支持（wgpu 渲染），国内社区曾有一定讨论。
- **Android / iOS**：**没有任何官方支持证据**（未在仓库/文档中找到移动端目标或工具链）。按 GitHub issue 搜索也没有有效的移动端实现线索。
- **纯 Rust UI**：是。
- **许可证**：MIT。
- **视频帧**：使用 wgpu，但**没有公开的"导入外部纹理"API 文档证据**（未核实其内部 API）。即使有，也要自己写。
- **结论**：**不建议**。维护停滞 + 无移动端 + 生态极小，对五平台项目是最差组合。

**证据**：[crates.io ribir](https://crates.io/crates/ribir) · [lib.rs ribir_gpu](https://lib.rs/crates/ribir_gpu) · [Ribir 官网文档](https://ribir.org/docs/introduction)

---

### 2.5 egui / iced / Freya / Xilem / vizia

#### egui（+ eframe）
- **桌面**：✅ 三平台；Wayland 有正式的 `wayland` feature（默认启用，官方注释"为 Linux 编译必须启用"）。eframe 0.36.2（2026-09-08）。
- **移动端**：**eframe 明确提供 Android feature**：`android-game-activity` 与 `android-native-activity`（经 `egui-winit`）。**iOS 没有任何官方支持** —— 对五平台项目这是致命缺口，需要自己写 winit/iOS 胶水。这也是 egui 从"视频最舒服"跌出前二的原因。
- **纯 Rust UI**：是。
- **许可证**：MIT OR Apache-2.0。
- **视频帧 —— 全表最方便**：
  - wgpu 路径：`egui_wgpu::Renderer::register_native_texture(device, texture: wgpu::TextureView, filter: FilterMode) -> TextureId`，以及 `update_egui_texture_from_wgpu_texture`、`free_texture`。**直接把已解码帧的 `wgpu::TextureView` 挂上去即可，零拷贝**。
  - OpenGL 路径：`egui_glow::Painter::register_native_texture` / `replace_native_texture`（外部 GL 纹理 ID）。
  - 另有 `egui::PaintCallback` 允许在 egui 的 pass 里插入自定义渲染。
- **生态**：30,850★、1,109 open issue（数量高但有大量是讨论/需求）、crates.io 累计 2,520 万次下载（近 90 天 585 万）—— **下载量在所有 Rust UI 中碾压性第一**，Rerun 等项目在生产使用。
- **风险**：① 无 iOS；② 即时模式 GUI 做"移动端 App 级"界面（滚动、列表、动画）体验不如保留式框架；③ 移动端无障碍（AccessKit 在 Android 的支持程度未核实）。
- **证据**：[egui-wgpu Renderer API](https://docs.rs/egui-wgpu/latest/egui_wgpu/struct.Renderer.html) · [egui_glow Painter API](https://docs.rs/egui_glow/latest/egui_glow/painter/struct.Painter.html) · [eframe 0.36.2 feature flags](https://docs.rs/eframe/latest/eframe/)

#### iced
- **桌面**：✅ 三平台；官方 README 明确写 **"Cross-platform support (Windows, macOS, Linux, and the Web)"** —— **没有移动端**，且自我定位为 "Iced is currently experimental software"。
- **移动端**：无官方目标；仓库顶层只有 `core/graphics/renderer/runtime/tiny_skia/wgpu/widget/winit` 等，无 android/ios 目录。
- **视频帧**：`iced::widget::image::Handle` 只有 `Path` / `Bytes` / `Rgba{width,height,pixels}` 三种 —— **纯 CPU 像素路径**，`iced_wgpu::Renderer` 也只有 `load_image`/`draw_primitive`，没有"导入外部纹理"的公开方法。自研可行（`examples/custom_shader` 展示自定义 wgpu pipeline/primitive）但要从零写。
- **维护**：31,670★、0.14.0 发布于 2025-12-07（**已近 10 个月无稳定版**，但 2026-10-05 仍在 push，说明 0.15 在长周期开发中）。
- **结论**：桌面单平台视频（如 COSMIC 桌面生态）是它的主场，五平台项目不合适。
- **证据**：[iced README](https://github.com/iced-rs/iced) · [iced_wgpu Renderer](https://docs.rs/iced_wgpu/latest/iced_wgpu/struct.Renderer.html) · [iced image::Handle](https://docs.rs/iced/latest/iced/widget/image/enum.Handle.html)

#### Freya
- 基于 Skia 的类 React 声明式框架，3,207★、MIT、维护中（`0.5.0-rc.9` 发布于 **2026-10-04**，非常新）。桌面三平台。**Android/iOS 无任何支持证据**，视频帧能力无公开纹理导入 API 证据（未核实）。定位是"桌面优先的 React 风格"，移动端与实时视频都不适合。

#### Xilem
- Linebender 的**实验性**框架（仓库描述自称 "An experimental Rust native UI framework"），5,552★、Apache-2.0、0.4.0（2025-10-29）、最后 push 2026-09-14。桌面三平台。移动端只有**社区脚手架**（`johanholmerin/xilem-cross-platform`），非官方。视频帧能力无证据。**今天不能用于产品**。

#### vizia
- 2,322★、MIT、0.4.0（2026-04-23）、最后 push 2026-08-21。桌面三平台。无移动端、无视频纹理证据、社区规模小（近 90 天下载 573 次）。**不建议**。

---

### 2.6 Tauri v2

- **桌面**：✅ 三平台，生态最大（111,602★）。Linux 的图形问题较多，官方专门有 [Linux Graphics Issues](https://v2.tauri.app/develop/debug/linux-graphics/) 页面，说明 WebKitGTK/驱动组合下的坑是已知常态。
- **Android / iOS**：**官方支持**，且文档是一等公民：官方分发文档里同时有 [App Store](https://v2.tauri.app/distribute/app-store/) 和 [Google Play](https://v2.tauri.app/distribute/google-play/) 章节，签名文档覆盖 iOS/Android，插件生态里有 barcode-scanner、biometric、haptics、NFC 等移动专属插件。移动端接入需要把 crate 改成 `crate-type = ["staticlib","cdylib","rlib"]` + `#[cfg_attr(mobile, tauri::mobile_entry_point)]`。**已知坑**（open，2026-09 仍在新增）：Android open issue **119 个**，iOS open issue **62 个**，其中比较扎眼的：
  - `[ios] Release build fails to link on Xcode 27: SwiftPM's default build system gives non-public @_cdecl functions local visibility`（2026-09-24）
  - `[ios] Latest xcode requires functionality that's not supported right now by Tauri`（2026-09-23）
  - `[android] emit on the event-loop thread deadlocks the UI thread`（2026-09-29）
  - `[android] Activity callback may be lost, causing the invoke to hang`（2026-09-23）
  - `[android] build fails on Windows hosts without Developer Mode: jniLibs symlink needs SeCreateSymbolicLinkPrivilege`（2026-09-22）
- **是否纯 Rust 写 UI**：**否** —— Rust 是后端，UI 是 HTML/CSS/JS，渲染在系统 WebView 里。Rust 核心 + Web UI 的对照方案。
- **许可证**：MIT OR Apache-2.0。**但要注意非代码层面的约束**：iOS 只能用 WKWebView（Apple 强制），Android 用系统 WebView —— 平台条款与 WebView 版本碎片化是运维问题，不是许可证问题。
- **实时视频帧 —— 关键结论：不适合**
  - Rust 侧解码后要进 WebView 只能靠 IPC（`invoke`/event）或自定义协议 → `Blob`/`data:` URL → `<canvas>`。IPC 的 JSON/序列化开销 + JS 侧反序列化 + texImage2D 上传，**每帧都是多次拷贝**。
  - 更现实的做法是**放弃 Rust 解码**，改为 JS 侧 WebCodecs 解码（Android WebView/Chrome 支持较好，WKWebView 上 WebCodecs 支持较晚）—— 那就等于把实时视频这条技术护城河让给浏览器，与"Rust 核心"的初衷相悖。
  - 因此 Tauri 适合"以 Web 技术为主、视频是次要功能"的产品，**不适合 60fps 1080p+ 远程桌面/投屏为主业的 SecRelay**。
- **打包与构建链路**：`tauri-cli` 2.12.1（与核心同步发版）极其成熟，支持签名/公证/商店分发/CI pipeline 文档；Windows 上 Android 构建需要开启开发者模式（symlink 权限）。**这是全表打包链路最省心的一项**。
- **风险**：① 视频实时性是结构性缺陷；② 三套 WebView 引擎（WebView2/WKWebView/WebKitGTK）行为差异，UI 一致性成本；③ iOS 与最新 Xcode 的兼容问题正在发生；④ 移动端包体与内存基线高于原生。

**证据**：[Tauri v2 迁移/移动端准备](https://v2.tauri.app/start/migrate/from-tauri-1/) · [Linux Graphics Issues](https://v2.tauri.app/develop/debug/linux-graphics/) · [App Store 分发](https://v2.tauri.app/distribute/app-store/) · [Google Play 分发](https://v2.tauri.app/distribute/google-play/) · [crates.io tauri](https://crates.io/crates/tauri)

---

### 2.7 Flutter + flutter_rust_bridge（"Rust 核心 + 成熟跨平台 UI"对照方案）

- **平台**：Windows / macOS / Linux / Android / iOS **全部生产级**，这是五平台要求下唯一"无需论证"的 UI 层。
- **是否纯 Rust UI**：**否**。UI 是 Dart/Flutter widget，Rust 通过 FFI 承担网络、解码、加密等核心逻辑。
- **许可证**：Flutter **BSD-3-Clause**；`flutter_rust_bridge` **MIT**（2.13.0 稳定 / 2.14.0-beta.2，2026-09-12，累计 761 万次下载，近 90 天 170 万，仓库 5,440★、35 open issue，**维护非常健康**）。与 GPLv3 无冲突。
- **实时视频帧 —— 关键结论：有成熟的第三方基建，但桌面端要自己补**
  - Flutter 官方 `Texture` widget 的文档只引用两个注册表：**Android** 的 `TextureRegistry`（`SurfaceProducer` / `SurfaceTexture`）与 **iOS** 的 `FlutterTexture` 协议。这意味着**移动端外部纹理是官方一等公民**：Android 拿 `Surface` → MediaCodec 直接解码到它；iOS 实现 `FlutterTexture` → VideoToolbox/CVPixelBuffer 零拷贝上屏。
  - **桌面端不是官方能力**，但有成熟的 Rust 桥：**`irondash_texture`**（Rust Bindings for Flutter External Textures，MIT，0.5.0 稳定 / 0.6.0-dev.0 于 2026-08-05）明确提供：
    - `BoxedIOSurface` —— macOS / iOS 的 IOSurface 纹理
    - `BoxedGLTexture` —— Linux
    - `BoxedTextureDescriptor<ID3D11Texture2D>` 与 `BoxedTextureDescriptor<DxgiSharedHandle>` —— **Windows**（正是 D3D11VA/Media Foundation 解码产出的东西）
    - Android 侧则从 texture 里申请 JNI `Surface` 或 NDK `ANativeWindow`
    - 另提供 `PayloadProvider` + `mark_frame_available()` + `SendableTexture`（可跨线程），`BoxedPixelData` 作为全平台退路。
    - **需要注意**：该 crate 下载量偏小（约 519 次/月），稳定版 0.5.0 是 2023-12 的版本，0.6.0 仍是 dev —— **能解决架构问题，但需要接受"自担维护风险"**。
  - Windows 上还有一个已知的 Flutter 引擎侧问题：`[Windows][Impeller] Snapshotting a GPU-surface external texture calls ResolveTextureSkia and crashes`（flutter/flutter issue #190774）—— 说明桌面外部纹理在 Impeller 迁移期仍有工程坑（该 issue 正文未核实，来自搜索结果标题）。
- **flutter_rust_bridge 的能力边界**：`ZeroCopyBuffer<Vec<u8>>`（或 `zero-copy` cargo feature）确实让 Rust→Dart 的大块字节**零拷贝**（Web 平台除外，会退化为拷贝）。但**它只解决"跨语言传指针"，不解决"上屏"** —— Dart 侧收到的 `Uint8List` 仍要交给 `ui.decodeImageFromPixels` 或 `Texture` 才能显示。所以：**小窗口/低帧率可用 `ZeroCopyBuffer`+`RawImage`；高帧率必须走 `irondash_texture`/TextureRegistry 的 GPU 路径**。
- **打包与构建链路**：成熟（`flutter build ios/android/windows/macos/linux`），签名、CI、商店发布都有官方文档；FRB 用 `flutter_rust_bridge_codegen` 生成胶水，Android 需 NDK + `cargo-ndk`，iOS 需 Xcode 工程（FRB 有官方模板）。`cargo-ndk` 4.1.2（2025-08-09，近 90 天 73.6 万下载）非常稳定。
- **生态与社区**：最大的一档。移动端 IME、无障碍、后台、崩溃率全部经过海量 App 验证（这是 Slint/Dioxus 完全不具备的"已被证明"属性）。
- **主要风险**：① **UI 层不是 Rust**，与"希望 Rust + 类 Flutter 的跨平台 UI 库"的原始技术偏好冲突；② 双语言心智负担 + FRB 代码生成链路升级成本；③ Dart FFI 边界调试（跨语言 stack trace、内存所有权）；④ 桌面外部纹理依赖 `irondash_texture` 这类小维护者项目。

**证据**：[Flutter Texture class 文档（只列 Android/iOS 注册表）](https://api.flutter.dev/flutter/widgets/Texture-class.html) · [irondash_texture README（各平台 payload 类型）](https://lib.rs/crates/irondash_texture) · [crates.io irondash_texture](https://crates.io/crates/irondash_texture) · [flutter_rust_bridge zero copy 文档](https://cjycode.com/flutter_rust_bridge/v1/feature/zero_copy.html) · [crates.io flutter_rust_bridge](https://crates.io/crates/flutter_rust_bridge) · [flutter/flutter issue #162273 OpenGL external textures for Windows](https://github.com/flutter/flutter/issues/162273)（标题经搜索核实）· [flutter/flutter issue #190774 Windows Impeller 外部纹理](https://github.com/flutter/flutter/issues/190774)（标题经搜索核实）

---

### 2.8 平台原生 UI（SwiftUI / Jetpack Compose / WinUI3 / GTK4）+ Rust 核心

- **平台矩阵**：WinUI3/Windows App SDK（Win）、SwiftUI+AppKit（macOS）、GTK4 或 Qt（Linux）、Jetpack Compose（Android）、SwiftUI（iOS）。**五平台全覆盖，且视频帧零拷贝是天然强项**：
  - iOS/macOS：`AVSampleBufferDisplayLayer`（直接吃 CMSampleBuffer）或 `MTKView` + `CVMetalTextureCache` —— 这是所有视频 App 的标准做法，延迟最低。
  - Android：`SurfaceView` + MediaCodec output Surface，或 `TextureView` + SurfaceTexture；Compose 用 `AndroidView` 包进去。
  - Windows：`MediaPlayerElement`/`Media Foundation` + DirectComposition/`SwapChainPanel`。
  - Linux：`GtkGLArea` + GStreamer/VA-API。
- **Rust 核心接入方式与维护状态**：
  - `uniffi` **0.32.2**（2026-09-23，MPL-2.0，累计 1,358 万下载，近 90 天 459 万）—— **接口定义语言自动生成 Kotlin/Swift/Python 绑定，当前最主流的跨平台原生绑定方案，维护健康**。
  - `jni` **0.22.4**（2026-03-16，MIT/Apache-2.0，累计 **2.0 亿**下载）—— Android 事实标准。
  - `swift-bridge` **0.1.59**（2026-01-06，Apache-2.0/MIT，累计 209 万下载）—— iOS 绑定生成器，更新节奏明显慢于 uniffi（**7 个月无新版本**），生态位正在被 uniffi 挤压。
  - `objc2` **0.6.4**（2026-02-26，MIT，1.25 亿下载）—— 直接调 Apple 框架。
  - `flutter_rust_bridge` 也可用于纯 Dart 场景，但本方案里用不到。
- **许可证**：各平台 SDK 均为专有/宽松许可，Rust 绑定库为 MIT/Apache-2.0/MPL-2.0。**唯一需要留意的是 GTK4（LGPLv3）**：动态链接对 GPLv3 项目无问题，但 GTK4 在 Android/iOS 上事实上不可用（GTK4 有实验性 Android 后端，生态基本为零），所以 Linux 桌面用 GTK4、移动端仍需另写两套。
- **成本**：**UI 代码要写 4~5 遍**（SwiftUI 两遍、Compose 一遍、WinUI/C# 或 Rust 原生窗口一遍、GTK4 一遍），CI 要维护 5 套签名打包流水线。对 SecRelay 这种功能面大的产品，这是最大的人力成本项。
- **风险**：① 人力/工期成本是全表最高的（UI 层无复用）；② 多套 UI 导致交互不一致；③ WinUI3 是 C#/C++ 生态，Rust 只能通过 C ABI/`windows` crate 做后端逻辑，Rust 侧代码复用度高但 UI 侧完全不复用；④ 若坚持 Rust 写 Windows 原生 UI，`winui` crate 目前仍是 **0.0.0（2022-04-22）**，实际上不存在可用的 Rust WinUI 绑定 —— 只能走 `windows` crate 直调 Win32/DirectComposition，工作量更大。

**证据**：[crates.io uniffi](https://crates.io/crates/uniffi) · [crates.io swift-bridge](https://crates.io/crates/swift-bridge) · [crates.io jni](https://crates.io/crates/jni) · [crates.io windows](https://crates.io/crates/windows) · [crates.io winui（0.0.0，2022）](https://crates.io/crates/winui)

---

## 3. 横向专题

### 3.1 视频帧显示能力横向排名（SecRelay 的第一约束）

| 排名 | 方案 | 机制 | 1080p60 | 4K60 | 备注 |
|---|---|---|---|---|---|
| 1 | **平台原生** | AVSampleBufferDisplayLayer / SurfaceView / DirectComposition | 轻松 | 轻松 | 但 UI 要写 4~5 套 |
| 2 | **Flutter** | Android `SurfaceProducer`/`SurfaceTexture`、iOS `FlutterTexture`、桌面 `irondash_texture`（D3D11/DXGI/IOSurface/GL） | 轻松 | 轻松 | 桌面依赖第三方 crate |
| 3 | **egui** | `egui_wgpu::Renderer::register_native_texture(wgpu::TextureView)` / `egui_glow::Painter::register_native_texture` | 可 | 可 | 无 iOS，方案作废于平台而非性能 |
| 4 | **Slint** | `set_rendering_notifier` + `GraphicsAPI::{NativeOpenGL,WGPU29,WGPU30}`；`Image::from_borrowed_gl_2d_rgba_texture`；官方 `examples/wgpu_texture`、`examples/opengl_texture` | 可（自研 wgpu 桥） | 需自研 | Windows 无 D3D11 借用 API，macOS 无 Metal 借用 API |
| 5 | **Makepad** | 内部已有 `VideoExternal`/`OES`/`CVMetal`/`IOSurface`，但**无公开 API**（issue #1153） | 上游/ fork 后才能 | 同左 | 需要改框架源码 |
| 6 | **iced** | 自定义 shader/primitive 可行，image 只有 CPU `Rgba` | CPU 上传 | 否 | |
| 7 | **Dioxus / Tauri（WebView）** | Rust→IPC/blob→JS→texImage2D；或干脆 JS 侧 WebCodecs | 否 | 否 | 结构性缺陷 |

> 数量感：1080p RGBA 每帧 8.29 MB，60fps = **498 MB/s**；4K RGBA 每帧 33.2 MB，60fps ≈ **1.99 GB/s**。走 CPU 上传意味着把这条带宽压在上传路径上（外加解码器下载一次），在移动端尤其会显著推高功耗与发热。**"能否零拷贝"应当被当作 SecRelay 的一等选型标准**。

### 3.2 移动端就绪度

| 候选 | Android | iOS | 上架风险 |
|---|---|---|---|
| Flutter | 生产级 | 生产级 | 低 |
| 平台原生 | 生产级 | 生产级 | 低 |
| Tauri v2 | 官方支持，119 个 open issue | 官方支持，62 个 open issue；最新 Xcode 兼容问题 | 中 |
| Slint | 官方支持；**IME/SwiftKey/软键盘/redraw 5+ 个 open issue** | 官方支持（`docs/ios.md`），文档较少 | 中 |
| Dioxus | 官方支持；白屏 race、链接失败、Windows 主机链接失败 | 官方支持；同上有"webview 空白/冻结"跨平台 bug | 中高 |
| Makepad | 官方工具链，已出货（有 Play 16KB page 相关修复） | 有工具链，**TextInput 聚焦即崩溃未修（2026-06-16 open）** | 高（iOS） |
| egui/eframe | 官方 feature（`android-native-activity`/`android-game-activity`） | **无** | — |
| iced | 无 | 无 | — |
| Xilem/vizia/Freya/Ribir | 无（Xilem 仅社区脚手架） | 无 | — |

### 3.3 许可证与 GPLv3 + App Store（**本项目最容易踩法律坑的地方**）

**各候选许可证**（均经 crates.io `versions[0].license` 字段核实）：

| 候选 | 许可证 | 与 GPLv3 项目兼容 | copyleft 传染 |
|---|---|---|---|
| Slint | `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0` | ✅ 直接选 GPLv3 档 | 无额外负担（本项目本来 GPLv3） |
| Dioxus | MIT OR Apache-2.0 | ✅ | 无 |
| Makepad | MIT OR Apache-2.0 | ✅ | 无 |
| Ribir | MIT | ✅ | 无 |
| egui / eframe | MIT OR Apache-2.0 | ✅ | 无 |
| iced | MIT | ✅ | 无 |
| Freya | MIT | ✅ | 无 |
| Xilem | Apache-2.0 | ✅ | 无 |
| vizia | MIT | ✅ | 无 |
| Tauri | Apache-2.0 OR MIT | ✅ | 无 |
| Flutter | BSD-3-Clause | ✅ | 无 |
| flutter_rust_bridge | MIT | ✅ | 无 |
| irondash_texture | MIT | ✅ | 无 |
| uniffi | MPL-2.0 | ✅（文件级 copyleft，可动态/静态链接） | 仅限其自身文件 |
| GTK4 | LGPL-3.0 | ✅（动态链接） | 无 |

**Slint 三档授权对本项目的准确含义**（据官方 `LICENSE.md` 与 Royalty-Free 2.0 全文）：
1. **GPLv3 档**：免费，用于开源软件；官方措辞是"Permits use in open source software under GPL-compatible terms, at no cost, for desktop, mobile, and web applications, as well as for embedded systems"，且"your own files can stay MIT or Apache-2.0"（即你自己的代码不必被迫 GPL，但**发布的二进制整体必须是 GPLv3**）。**SecRelay 已经是 GPL-3.0-only，所以这一档是零成本、零附加义务的天然匹配。**
2. **Royalty-Free 2.0 档**：免费、允许专有桌面/移动/Web 应用，**排除嵌入式**；条件是归因 —— 二选一：(a) 在可从顶层菜单到达的 About 界面里放 `AboutSlint` widget；(b) 展示 Slint attribution badge。注意它是**并列选项（OR）**，选它并不要求你放弃 GPLv3 源码许可，但会给"完全自由的 GPLv3 二进制"引入一个非 GPL 的归因条件 —— 对一个以 GPLv3 为立场的项目没必要。
3. **商业档**：本项目不需要。

**⚠️ 真正的法律风险不在 UI 框架，而在 "GPLv3 + Apple App Store"**：
- 2010-2011 年 VLC for iOS 被从 App Store 下架的著名事件，根因就是 **App Store 的使用条款与 GPL 不兼容**（Apple 对已分发应用施加额外限制，与 GPL 的"不得附加限制"以及 GPLv3 的反 Tivoization 条款冲突）。FSF 的 Licensing Compliance Lab 曾就此发布执法说明；GNU 项目自己的 FreeDink 也因同一原因明确放弃 iPhone 移植。
- 这意味着：**如果 SecRelay 以 GPLv3 发布，iOS 上架路径存在真实的许可证冲突风险**，与选哪个 UI 框架无关。GPLv3 档位的 Slint 只是让你"进入"这个既有问题，并不会加剧它；但也不能靠换框架解决它。
- 可行的规避方向（需要法务确认，本文不构成法律意见）：① 对 iOS 分发采用 **Slint Royalty-Free 档位 + 放宽 iOS 目标平台的源码许可**（即 iOS 版不作为 GPLv3 二进制分发，而桌面/Android 版继续 GPLv3）—— 这是把"UI 库许可"与"应用许可"解耦的常见做法；② 与 SixtyFPS GmbH 谈商业授权；③ iOS 侧改为平台原生 UI（SwiftUI）并采用宽松许可，仅桌面/Android 为 GPLv3（此时 UI 代码本就要分叉，许可证分叉的成本相对可接受）；④ 接受"iOS 不上架，只走 TestFlight/侧载/企业分发"。
- **建议**：把"GPLv3 的 iOS 上架合规策略"作为独立于 UI 选型的**前置法务议题**立项，不要等 UI 定完再处理。

**证据**：[Slint LICENSE.md（三档选择与"your own files can stay MIT or Apache-2.0"）](https://raw.githubusercontent.com/slint-ui/slint/master/LICENSE.md) · [Slint Royalty-Free 2.0 全文（归因条款）](https://raw.githubusercontent.com/slint-ui/slint/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md) · [Slint 三档授权表（官方博客）](https://slint.dev/blog/making-slint-desktop-ready.html) · [Slint 定价页](https://slint.dev/pricing) · [GNU Savannah：GPL Apps and Apple AppStore（含 FSF 执法说明与 VLC 案例）](https://savannah.gnu.org/forum/forum.php?forum_id=6582) · [VLC 因 DRM 争议被下架报道](https://www.slashgear.com/apple-pulls-vlc-from-app-store-over-open-source-drm-dispute-08124911/)

### 3.4 打包与构建链路难度

| 候选 | 桌面 | Android | iOS | CI 难度 |
|---|---|---|---|---|
| Tauri v2 | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐（官方，Windows 需开发者模式） | ⭐⭐⭐（最新 Xcode 有兼容坑） | 低（文档最全） |
| Flutter | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | 低 |
| egui/eframe | ⭐⭐⭐⭐⭐ | ⭐⭐⭐（eframe feature + `android-activity`） | ✖ | 中 |
| Slint | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐（`cargo apk2` 1.4.2） | ⭐⭐⭐（Xcode 工程 + `cargo-xcode`） | 中 |
| Dioxus | ⭐⭐⭐⭐⭐ | ⭐⭐⭐（`dx bundle`，但 Windows 主机链接有坑） | ⭐⭐⭐ | 中 |
| Makepad | ⭐⭐⭐⭐ | ⭐⭐⭐⭐（`cargo makepad android`，已修 16KB page） | ⭐⭐⭐（工具链齐，但有崩溃 bug） | 中高（需 git 依赖） |
| 平台原生 | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | **极高（5 套流水线）** |
| Xilem/vizia/Freya/Ribir/iced | ⭐⭐⭐⭐ | ✖ | ✖ | — |

### 3.5 维护活跃度汇总（2026-10-06 快照）

| 候选 | Stars | Open issues | 最后 push | 最新稳定版 | 90 天下载 |
|---|---|---|---|---|---|
| Tauri | 111,602 | 1,484 | 2026-10-05 | 2.12.1（2026-10-01） | 13,323,942 |
| Dioxus | 39,322 | 815 | 2026-10-03 | 0.7.10（2026-07-30） | 1,098,863 |
| iced | 31,670 | 504 | 2026-10-05 | 0.14.0（2025-12-07） | 623,484 |
| egui | 30,850 | 1,109 | 2026-10-05 | 0.36.2（2026-09-08） | 5,851,867 |
| Slint | 24,066 | 822 | 2026-10-05 | 1.18.1（2026-09-21） | 548,066 |
| Makepad | 7,141 | 33 | 2026-10-05 | 1.0.0（2025-05-13，**crate 滞后**） | 1,338 |
| Xilem | 5,552 | 158 | 2026-09-14 | 0.4.0（2025-10-29） | 2,455 |
| Freya | 3,207 | 41 | 2026-10-05 | 0.4.3 / 0.5.0-rc.9（2026-10-04） | 8,136 |
| vizia | 2,322 | 66 | 2026-08-21 | 0.4.0（2026-04-23） | 573 |
| Ribir | 1,733 | 35 | **2026-04-21** | 0.3.0 / 0.4.0-alpha.65 | **403** |
| flutter_rust_bridge | 5,440 | 35 | 2026-10-05 | 2.13.0（2026-09-12） | 1,706,396 |

---

## 4. 最终推荐

### 推荐第 1 名：**Slint**（若"纯 Rust UI"是硬约束）
理由：**唯一同时满足"纯 Rust UI + 官方 Android/iOS 支持 + GPLv3 原生契合 + 官方 GPU 纹理导入示例 + 商业公司持续维护"的方案**；GPLv3 档位让本项目零成本、零归因义务，且 `examples/wgpu_texture` + `examples/ffmpeg` 已经给出了自研零拷贝视频链路的起点。
必须先验证的三件事：① Windows（D3D11VA）与 macOS（VideoToolbox）到 wgpu 的纹理导入自研工作量；② Android 上的 IME/SwiftKey issue 是否影响文字消息功能；③ iOS 上架与 GPLv3 的合规方案（见 §3.3）。

### 推荐第 2 名：**Flutter + flutter_rust_bridge**（若"能上架、能交付"优先于"纯 Rust UI"）
理由：五平台（含 iOS）生产级就绪，视频零拷贝在移动端是官方能力（`SurfaceProducer`/`FlutterTexture`），桌面端有 `irondash_texture` 补齐 D3D11/DXGI/IOSurface/GL 四种 GPU 纹理导入，FRB 的 `ZeroCopyBuffer` 解决跨语言大数据拷贝；代价是 UI 层用 Dart，与"Rust UI"的原始偏好相悖。

> 落选说明（简述）：**egui** 视频能力最强、许可证最干净，但因**无 iOS 支持**直接出局；**Makepad** 架构最"对症"（内部本就零拷贝且自绘 D3D11/Metal），但公开纹理导入 API 仍是 0 评论的 open 提案（#1153）、iOS 文本输入崩溃未修、crate 发布滞后于仓库，风险高于 Slint；**Dioxus/Tauri** 的 WebView 架构与 60fps 1080p+ 实时视频结构性冲突；**iced/vizia/Freya/Xilem/Ribir** 均无移动端或维护不足；**平台原生**是工程质量最优解但 UI 要写 4~5 遍，人力成本最高，可作为"iOS 合规避险 + 性能兜底"的备选组合拳。

---

## 5. 待核实 / 本文局限

1. `github.com` 在本机被 DNS 劫持，**issue 与 PR 的正文未能逐字抓取**；文中所有 issue 的**标题、状态、创建日期、编号**均通过 GitHub REST API 核实，属于权威元数据，但正文细节（如具体崩溃堆栈）请在上手前二次确认。
2. `flutter/flutter` 的 issue #162273（Windows OpenGL external textures）与 #190774（Windows Impeller 外部纹理快照崩溃）**仅核实了标题**，正文未读。
3. `iced` 是否有未公开/未文档化的外部纹理导入路径、`vizia`/`xilem`/`freya` 的视频帧能力、`Ribir` 的 wgpu 纹理 API，**均未找到公开证据**，按"不具备"处理。
4. Wayland 下的具体可用性：Slint（winit backend）与 egui/eframe（`wayland` feature 默认启用）证据较硬；Tauri 有官方 Linux 图形问题专页，说明坑是常态；其余框架仅确认"支持 Linux"，未逐项核实 Wayland 专有 bug。
5. Slint 的移动端与桌面端**签名/公证/商店审核**流程未逐项走查（官方文档存在，但未逐页精读）。
6. 本文不构成法律意见；§3.3 的 GPLv3 + App Store 结论请在正式立项前由法务确认。
