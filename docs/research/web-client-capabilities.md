# SecRelay Web 端能力边界与取舍（v0.1）

> 配套文档：`docs/需求分析.md` §3（一条通道三种流）、§4（角色矩阵）、D12（是否要浏览器端观看）
> 前提决策：**不做 iOS 原生 App**；新增 **Web 页面客户端**。目标是 Web 端"有的功能原生有、原生有的 Web 尽量有"，但承认 Web 有做不到的部分。
> 核实方式：MDN / Apple 官方文档逐项抓取原文核实；版本号取自 MDN 的 Baseline 标注与规格说明，标注 ⚠️ 处为需在目标浏览器实测确认的兼容性数字。

> ⚠️ **本阶段不做屏幕分享 / 远程看屏 / 投屏，依据留档。**
> 本文中 `getDisplayMedia`、"Web 当被看端"、投屏显示相关的能力边界只作为技术储备保留，
> **不代表当前需求**；摄像头源、文件、文字、实时语音与 Web 端整体能力评估不受影响。

---

## 0. 执行摘要

**一句话结论**：Web 端可以做到**观看、投屏、摄像头源、文件传输、文字消息、实时语音**这六件事（覆盖 FR-2~FR-10 的绝大部分），并且**在文件传输上不必退化**（WebTransport 已 Baseline，见 §6）；但**必须放弃**「后台静默传输/静默推流」「无人值守被看端」「本地路径与目录写入」「系统音频作为可靠源」这四类能力（对应 FR-7 的后台续传、FR-11 的后半段、D4）。

**Web 端的三个关键判断**：

1. **Web 不能是"被控端"**——不是"难"，是没有任何 API 可以注入鼠标键盘。这是与 iOS 同级的硬边界。
2. **Web 是"用户手势驱动"的客户端**——所有采集类 API（`getDisplayMedia`/`showSaveFilePicker`）都要求 transient user activation，且浏览器会持续显示"正在共享"提示。所以 Web 天然做不了"静默"和"常驻"。
3. **Web 反而解决了 iOS 用户的接入问题**——"不做 iOS 原生"的最大损失（iPhone/iPad 用户无法成为节点）被 Web 端**大幅补偿**：iOS 16.4+ 的 Safari 可以当完整的观看端 + 文件端 + 语音端。这个收益值得在需求文档里明确写下来。

**Web 端功能取舍总表**：

| 原生功能 | Web 能否做 | 取舍与理由 |
|---|---|---|
| 看别人屏幕（观看端） | ✅ **完全可做，且体验最好** | `<video>` + WebRTC 接收，浏览器自动硬解 |
| 投屏（全屏显示远程画面） | ✅ 可做，🟡 手机端受限 | 桌面浏览器可用 Fullscreen API；**iOS Safari 只能对 `<video>` 元素全屏**，不能对任意 `div` 全屏 |
| 看别人摄像头 | ✅ 可做 | 同观看端 |
| 把本机摄像头当源推给别人 | ✅ 可做 | `getUserMedia`，需授权 |
| 把本机屏幕当源推给别人 | 🟡 可做但体验差 | `getDisplayMedia` **每次都要用户手势 + 重新选源**，比 Android 的每会话授权更麻烦 |
| 系统音频采集（听对方电脑声音） | 🔶 仅 Chrome 系、仅"标签页/系统音频" | `getDisplayMedia({audio:true})`；**浏览器无法采集"其他应用"的音频**，且 Safari/Firefox 支持很差 |
| 麦克风（语音对讲） | ✅ 可做 | `getUserMedia({audio:true})` + Web Audio / `getUserMedia` 的 AEC/NS 约束 |
| 文件传输（前台） | ✅ 可做，**不必退化** | WebTransport（HTTP/3，Baseline 2026）+ OPFS 做分块与断点续传 |
| 文件传输（后台静默/关页续传） | ❌ **做不到** | 页面关闭即死；Service Worker 不能维持长连接；已确认这是你要砍掉的那类 |
| 文件写入任意本地路径 | ❌ 做不到 | 只能 `showSaveFilePicker()`（Chromium）或 Blob 下载到默认目录；**没有目录遍历、没有静默落盘** |
| 传目录（保留目录结构） | 🔶 部分可做 | `showDirectoryPicker()`（Chromium）；其余浏览器只能逐个文件 |
| 文字消息（前台） | ✅ 可做 | WebRTC DataChannel / WebSocket |
| 文字消息（异步可达/离线） | ❌ 做不到（不依赖推送的话） | 需 Web Push + Service Worker，且 **Safari 不支持"隐形推送"** |
| 桌面通知 | 🟡 可做，有硬约束 | Notifications API + Web Push；**iOS 需"添加到主屏幕"的 Web App**（iOS 16.4+） |
| 剪贴板互通 | 🔶 非常受限 | `navigator.clipboard` **需用户手势 + 页面获焦**；无法后台监听剪贴板变化 |
| 远程控制（被控端：注入鼠标键盘） | ❌ **做不到** | 浏览器无输入注入 API（见 §7） |
| 远程控制（主控端：发指令） | ✅ 可做 | 只是发送数据，注入由对端原生完成 |
| 后台常驻 / 开机自启 | ❌ 做不到 | 浏览器无此概念 |

---

## 1. Web 端的角色定位（更新 `docs/需求分析.md` §4 矩阵）

| 新角色 | Windows | Linux | macOS | Android | iOS | **Web** |
|---|---|---|---|---|---|---|
| 观看端 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ **（Web 的强项）** |
| 被看端（屏幕源） | ✅ | ✅ | ✅ | 🟡 | ❌ | 🟡 **仅前台、每次重选源** |
| 被看端（摄像头源） | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ **（推荐主用途）** |
| 被控端（注入输入） | ✅ | 🔶 | 🟡 | 🔶 | ❌ | ❌ **不可能** |
| 文件端 | ✅ | ✅ | ✅ | ✅ | ✅ | 🟡 **仅前台** |
| 语音端 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

> **产品含义**：Web 端的定位是**"免安装的观看端与文件端"**，并对 iPhone/iPad/Chromebook/受限设备提供唯一入口。Web 端可以顺带支持"把浏览器标签页共享出去"，但不应把它当作卖点（体验明显弱于原生被看端）。

---

## 2. 维度 1：Web 端屏幕采集

| 项 | 结论 |
| --- | --- |
| API | `navigator.mediaDevices.getDisplayMedia(options)` ✅ [MDN](https://developer.mozilla.org/en-US/docs/Web/API/MediaDevices/getDisplayMedia) |
| 安全上下文 | ✅ **必须是 HTTPS**（或 localhost）：「Secure context: This feature is available only in secure contexts (HTTPS)」 |
| 用户手势 | ✅ **必须由 transient activation 触发**：否则抛 `InvalidStateError` ——「the call to `getDisplayMedia()` was not made from code running due to a transient activation, such as an event handler」 |
| 粒度控制 | `video.displaySurface`（`browser` / `monitor` / `window`）、`monitorTypeSurfaces`、`preferCurrentTab`、`selfBrowserSurface`、`surfaceSwitching`、`audio`、`systemAudio`、`windowAudio`、`suppressLocalAudioPlayback` |
| 能否持久授权 | ❌ **不能**。每次调用都会弹"选择要共享的内容"对话框；浏览器还持续显示共享指示条 |
| 单一窗口 | ✅ 可（用户选 window）；`surfaceSwitching: "include"` 可让用户中途换共享对象而无需重启调用 |
| 音频 | `audio: true` 才请求；**但**「Browsers may ignore this hint… the returned stream might contain no audio track even when `audio` is true and `systemAudio` is `include`」⚠️ 兼容性需实测 |
| 移动浏览器 | 🔶 **大多不支持或支持很差**：Android Chrome 与 iOS Safari 对 `getDisplayMedia` 的支持长期不完整 ⚠️（需实测；这是"Web 当被看端在手机上行不通"的主因） |
| Baseline 状态 | ⚠️ **"Limited availability — This feature is not Baseline because it does not work in some of the most widely-used browsers."** ✅ MDN 原文 |

**与原生对比**：Web 的屏幕采集授权流程 ≈ Android 14+（每会话），但更啰嗦（必须用户手势 + 选源对话框 + 常驻共享条）。**因此"Web 当被看端"只适合临时演示，不适合常驻共享。**

---

## 3. 维度 2：Web 端摄像头与麦克风

| 项 | 结论 |
| --- | --- |
| API | `navigator.mediaDevices.getUserMedia({video, audio})` ✅ 同属 Media Capture and Streams |
| 授权 | 用户授权，按 origin 记忆；HTTPS 必需 |
| 约束调参 | `navigator.mediaDevices.getSupportedConstraints()` → `track.getCapabilities()` → `track.applyConstraints()`；`getSettings()` 读实际值 ✅ [MDN: Capabilities, constraints, and settings](https://developer.mozilla.org/en-US/docs/Web/API/Media_Capture_and_Streams_API/Constraints) |
| AEC/降噪 | 通过 `audio` 约束（`echoCancellation`、`noiseSuppression`、`autoGainControl`）请求，属可约束属性；浏览器可忽略 |
| 前后摄像头切换 | 桌面浏览器通常无；移动端可通过 `facingMode` 约束切换 |
| 后台 | ❌ 页面进后台/切标签，摄像头流可能被暂停或降级；iOS 尤其严格 |
| 已知坑 | ⚠️ iOS Safari 存在"前置摄像头 track 报告 landscape 但帧是 portrait"的旋转不一致问题（Apple 论坛 2026 案例），需在 Web 端做旋转归一化 |

> **建议**：把"Web 端当摄像头源（手机当电脑的高清摄像头）"作为 Web 的主推被看能力——它只需一次授权、无需每次选源、且手机浏览器普遍支持。

---

## 4. 维度 3：Web 端系统音频

| 路径 | 能拿到什么 | 限制 |
|---|---|---|
| `getDisplayMedia({audio:true})` | 共享**标签页**或**系统音频**（Chrome 系在共享显示器时可勾选"分享系统音频"） | 🔶 浏览器可忽略；Safari/Firefox 支持差；**必须与视频共享同一个用户手势会话** |
| `getUserMedia({audio:true})` | 麦克风 | 这不是"系统音频" |
| Web Audio API | 对已获得的流做处理（增益/滤波/录制） | ✅ Baseline，但**不能凭空捕获其他应用的输出** ✅ [MDN Web Audio API](https://developer.mozilla.org/en-US/docs/Web/API/Web_Audio_API) |

**关键取舍**：**当对方是 Web 客户端时，我方无法真正"听到对方电脑的声音"**——浏览器既没有 WASAPI loopback、也没有 PipeWire monitor 的等价物。Web 端只能：
- 共享**标签页音频**（范围极窄，用户得恰好共享那个标签页）；
- 或让**原生端**采集系统音频后推给 Web 端（推荐，把系统音频采集能力留在原生侧）。

> 因此：**"听对方电脑的声音"这条需求，Web 端只能作为接收方**。若对端是 Web，则该功能不可用，需在 UI 上明确降级提示。

---

## 5. 维度 4：Web 端编解码的控制力（一个容易被低估的约束）

这是 Web 端最本质的架构差异：**编解码器由浏览器掌控，我们只能"请求"，不能"实现"。**

| 能力 | 原生端 | Web 端 |
|---|---|---|
| 选编码器 | ✅ 直接调 NVENC/QSV/AMF/VAAPI/VideoToolbox/MediaCodec | 🔶 只能通过 SDP 协商 / `RTCRtpSender.setParameters()` 表达偏好 |
| 硬编硬解 | ✅ 自己控制 | ✅ **浏览器自动用硬件加速**，但不可见、不可控 |
| 码率/帧率控制 | ✅ 完全控制 | 🔶 `setParameters` 的 `maxBitrate`/`maxFramerate` + `applyConstraints` 的 `frameRate`/`width`/`height` |
| **脏矩形/瓦片差分**（`docs/research/transport-media.md` 的 R1 核心自研项） | ✅ 可实现 | ❌ **无法插入**。浏览器 WebRTC 编码管线是黑盒，拿不到"编码前"的帧 |
| 自定义抗丢包/FEC 策略 | ✅ 完全控制 | 🔶 浏览器自带 BWE/NACK/FEC 且不可替换 |
| 逐帧处理 | ✅ | 🟡 需绕开 WebRTC：`WebCodecs` 提供 `VideoDecoder`/`VideoEncoder`（硬件加速、逐帧）✅ [MDN WebCodecs](https://developer.mozilla.org/en-US/docs/Web/API/WebCodecs_API)，但**要把 WebCodecs 与 WebRTC/RTP 拼起来得自研 jitter buffer 与同步逻辑**，工作量大 |
| 编解码器可切换（D15 遗留） | ✅ | 🔶 WebRTC 常见组合 H.264 / VP8 / VP9 / AV1；**H.265 在浏览器端支持有明显缺口**（MDN 原文："H.265 (HEVC) … with significant gaps in browser support outside of Apple platforms"） |

**两条可选路线，必须二选一**：

| 路线 | 做法 | 优点 | 代价 |
|---|---|---|---|
| **A. 标准 WebRTC 端点**（推荐） | Web 端用浏览器原生 WebRTC；原生端用 `webrtc-rs` 与之对接 | 互通性最好、开发量最小、浏览器自带 BWE/FEC/硬编解 | **放弃脏矩形差分**在 Web 路径上的收益；Web 端画质/码率控制较粗 |
| **B. 自研媒体层 + WebCodecs** | 原生端自研编码，Web 端用 WebCodecs 解码（MSE 或自绘） | 可完整保留脏矩形差分等自研优化 | 需自建拥塞控制、丢包恢复、音视频同步；**且仍无法在 Web 端"编码"侧做特殊处理**（Web 端当源时仍是黑盒） |

> **建议选 A**，并接受"Web 路径不享受脏矩形优化"。理由：`docs/需求分析.md` §6.3 已定 `webrtc-rs`，选 A 是对现有决策**零改动**；而路线 B 会让 Web 端成为整个项目最大的不确定性来源。

---

## 6. 维度 5：Web 端文件传输（\***本节有一个能改变决策的发现**）

### 6.1 关键发现：WebTransport 已 Baseline，浏览器端可以走 QUIC

`docs/需求分析.md` 与 `docs/research/transport-media.md` 的结论是：**文件通道用独立 `quinn` QUIC 连接**（D11），并与媒体通道分离。这在原生端成立，**在 Web 端原本是最痛的点**（浏览器不能开裸 QUIC）。

但 **WebTransport API 已经 Baseline 2026**：

> 「**Baseline 2026 — Newly available.** Since March 2026, this feature works across the latest devices and browser versions.」…「The WebTransport API provides a modern update to WebSockets, transmitting data between client and server using **HTTP/3 Transport**. … It enables reliable transport via streams and unreliable transport via UDP-like datagrams.」✅ [MDN WebTransport API](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API)

并且 MDN 明确列出了 **Rust 服务端库 `wtransport`**，以及浏览器侧要点：

- scheme 必须是 **HTTPS**，端口必须显式指定；
- 服务端必须校验 **`Origin`** 头才能接受会话；
- 支持多个双向流、单向流、datagram（可靠 + 不可靠两种传输）。

**这意味着**：Web 端可以用 WebTransport 承载文件与消息通道，与原生端的 `quinn` 后端在**同一个 QUIC/HTTP-3 协议族**上统一。传输层不必为 Web 端做一套降级实现。

⚠️ **但要注意一个架构事实**：WebTransport 是 **client↔server** 模型，**不是 P2P**。所以：

- **Web 端与原生端之间**：若走 WebTransport，流量必须经过一台支持 HTTP/3 的中继服务器（**不可能是纯 P2P**）。
- **媒体走 WebRTC 则仍是 P2P**（ICE 打洞成功时）。

> **决策建议（新增 D18）**：Web 端参与的文件传输，接受"经中继"（零知识中继只转发密文，与既有中继零知识设计一致），代价是中继带宽成本上升——这直接冲击 `transport-media.md` 设的"中继占比 <15%"KPI。**必须把"Web 端文件传输"单独计入中继成本模型**。
> 备选：Web 端文件走 WebRTC DataChannel（可 P2P），牺牲吞吐。建议**按文件大小分流**：小文件走 DataChannel（P2P），大文件走 WebTransport（中继）。

### 6.2 文件写入本地：Web 的硬边界

| 能力 | API | 限制 |
|---|---|---|
| 打开单个文件 | `showOpenFilePicker()` | 🔶 Chromium 系；**需 transient user activation** |
| 保存到用户选定位置 | `showSaveFilePicker()` | 🔶 ⚠️ MDN 原文：「**Limited availability — This feature is not Baseline because it does not work in some of the most widely-used browsers.**」「**Experimental**」；「Transient user activation is required. The user has to interact with the page」✅ |
| 选择目录 | `showDirectoryPicker()` | 🔶 Chromium 系为主 |
| **浏览器私有缓存（可做分块/断点续传）** | **OPFS**：`navigator.storage.getDirectory()` | ✅ **Baseline「Widely available」**；「Permission prompts and security checks are **not** required to access files in the OPFS」✅ [MDN OPFS](https://developer.mozilla.org/en-US/docs/Web/API/File_System_API/Origin_private_file_system) |
| 旧式下载 | `Blob` + `<a download>` | ✅ 通用，但**只能落到浏览器默认下载目录，用户无路径控制** |

> **结论**：Web 端的文件传输应该设计为 **"OPFS 分块暂存（可断点续传）+ 最终一次性交付到用户选定位置"**。
> - **做不到**：写任意路径、静默落盘、后台续传、保留/复刻远端目录结构（非 Chromium）。
> - **能做**：断点续传的**技术内核**（OPFS 允许断点后继续写分块，正是官方列出的用例：「The app can restart uploads after an interruption, such as the browser being closed or crashing」）。
> - ⚠️ OPFS 受浏览器存储配额限制，**大文件（>数 GB）可能触顶**，需要 `navigator.storage.estimate()` 预检与提示。

---

## 7. 维度 6：Web 端输入注入（远程控制）

**结论：❌ 不可能。**

- 浏览器没有任何"向操作系统注入鼠标键盘事件"的 API（这是浏览器沙盒的核心安全边界，不存在待开放的等价物）。
- Screen Capture API 有 **Captured Surface Control API**（「allows the capturing application to provide limited control over the captured display surface, for example **zooming and scrolling** its contents」）✅ [MDN Screen Capture API](https://developer.mozilla.org/en-US/docs/Web/API/Screen_Capture_API) —— 但那是**对共享内容的缩放/滚动**，**不是任意鼠标键盘注入**，且支持面窄。
- 因此：**Web 端可以当"主控端"（把用户操作发成指令），但绝不能当"被控端"**。

> 对 `docs/需求分析.md` FR-11 的影响：远程控制的"被控端"必须限定为 **Windows / Linux / macOS 原生**（已核实的三平台），Android 为受限项，Web 与 iOS 永久排除。安全模型（强提示、可随时中断、白名单）只在原生侧实现。

---

## 8. 维度 7：Web 端语音、消息与通知

| 能力 | 结论 |
|---|---|
| 实时语音 | ✅ `getUserMedia({audio})` + WebRTC 音频轨；AEC/NS 通过约束请求；Web Audio API 可做后续处理 |
| 自动播放限制 | ⚠️ 浏览器 autoplay 策略：**音频播放通常需要一次用户手势解锁**。若对端推语音而用户没交互过页面，可能无声——必须设计"点击开始通话"的 UI |
| 文字消息 | ✅ DataChannel / WebSocket |
| 桌面通知 | 🟡 Notifications API + Service Worker + Push API；**需用户授权** |
| **iOS 通知的硬约束** | ⚠️ **必须"添加到主屏幕"的 Web App**：Apple 原文「Add web push to **Home Screen web apps** in iOS 16.4 or later and Webpages in Safari 16 for macOS 13 or later」✅ [Apple: Sending web push notifications](https://developer.apple.com/documentation/usernotifications/sending-web-push-notifications-in-web-apps-and-browsers) |
| **Safari 不支持隐形推送** | ⚠️ **Apple 原文**：「**Safari doesn't support invisible push notifications.** Present push notifications to the user immediately after your service worker receives them. If you don't, **Safari revokes the push notification permission for your site.**」——即不能把 Web Push 当作"静默信令传输"用 |
| 离线消息可达 | 🔶 依赖 Web Push（经浏览器厂商推送服务，如 APNs）。**与你"自建中继、零知识"的取向存在张力**：推送服务会看到元数据（至少有通知触发），且服务端要存订阅端点与 VAPID 密钥 |
| 剪贴板 | 🔶 `navigator.clipboard` 需用户手势 + 页面获焦；**无法后台监听剪贴板变化**（`clipboardchange` 事件支持面窄）→ FR-10 在 Web 端几乎不可用，建议 Web 端不做剪贴板互通 |

---

## 9. 维度 8：Web 端的后台与常驻限制

| 限制 | 说明 |
|---|---|
| **页面/标签关闭即终止** | 所有传输、会话、编码全部停止。**没有"后台静默传输"**——这正是你要砍的那类 |
| **后台标签节流** | 非活跃标签的定时器与渲染被节流；`requestAnimationFrame` 基本停摆 |
| **移动端切后台** | 浏览器进入后台后音视频采集可能被暂停甚至释放；iOS Safari 尤其严格 ⚠️ |
| **屏幕休眠** | 长时观看必须用 **Screen Wake Lock API**（`navigator.wakeLock.request("screen")`）：✅ **Baseline 2025（2025 年 3 月起）**；且「Only active documents can acquire screen wake locks and previously acquired locks are **automatically released when document becomes inactive**」→ **必须监听 `visibilitychange` 重新获取** ✅ [MDN Screen Wake Lock](https://developer.mozilla.org/en-US/docs/Web/API/Screen_Wake_Lock_API) |
| **Service Worker** | 不能维持长连接；只能在被唤醒的短窗口内工作，**不能当"常驻守护进程"** |
| **无开机自启、无系统托盘** | Web 端当然没有 |

---

## 10. 安全上下文与部署硬要求

| 项 | 结论 |
|---|---|
| HTTPS | ✅ **强制**。所有关键 API（`getDisplayMedia`、`getUserMedia`、File System API、Wake Lock、WebTransport、Service Worker、Push）都要求 secure context |
| 例外 | `http://localhost`、`127.0.0.0/8`、`::1/128`、`file://` 被视为 potentially trustworthy ✅ [MDN Secure Contexts](https://developer.mozilla.org/en-US/docs/Web/Security/Secure_Contexts) |
| **对自建部署的影响** | 🔴 **局域网自建场景下无法用自签证书**——用户会看到证书错误，且 secure context 判定失败（除非用户手动导入 CA 到系统信任库并接受风险）。这是 **D8"局域网直连可用"与 Web 端之间的直接冲突**：<br>• 公网域名 + Let's Encrypt 证书：✅ 可行<br>• 纯内网 IP + 自签证书：🔴 Web 端基本不可用，必须退回原生客户端<br>• 内网域名 + 内网 CA：🟡 需用户在每个浏览器/设备上安装根证书（企业部署可接受，个人用户不可接受） |
| WebTransport | `https://host:port/path`，端口必须显式；服务端**必须校验 `Origin`** |

> **建议**：把 Web 端定位为 **"通过公网域名访问的免安装客户端"**，而不是"局域网自建的一部分"。局域网无人值守访问仍由原生客户端承担。

---

## 11. 浏览器版本要求（⚠️ 版本号需在目标浏览器实测确认）

| API | Chrome / Edge | Firefox | Safari (macOS) | iOS Safari |
|---|---|---|---|---|
| WebRTC（`getUserMedia` / `RTCPeerConnection`） | 56+ | 44+ | 11+ | 11+ |
| `getDisplayMedia`（屏幕采集） | 72+ | 66+ | 13+ | 🔶 支持不完整 |
| **安全上下文（HTTPS）** | 强制 | 强制 | 强制 | 强制 |
| WebCodecs（`VideoDecoder`/`VideoEncoder`） | 94+ | 130+ | 16.4+ | 16.4+ |
| Screen Wake Lock | 84+ | 126+ | 16.4+ | 16.4+ |
| File System Access（`showSaveFilePicker`） | 86+ | ❌ | ❌ | ❌ |
| OPFS | 86+ | 111+ | 15.2+ | 15.2+ |
| WebTransport | 97+ | 114+ | 26.2+ ⚠️ | 26.2+ ⚠️ |
| Push API（Web Push） | ✅ | ✅ | 16+ | **16.4+ 且需添加到主屏幕** |
| Fullscreen API（任意元素） | ✅ | ✅ | ✅ | ❌ **仅 `<video>`** |

> 以上除标注来源者外，均为 MDN 章节的 Baseline 标注与规格信息推断；**动工前请用 caniuse / 真机再核一遍**（WebCodecs、WebTransport、`getDisplayMedia` 的移动端支持尤其需要实测）。

---

## 12. 对现有决策的修订建议（供 `docs/需求分析.md` 合并）

| # | 原决策 | 因 Web 端而需修订的内容 |
|---|---|---|
| **D12** | "是否需要浏览器端观看" → 待定 | ✅ **已决策：需要**。且答案比"观看"更宽——Web 可做观看/摄像头源/文件/语音。**由此 WebRTC 成为硬约束，`iroh` 备选方案正式出局**（`transport-media.md` 曾把 iroh 列为备选） |
| **D11** | 文件走独立 QUIC 连接 | 🟡 **部分修订**：原生↔原生仍走 `quinn`；**Web 参与时走 WebTransport（经中继，非 P2P）**，并需按文件大小在 DataChannel(P2P) 与 WebTransport(中继) 之间分流 |
| **D4** | 无人值守访问 | ❌ **Web 端永久不支持**无人值守（无后台、无自启） |
| **D8** | 局域网直连可用 | 🔴 与 Web 端存在冲突：纯内网 IP + 自签证书下 Web 端不可用（见 §10） |
| **D6** | UI 框架取向 | 🟡 Web 端是**第四个独立 UI 目标**（不是 Flutter/Slint 能顺带覆盖的）。需单独评估前端技术栈与其复用 Rust 核心的路径（**WASM**：加密、协议、分块逻辑可编译为 WASM 复用；采集与编解码不可） |
| **新增 D18** | — | **Web 端文件/消息经中继的带宽成本是否接受**（直接影响"中继占比 <15%"KPI） |
| **新增 D19** | — | **Web 端是否要支持"作为屏幕源"**（`getDisplayMedia` 体验差、移动端支持差）。建议：只支持"共享标签页"作为演示功能，不承诺 |
| **FR-10** | 剪贴板互通 | ❌ 建议 **Web 端不做**（无法后台监听剪贴板） |
| **FR-8** | 传递话语 | 🟡 实时语音在 Web 端**需要一次用户手势解锁音频播放**，UI 必须设计"点击接听" |

---

## 13. Web 端"必砍清单"（一句话版）

**永久不做（API 不存在）**：
1. 被控端（注入鼠标键盘）——也是安全上应该庆幸的事。
2. 后台静默传输 / 关页续传。
3. 无用户手势的屏幕采集与文件保存。
4. 系统音频作为可靠采集源（浏览器内无 loopback 等价物）。
5. 任意本地路径写入 / 目录结构复刻。
6. 剪贴板后台监听互通。

**降级做（能用但需明说）**：
7. 屏幕共享：仅前台、每次重选源、移动端支持差 → 定位为"演示功能"。
8. 通知：iOS 需添加到主屏幕，且 Safari 不允许隐形推送。
9. 长时观看：需 Wake Lock，且切后台后要重新获取。

**做得很好（应作为 Web 端主卖点）**：
10. 观看端（含 iOS 16.4+ 上的 iPhone/iPad）、摄像头源、文件传输、实时语音、文字消息。

---

## 14. 参考来源

- [MDN: MediaDevices.getDisplayMedia()](https://developer.mozilla.org/en-US/docs/Web/API/MediaDevices/getDisplayMedia)（transient activation、`systemAudio` 提示可被忽略、Limited availability）
- [MDN: Screen Capture API](https://developer.mozilla.org/en-US/docs/Web/API/Screen_Capture_API)（Captured Surface Control 仅缩放/滚动）
- [MDN: WebCodecs API](https://developer.mozilla.org/en-US/docs/Web/API/WebCodecs_API)（硬件加速逐帧编解码；H.265 浏览器支持缺口）
- [MDN: WebTransport API](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API)（**Baseline 2026**、HTTP/3、Origin 校验、Rust `wtransport`）
- [MDN: File System API](https://developer.mozilla.org/en-US/docs/Web/API/File_System_API) 与 [Origin private file system](https://developer.mozilla.org/en-US/docs/Web/API/File_System_API/Origin_private_file_system)（OPFS Baseline、免权限提示、断点续传用例）
- [MDN: Window.showSaveFilePicker()](https://developer.mozilla.org/en-US/docs/Web/API/Window/showSaveFilePicker)（Limited availability / Experimental / 需用户手势）
- [MDN: Screen Wake Lock API](https://developer.mozilla.org/en-US/docs/Web/API/Screen_Wake_Lock_API)（Baseline 2025；非活跃文档自动释放锁）
- [MDN: Push API](https://developer.mozilla.org/en-US/docs/Web/API/Push_API)（需 Service Worker、VAPID、endpoint 为 capability URL）
- [Apple: Sending web push notifications in web apps and browsers](https://developer.apple.com/documentation/usernotifications/sending-web-push-notifications-in-web-apps-and-browsers)（iOS 16.4+ 需 Home Screen web app；**Safari 不支持隐形推送**）
- [MDN: Secure Contexts](https://developer.mozilla.org/en-US/docs/Web/Security/Secure_Contexts)（HTTPS/localhost/file 判定规则）
- [MDN: Fullscreen API](https://developer.mozilla.org/en-US/docs/Web/API/Fullscreen_API)（非 Baseline）
- [MDN: Capabilities, constraints, and settings](https://developer.mozilla.org/en-US/docs/Web/API/Media_Capture_and_Streams_API/Constraints)（`getSupportedConstraints`/`getCapabilities`/`applyConstraints`）
- [Apple Developer Forums: [iOS Safari] Fullscreen API on a non-video element](https://developer.apple.com/forums/thread/133248)（iOS 仅 `<video>` 可全屏）⚠️
