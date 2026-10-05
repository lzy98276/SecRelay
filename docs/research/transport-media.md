# SecRelay 传输层与媒体栈调研（v0.1）

> 配套文档：`docs/需求分析.md` §6.3、§7
> 调研范围：A 传输层 / B NAT 穿透 / C E2EE 与身份 / D 编码与媒体处理 / E 文件传输
> 结论形态：**一套可落地的组合 + 必须自研的部分 + 成本模型**
> 版本与发布时间均于调研时从 crates.io API、官方文档、RFC 逐项核实（见各节引用链接）。

---

## 0. 执行摘要（先看这一节）

**推荐的一套组合**（详见 §6.2）：

| 层 | 选型 | 理由一句话 |
|---|---|---|
| 传输抽象 | **双栈**：`webrtc-rs` (0.21) 走 WebRTC 语义 + `quinn` (0.11) 负责大文件 | 媒体白拿 ICE/BWE/NACK/FEC/DTLS-SRTP；文件用 QUIC 流才能跑满带宽 |
| 打洞 | 自建 ICE（复用 webrtc-rs 的 ICE agent）+ 自建 STUN | 打洞逻辑不要自己写，但要能自控候选策略 |
| 中继兜底 | **自建 coturn 集群（中继零知识）+ 自研 relay 协议用于信令与消息** | TURN 转发密文，服务器不解密；成本可控 |
| 编解码 | **H.264 High Profile 基线（硬编优先）→ HEVC 可选 → AV1 暂缓** | 5 平台硬解覆盖 + WebRTC 互通性；AV1 在屏幕共享场景仍不稳 |
| 桌面文字 | **不用纯视频方案**：脏矩形/瓦片差分 + H.264 准无损混合 | 纯视频编码不可能同时满足"文字锐利"和"码率可接受" |
| E2EE | **DTLS-SRTP（跳跳）+ SFrame/自研帧级 AEAD（端到端）+ Noise IK（控制通道）** | SFU/TURN 可转发不可读 |
| 身份 | **Ed25519 长期设备密钥 + X25519 预共享 + QR 配对 + 6 位 SAS 校验** | 服务端零知识且防 MITM |
| 文件 | **QUIC 双流向流 + BLAKE3 块级哈希 + FastCDC 去重 + Syncthing 式块协议** | 断点续传/去重/目录同步有成熟先例 |

**最大的 3 个技术风险**（详见 §7）：
1. **WebRTC Rust 生态仍在动荡期** —— webrtc-rs 刚经历 sans-I/O 重构（0.17 特性冻结 → 0.21 重写），历史上有每连接 ~109 KiB 的线性内存泄漏；str0m 虽稳但**明确不支持 TURN**（需自建）。
2. **远程桌面"文字清晰度"与通用视频编码的目标函数冲突** —— 这是唯一一个"选对库也解决不了、必须自研"的部分。
3. **NAT 穿透失败率 × 视频中继带宽成本** —— 中继 1080p 一小时约 **1.6–3.6 GB**，按云出口价可能吃掉整个产品的毛利。

---

## 1. 术语与结论速览表

| 方案 | Rust 生态成熟度 | DataChannel 传文件 | 音视频质量 | 拥塞控制/带宽估计 | NAT 穿透 | 浏览器互通 | 维护活跃度 | 结论 |
|---|---|---|---|---|---|---|---|---|
| **webrtc-rs** (`webrtc` 0.21.0) | 中 | ✅ SCTP，但大流量有坑 | ✅ 完整 | ✅ TWCC + GCC | ✅ ICE/STUN/TURN 全 | ✅ 最好 | 活跃（2026 重构中） | **媒体首选** |
| **str0m** (0.24.1) | 中高（代码质量最高） | ✅ SCTP（较新） | ✅ 完整 | ✅ TWCC | ⚠️ **TURN 需自建** | ✅ | 活跃（0.24，2026-10） | **可作为 SFU/服务端内核** |
| **quinn** (0.11.12) | 高 | N/A（用 stream） | ❌ 需自建 | ✅ BBR/CUBIC | ❌ 全自建 | ⚠️ WebTransport | 非常活跃 | **文件/控制通道首选** |
| **iroh** (1.3.0) | 高 | N/A（用 stream） | ⚠️ 可，但非为低延迟媒体设计 | ✅ QUIC | ✅ **自带打洞 + 中继** | ❌ 非 WebRTC | 非常活跃（n0） | **备选/快速起步** |
| **libp2p** | 高 | ❌ | ❌ | ✅ QUIC | ✅ 但重 | ❌ | 活跃 | 不推荐 |
| **Tailscale/WireGuard** | — | N/A | ✅（隧道透明） | ❌ 无媒体感知 | ✅ 极强（>90%） | N/A | 活跃 | 不适用（L3 VPN，无法做逐会话权限） |
| **纯自建 UDP** | — | ❌ | ❌ | ❌ 全自建 | ❌ 全自建 | ❌ | — | 不推荐 |
| **LiveKit** (Rust SFU) | 高 | ✅ | ✅ | ✅ | ✅ | ✅ | 活跃 | **多人才上，一对一不上** |
| **mediasoup** | 高（C++，Rust 无 SDK） | ✅ | ✅ | ✅ | ✅ | ✅ | 活跃 | 同上，但需 C++ 运维 |

---

## 2. A —— 传输层选型对比

### 2.1 WebRTC：`webrtc-rs` vs `str0m`

#### 2.1.1 现状核实（重要）

调研中发现了**一个关键的版本陷阱**，必须先讲清楚：

- `webrtc.rs` 官方博客在 **2026-01-31** 宣布 `webrtc` **v0.17.0 是 Tokio 耦合分支的最后一个特性版本**，之后进入"仅修 bug"状态，主干转向基于 `rtc` crate 的 **sans-I/O 重写**。官方明确列出了触发重写的架构性问题：
  - **每个 PeerConnection 泄漏约 109 KiB 内存**，实测回归 `leak = 111 KiB × connections + 172 KiB`（1 连接 283 KiB / 4 连接 620 KiB / 21 连接 2.5 MiB / 40 连接 4.6 MiB）；
  - 回调 `Box<dyn Fn>` 所有权不清、无法手动清理；
  - 深层 Tokio 耦合（隐藏 `tokio::spawn`、`tokio::time::sleep`）；
  - 回调持锁导致潜在死锁。
  来源：[webrtc v0.17.0: Feature Freeze and Shifting to Sans-I/O](https://webrtc.rs/blog/2026/01/31/webrtc-v0.17.0-feature-freeze-sansio-shift.html)
- 但 **当前 crates.io 上 `webrtc` 最新版是 0.21.0（2026-09 发布）**，已整合到 sans-I/O 分支。发布说明里可见大量修复：SCTP 流 ID 分配时机、SCTP 零窗口死锁、Android 后台 ~10s 后 ICE restart 卡死、以及 **"慢消费者时入站 DataChannel 消息被静默丢弃"**（#858/#861）。来源：[webrtc-rs/webrtc v0.21.0 release](https://newreleases.io/project/github/webrtc-rs/webrtc/release/v0.21.0)
- `rtc` crate（sans-I/O 核心）在 2026-01-04 发布 0.3.0，宣称 ICE / DTLS / SRTP / SCTP / DataChannel / RTP-RTCP / SDP / PeerConnection 已完整，**Simulcast 与 RTCP feedback interceptor 仍在进行中**。来源：[Announcing rtc 0.3.0](https://webrtc.rs/blog/2026/01/04/announcing-rtc-v0.3.0)

> **对 SecRelay 的含义**：不要 pin 在 0.17.x。用 0.21.x，并把"每连接内存占用"做成 CI 里的回归测试项（建立/销毁 1000 条连接看 RSS）。如果团队能接受 sans-I/O 的复杂度，直接用 `rtc` 更省心（无隐藏线程、可确定性测试）。

- `str0m` 最新 **0.24.1（2026-10-03）**，MIT/Apache-2.0，下载量 239 万。它的官方 FAQ 对照表里明确写了能力边界：

  | 能力 | str0m | libWebRTC |
  |---|---|---|
  | Peer Connection API | ❌ | ✅ |
  | SDP / ICE / DataChannel / TWCC / BWE / Simulcast / NACK | ✅ | ✅ |
  | **Adaptive Jitter Buffer** | ❌ | ✅ |
  | 采集 / 编码 / 解码 / 渲染 | ❌ | ✅ |
  | **TURN** | ❌ | ✅ |
  | **网卡枚举** | ❌ | ✅ |
  | 平台测试覆盖 | Win/Linux/macOS ✅；**iOS/Android 仅编译通过、未测试** | 全 |

  来源：[docs.rs/str0m 0.24.1](https://docs.rs/str0m/latest/str0m/)
  str0m 自述定位是"**我们把它当服务端 SFU 用**，P2P 用法测试较少"。

> **对 SecRelay 的含义**：str0m 的"无 TURN、无网卡枚举、无自适应抖动缓冲、移动端未测试"这四条，每一条都是 SecRelay 的核心需求。**str0m 不适合做客户端媒体栈**，但非常适合做**自研中继/SFU 的服务端内核**（0.24 已支持 SCTP data channel，`Channel::write` 在文档中）。

#### 2.1.2 WebRTC 的关键能力：DataChannel 传文件

WebRTC DataChannel = SCTP over DTLS。真实限制：

- **必须自己处理背压**。这正是 webrtc-rs 0.21.0 修的那个 bug 的场景（"Inbound data-channel messages are silently dropped when the consumer is slow"）——在 0.17 及更早版本里，消费者慢时消息**被静默丢弃且不报错**。传文件时这会表现为"哈希对不上但没有任何错误"。
- **有序 + 可靠**模式下，SCTP 的 head-of-line blocking 会与小流量消息（控制指令）互相阻塞，除非**分开建多条 DataChannel**（WebRTC 的 DataChannel 是独立 SCTP stream，天然多路复用）。
- WebRTC 的拥塞控制是**面向实时媒体调优的**（GCC/TWCC，目标低延迟），在批量传输场景下**打不满链路**。这是事实上的工程共识：浏览器里 DataChannel 传大文件通常只有链路带宽的 30–60%。
- 部分可靠/无序模式（`maxRetransmits: 0`）适合媒体，不适合文件。

> **结论**：DataChannel 适合**控制流、消息、小文件、剪贴板**（<几十 MB）。**大文件传输应走独立的 QUIC 连接**（见 §5）。

#### 2.1.3 WebRTC 一次性买到的东西（这是它最大的价值）

| 能力 | 规范/实现 | 自研成本 |
|---|---|---|
| NAT 穿透 | ICE (RFC 8445) + STUN (RFC 8489) + TURN (RFC 8656) | 3–6 人月 |
| 媒体加密 | DTLS-SRTP (RFC 5764 + RFC 3711) | 1–2 人月 |
| 拥塞控制 + 带宽估计 | GCC + Transport-CC | 3–6 人月（且很难调对） |
| 丢包恢复 | NACK/RTX + (ULP)FEC + FlexFEC | 2–3 人月 |
| 抖动缓冲 + A/V 同步 | RTP/RTCP SR/RR + 自适应抖动缓冲 | 2–4 人月 |
| 媒体协商 | SDP (RFC 8866) + 编解码 payload 格式 | 2 人月 |

**总计约 15–25 人月**。这就是"一对一 P2P 首选 WebRTC 语义"的量化依据。

---

### 2.2 QUIC（`quinn`）+ 自研媒体层

`quinn` **0.11.12（2026-09）**，MIT/Apache-2.0，累计下载 **3.36 亿**，是 Rust 生态最成熟的 QUIC 实现（`quinn-proto` 0.11.19 为纯协议层，同样 sans-I/O）。

**QUIC 给了你什么**：TLS 1.3 集成、多路复用流、流级/连接级流控、丢包检测与重传、CUBIC/BBR 拥塞控制、连接迁移（换网不断连）、0-RTT、路径 MTU 发现。

**洞穿 NAT 需要自己做**：QUIC 本身**没有** ICE。要自己实现：
1. **地址发现**：向 STUN-like 服务端查询 reflexive 地址（可以复用 `stun_codec` 0.4.0 或自解析）。
2. **候选交换**：通过信令通道交换 host/srflx/relay 候选。
3. **打洞**：两端同时向对方候选发 QUIC Initial，需要处理"QUIC 连接与 socket 四元组绑定"的问题——QUIC 的 connection ID 机制帮了忙（可以在打洞的多个 socket 上试同一 CID）。
4. **回退中继**：需要一个 QUIC-aware 的中继（不能简单用 TURN，因为 TURN 是 UDP relay，理论上可以转发 QUIC 包，但 QUIC 的 anti-amplification 与 CID 路由会让它复杂）。

**还需自己实现的三件事**（如果要用 QUIC 跑媒体）：

| 要做的事 | 具体内容 | 参考实现 |
|---|---|---|
| **拥塞控制** | QUIC 默认 CUBIC 面向吞吐而非低延迟；要换 BBR 或自研延迟梯度控制，并按"媒体帧"而非"字节"排队 | quinn 可换 congestion controller；WebRTC 的 GCC 实现可参考 |
| **FEC** | 需要自己设计 XOR/Reed-Solomon 前向纠错包格式，配合 NACK | RFC 5109 (ULP FEC)、RFC 8627 (FlexFEC) |
| **抖动缓冲** | 需要自己实现自适应抖动缓冲（目标 <50 ms 深度、按到达间隔动态调整） | str0m 明确没有这个；可参考 WebRTC NetEq / libwebrtc `video_jitter_buffer` |
| **媒体包格式** | RTP 或自定义；用 QUIC DATAGRAM 帧（RFC 9221）而非 stream，才能避免 HOL blocking | quinn 支持 `send_datagram` |

> **结论**：用 QUIC 跑媒体是**研究项目**，不是产品起步方案。但 **QUIC 跑文件/控制是正解**。

---

### 2.3 iroh（QUIC + 打洞 + 中继）

**iroh 1.3.0（2026-09）**，MIT/Apache-2.0，下载 300 万，由 n0-computer 开发，"已在数十万台设备上生产运行"。

**它替你解决了最难的洞穿问题**：
- 基于**公钥寻址**（Endpoint ID，不是 IP），天然适配"设备身份"模型 —— 这一点与 SecRelay 的身份设计高度契合；
- 自带打洞（QUIC + 多路径尝试）与 **relay 兜底**；
- 生态里有现成的 `iroh-blobs`（0.103.0，内容寻址的 blob 传输，自带 BLAKE3 校验与断点续传）和 `iroh-gossip`（0.101.0，pub/sub）。
- **1.0.2 修了一个 relay 的 pre-auth DoS**（malformed frame 导致 out-of-bounds panic，因推荐 `panic = "abort"` 而可整机崩溃）。这说明 relay 代码已进入被安全研究者审视的阶段——好事，但也说明**自建 relay 必须跟版本**。来源：[iroh 1.0.2 – iroh-relay Security Fix](https://www.iroh.computer/blog/iroh-1-0-2)

**能否承载低延迟媒体？**
- QUIC DATAGRAM 可以，但**没有 WebRTC 那套媒体优化**（无 TWCC 式带宽估计反馈回路、无 NACK/RTX、无抖动缓冲、无 A/V 同步）。
- iroh 的设计目标是"可靠的任意设备间连接"，不是"实时媒体"。用它跑 1080p60 桌面流，你会重新实现 §2.2 里那三件事。
- 与浏览器互通**不可能**（iroh 是自有协议，不是 WebRTC）。

**中继成本（官方 Managed Relay 定价，2026）**：
- **$199/月/区域**（Pro 计划附加项）
- 含 **250 GB/月出口流量**，超出 **$0.09/GB**
- 单区域支持 **60,000 并发连接**
- 只有"经 relay 转发"的流量计费，P2P 直连不计费
来源：[iroh Dedicated Hosting](https://docs.iroh.computer/iroh-services/relays/managed)

> **含义**：$0.09/GB 在 §2.5 的成本模型里属于**偏贵**一档（自建 coturn 的云出口价大致 $0.02–$0.09/GB）。如果 SecRelay 预期中继占比 >20%，自建比用 iroh 托管中继便宜。

**iroh 的适用场景**：**如果 SecRelay 放弃"浏览器端观看"这个未来可能性**，iroh 是让"设备配对 + 打洞 + 中继 + 文件传输"一次性落地的极佳选择（尤其 `iroh-blobs` 直接给你 §5 的一半工作）。但**媒体栈仍要自建**。

---

### 2.4 libp2p / Tailscale WireGuard / 纯自建 UDP

| 方案 | 为什么不选 |
|---|---|
| **libp2p** | 面向"去中心化 p2p 网络"设计，抽象层次深（transport upgrader、multistream-select、swarm、behaviour）。它的 QUIC transport 不提供媒体所需的带宽估计/丢包恢复。rust-libp2p 社区自己就在讨论 [从 webrtc-rs 切换到 str0m](https://github.com/libp2p/rust-libp2p/issues/3659)——说明连它都不满意现有 WebRTC 选项。对 SecRelay 是"过度抽象 + 不解决核心问题"。 |
| **Tailscale / WireGuard** | 它解决的是 **L3 组网**（"我的设备像在同一局域网"），NAT 穿透极强（Tailscale 官方称直连成功率"well north of 90%"）。但 SecRelay 需要的是**逐会话、逐频道的授权与中继**（"这台电脑这次只允许看摄像头，不允许传文件"），L3 VPN 做不到；也没有媒体感知的拥塞控制（多条流会互相饿死）。**可作为参考实现学习其 magicsock 的打洞策略**，不作为架构。 |
| **纯自建 UDP** | §2.2 的清单之外还要加：加密握手、重传、分片重组、MTU 发现、NAT 穿透……这是从零写一个 QUIC。除非团队有 12+ 人月预算并有明确的"必须自控到字节"的需求，否则不理性。 |

---

### 2.5 商用/开源自托管 SFU（LiveKit / mediasoup）作为"多人与中继"备选

| 维度 | LiveKit | mediasoup |
|---|---|---|
| 语言 | **Go**（服务端）+ Rust SDK（`livekit` 0.9.3, 2026-09） | **C++** 核心 + Node.js/Rust(?) worker 层 |
| 自托管 | ✅ 官方文档完整（VM/K8s/多区域/端口/基准/监控） | ✅ 但需自己搭 worker 编排 |
| E2EE | ✅ **内置**，基于 insertable streams + SFrame，**SFU 不解密媒体** | ⚠️ 需自行在应用层叠加 SFrame |
| 中继兜底 | ✅ 内置 TURN | ✅ 内置 |
| 运维负担 | 中（单二进制 + Redis） | 高（C++ 编译 + worker 池） |
| 对 SecRelay 的适配 | **一对一场景是杀鸡用牛刀**：SFU 会强制所有媒体经服务器，破坏 P2P 直连的低延迟与零成本优势 | 同左，且更重 |

**LiveKit 的 E2EE 机制值得抄**（RFC 9605 SFrame）：媒体做**双层加密**——
1. **跳跳（HBH）加密**：SRTP，端点↔SFU；
2. **端到端（E2E）加密**：SFrame，端点↔端点。SFU 只看到 SFrame 头部（KID/CTR，**故意不加密**以便转发决策），看不到净荷。
来源：[RFC 9605 Secure Frame (SFrame)](https://datatracker.ietf.org/doc/rfc9605/)、[LiveKit Encryption Overview](https://docs.livekit.io/transport/encryption/)

> **决策建议**：**一对一不上 SFU**。SFU 只在以下条件同时成立时引入：(a) 需要一对一之外的会话语义（多人看一台 / 一人看多台且需服务端转码/录制），且 (b) 愿意接受媒体绕行服务器的延迟与带宽成本。
> **但要把 SFrame 的设计抄过来**（见 §4.3），这样将来接 SFU 时无需改协议。

---

### 2.6 ★ 一对一 P2P 首选方案 + 对称 NAT 兜底方案

#### 首选：ICE 直连 + WebRTC 媒体 + 独立 QUIC 文件通道

```
┌──────────────────────── SecRelay 传输栈 ────────────────────────┐
│                                                                  │
│  信令/控制通道 (QUIC stream, 经自建 relay)                        │
│    · 设备发现、能力协商、ICE 候选交换、SAS 校验、消息、桌面提示      │
│                                                                  │
│  ┌──────────────── 媒体通道 ────────────────┐                    │
│  │ ICE 打洞 (webrtc-rs 0.21 的 ICE agent)   │                    │
│  │   ↓ 成功 → 直连                           │                    │
│  │   ↓ 失败 → TURN (自建 coturn 集群)        │                    │
│  │ SRTP (DTLS-SRTP) + SFrame E2EE            │                    │
│  │ RTP: 屏幕/摄像头/麦克风                    │                    │
│  │ DataChannel: 控制指令、小消息              │                    │
│  └──────────────────────────────────────────┘                    │
│                                                                  │
│  ┌──────────────── 文件通道 ────────────────┐                    │
│  │ QUIC 连接 (quinn) —— 独立于媒体           │                    │
│  │   同一 ICE 打洞结果或 TURN/UDP relay       │                    │
│  │   块级 BLAKE3 + FastCDC + 断点续传         │                    │
│  └──────────────────────────────────────────┘                    │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
```

**为什么媒体与文件分两条连接**：
- 媒体要**低延迟 + GCC 拥塞控制**；文件要**高吞吐 + 公平分享**。放一条连接上，GCC 会让文件传输"礼貌"到打不满带宽，而文件流会把媒体挤到丢包。
- 两条连接各自有独立的拥塞控制状态，可以显式做**优先级**（媒体优先，文件在媒体空闲时提速）——比在一条连接里做流优先级简单得多。
- 切换/重连解耦：WiFi→4G 切换时媒体要快速恢复，文件可以慢一点。

#### 对称 NAT / CGNAT 兜底：分层回退，每层有明确预算

| 层级 | 手段 | 预期命中率（累计） | 延迟代价 |
|---|---|---|---|
| L0 | UPnP / NAT-PMP 显式端口映射（仅家用路由） | +3–8% | 0（映射成功后即直连） |
| L1 | STUN + ICE 常规打洞（host + srflx 候选） | **70–85%**（消费级互联网） | 0 |
| L2 | **端口预测 + 生日悖论扫描**（对称 NAT 对端） | +5–15% | +1–2s |
| L3 | **IPv6 直连**（若双端有原生 IPv6，完全绕开 NAT） | 随 IPv6 普及率增长 | 0 |
| L4 | **TURN 中继（自建 coturn，零知识转发）** | 剩余 ~10–25% | +RTT（通常 +20–80 ms） |

**IPv6 是最高杠杆的优化**：Google 统计显示 2026-03-28 全球 IPv6 访问占比**首次超过 50%（50.10%）**，此前 2025-06-21 曾达 49.56%；但各国差异极大（法国 73%、印度 72%、沙特 65%；意大利 17%、西班牙 10%）。Internet Society Pulse 跨源平均约 **43%**。来源：[18 Years Later, IPv6 Reaches Majority — ISOC Pulse](https://pulse.internetsociety.org/en/blog/2026/04/18-years-later-ipv6-reaches-majority/)

> **行动项**：SecRelay 必须**同时**收集 AAAA 候选并优先尝试 IPv6 直连。这是唯一一个"零成本、随生态增长自动改善"的打洞手段。IPv6 下通常没有 NAT，只有防火墙（有状态防火墙仍可被出向包打洞）。

---

## 3. B —— NAT 穿透现实与中继成本

### 3.1 打洞成功率：把数字说清楚

| 来源 | 数字 | 语境 |
|---|---|---|
| Tailscale 官方（2025-10） | 直连成功率 **"well north of 90%"**，内部指标 | 自有网络，含 DERP 协调 + 多年打磨的 magicsock + 赞助 FreeBSD 修 NAT 行为 |
| Fora Soft TURN 计费分析（2026-05 复核） | 消费级 WebRTC 会话 **15–25%** 回退 TURN；企业 WiFi 与受限移动运营商更高 | 通用 WebRTC 产品 |
| 同上 | **IoT 设备（CGNAT 后）与 AI 语音代理的回退率接近 100%** | 特殊场景 |

来源：[How Tailscale is improving NAT traversal (pt 1)](https://tailscale.com/blog/nat-traversal-improvements-pt-1)、[TURN Bandwidth Calculator](https://www.forasoft.com/learn/video-streaming/articles-streaming/turn-bandwidth-calculator)

**为什么 Tailscale 能到 >90% 而普通 WebRTC 只有 75–85%**：
1. **每次都先用 DERP 建连，再并行升级到直连**（"technically begins via DERP, then upgrades"）——不存在"打洞超时阻塞用户"的体验问题；
2. 客户端支持 **NAT-PMP/PCP/UPnP** 并监控映射 epoch（路由重启后自动重建映射）；
3. 赞助 **FreeBSD PF 的 Endpoint-Independent Mapping (EIM) 补丁** —— pfSense/OPNsense 路由器历史默认是 symmetric NAT，打补丁后可配成 cone NAT；
4. **同时探测并赛跑多条路径**（IPv4/IPv6、多网卡），选延迟最低的。

> **给 SecRelay 的启示**：Tailscale 的"**总是先走中继建立，再并行升级直连**"是极佳模式——它把"打洞"从**阻塞路径**变成**后台优化**，用户永远不会因为打洞而等待。建议直接采纳（详见 §6.3 的状态机）。

### 3.2 对称 NAT 与 CGNAT 的实际比例

**对称 NAT（"hard NAT"）**：为每个目标地址分配不同的源端口，导致对端无法预测端口。Tailscale 的定性描述：
> "Two devices, each behind 'hard NAT,' will almost always need to use a relay."
> "Enterprise-grade firewalls and carrier-grade NAT gateways (often behave this way for maximum connection isolation)."

**CGNAT**：数千用户共享一个公网 IP 池，"typically employ short port timeouts and symmetric mapping"。移动网络与大型 ISP 普遍使用。**两个移动设备（不同运营商）之间 P2P 直连的期望值很低**——这是 SecRelay "手机↔手机"场景必须正视的现实。

**多重 NAT（double NAT）** 会显著降低成功率：每多一层 NAT，协调复杂度与失败模式都增加（如"酒店 WiFi → VM → 云 VPC NAT"）。

**其他杀手**：
- 企业防火墙**直接封 UDP**（Tailscale 的 UDP 探测包根本发不出去）；
- **UniFi 安全网关默认把 P2P 流量判定为威胁并阻断**（需手动关规则）；
- IPS/DPI 把 P2P 打洞包识别为可疑流量。

> **因此在上述场景中，TURN over TCP/443 是唯一出路**。这正是 Tailscale 的 DERP 走 HTTPS(443) 的设计原因——"it will succeed even when direct UDP is filtered, at the cost of higher latency"。**SecRelay 的中继必须支持 TCP/443 回退**，否则在严肃企业网络上会完全不可用。

### 3.3 IPv6 的作用（量化）

- 作用：**双端原生 IPv6 时，无需 NAT 穿透，直连率接近 100%**（只受有状态防火墙影响）。
- 现实约束：IPv6 部署是**部分性**的（一端有、一端没有；只有 ULA；NAT64/DNS64 环境）。Tailscale 明确表示："many IPv6 deployments are partial... **IPv4/UDP hole punching remains the critical path for most connections, even in 2025**"。
- 中国：多家中文技术媒体在 2026-04 报道"全球 IPv6 流量占比首破 50%，中国发展成效显著"，但**中国移动/电信的 IPv6 普及率在不同来源间差异较大，且移动网络的 IPv6 防火墙策略普遍比 IPv4 更严格**——不要把 IPv6 当作中国市场的主要打洞手段，当作"有则更好"。

### 3.4 ★ 自建 TURN 的带宽成本估算

**基础换算**：`1 Mbps × 1 小时 × 单方向 = 0.45 GB`（来源：[Fora Soft TURN 计费分析](https://www.forasoft.com/learn/video-streaming/articles-streaming/turn-bandwidth-calculator)）

**关键事实：TURN 转发一次媒体，第 1 秒就过服务器两次**（入 + 出）。多数云厂商只对**出向**计费，因此保守模型按 **2 个方向**算。

#### 单条流每小时流量（GB）

| 码率 | 单方向 GB/h | 双方向计费 GB/h（中继实际成本） |
|---|---|---|
| 720p @ 2.5 Mbps | 1.13 | **2.25** |
| 1080p @ 4 Mbps | 1.80 | **3.60** |
| 1080p @ 3 Mbps | 1.35 | **2.70** |
| 1080p 高清晰度（文字场景）@ 8 Mbps | 3.60 | **7.20** |
| 1080p60 @ 12 Mbps | 5.40 | **10.80** |

> **回答需求里的问题**：**1080p 视频中继一小时约 1.8 GB（单向）/ 3.6 GB（双向计费）**。若按 4 Mbps 这个中等档位。

#### 单条中继会话的成本（按 4 Mbps，双向计费，3.6 GB/h）

| 云/服务 | 单价 | 每小时 | 每天（8h） | 每月（8h×22 天 = 176h） |
|---|---|---|---|---|
| Twilio NTS（US/EU） | $0.40/GB | $1.44 | $11.52 | **$253** |
| Twilio NTS（APAC） | $0.60–0.80/GB | $2.16–2.88 | $17.3–23.0 | **$380–507** |
| Cloudflare Realtime TURN | $0.05/GB（前 1000 GB 免费） | $0.18 | $1.44 | **$31.7** |
| **自建 coturn**（云出口 $0.02–0.09/GB + $150/月机器） | $0.05/GB 例 | $0.18 | $1.44 | **$31.7 + $150 ≈ $182** |
| iroh Managed Relay | $199/月/区域含 250 GB，超出 $0.09/GB | — | — | $199 + 超出部分 |

**规模化示例**（1,000 路峰值并发，1080p @ 2.5 Mbps，20% 走中继，每月 200 中继小时，双向计费）：
```
中继流数       = 1000 × 20%              = 200 路
每流每小时 GB  = 2.5 × 0.45 × 2          = 2.25 GB
每月中继 GB    = 200 × 200h × 2.25 GB    = 90,000 GB
Twilio US      = 90,000 × $0.40          ≈ $36,000/月
Cloudflare     = 89,000 × $0.05          ≈ $4,450/月
自建 coturn    = $150 + 90,000×$0.05     ≈ $4,650/月
```
来源与数字：[TURN Bandwidth Calculator](https://www.forasoft.com/learn/video-streaming/articles-streaming/turn-bandwidth-calculator)（该文明确标注按 RFC 8656 / RFC 8445 / Twilio 与 Cloudflare 2026 年 Q2 定价复核）

#### ★ 成本杠杆（重要）

| 杠杆 | 效果 |
|---|---|
| **从 25% 中继占比降到 15%** | 中继 GB 与账单**直接降 40%**，同时**降低延迟**（中继路径一定比直连长） |
| **从 $0.40/GB 换到 $0.05/GB** | **降 88%**，与流量无关，纯粹是采购杠杆 |
| **换更大的服务器** | **非杠杆**。"A bigger TURN box relays the same gigabytes at the same price." |

> **SecRelay 的运营策略**：(1) 把"IPv6 优先 + 并行直连升级 + NAT-PMP 映射"做到位，把中继占比压到 15% 以下；(2) 中继自建在**大带宽便宜出口**的云（如 Hetzner/OCI 的流量政策）或自建机房，而不是 AWS/Aliyun 的标准出口价；(3) coturn 只做转发，**不存储、不解密**，单机可承载数千会话（服务器成本几乎固定且可忽略）。

#### coturn 部署资源参考

- **单台中配 VPS 可中继数千并发会话**；瓶颈是网卡与带宽，不是 CPU。
- 需要开放 **UDP 中继端口范围**（默认 49152–65535，可缩窄，例如每实例 10000 个端口 → 最多 ~5000 并发会话，每会话 2 个端口）。
- 需要 `TLS/DTLS` 证书以支持 `turns:`（TURN over TLS 443）。
- 必须配置 `--no-cli`、`--no-multicast-peers`、`--denied-peer-ip` 以防 SSRF（coturn 历史上出过被用来探测内网的漏洞）。
- 建议**多区域部署 + DNS 加权**，让客户端连最近的 relay。

### 3.5 ★ 中继服务器能否不解密？

**能，而且这正是 SecRelay 名称（Relay）的应有之义。** 分三种中继语义：

| 中继类型 | 是否解密 | 原理 | 对 SecRelay 的可用性 |
|---|---|---|---|
| **TURN relay**（RFC 8656） | ❌ **不解密** | TURN 是 UDP 层的**数据报转发**。它看到的是 STUN/TURN 封装的外层，内层是已经 DTLS-SRTP 加密的 RTP 包（或 QUIC 包）。TURN 服务器**没有 SRTP 主密钥**，因为它不在 DTLS 握手里。 | ✅ **主用**。天然零知识 |
| **自研 relay（应用层转发）** | ❌ 可做到不解密 | 设计成"只转发端到端加密后的帧"，服务端只有路由表与不透明标签 | ✅ 用于信令、消息、桌面提示 |
| **SFU**（LiveKit/mediasoup） | ⚠️ **默认会解密** | SFU 需要读 RTP 头做转发决策、做 simlucast 层选择、做转码/录制 | ⚠️ 只有叠加 **SFrame (RFC 9605)** 才能做到"转发媒体但不读内容" |

**SFU + E2EE 的正确做法（RFC 9605 SFrame）**：
- SFU 仍做**跳跳 SRTP**（端点↔SFU 一跳一密）；
- 端点在**净荷**上再叠一层 SFrame E2EE（端点↔端点）；
- SFU 能看到的只有 SFrame 头部：**KID**（哪把密钥/哪个发送者）和 **CTR**（计数器）。规范**明确说明头部不加密**（RFC 9605 §7.1 "No Header Confidentiality"）——这正是为了"让 SFU 能转发"而做的**有意取舍**。
- SFrame 的已知代价（必须知道）：
  - **没有逐发送者认证**（§7.2）：用对称密钥，"任何会话内的成员都可以冒充其他成员发送媒体"；
  - **短 tag 有伪造风险**（§7.5）：32-bit tag 在 1 Gbps 攻击下平均约每 2^12 秒（约 1 小时）成功一次。**结论：用满长 tag（16 字节），不要用短 tag**；
  - 按帧加密时**无法做部分解码**（§6.3），而按包加密会增加开销；
  - 开销估算（附录 B）：假设 KID 2B + CTR 3B + tag 16B + config 1B = **22 字节/次加密**。

**Rust 实现**：`sframe` crate **2.0.0（2026-09-13）**，MIT/Apache-2.0，"pure rust implementation of SFrame (RFC 9605)"，默认用 `ring`，也可选 `rust-crypto` feature。代码量 5,476 行，较新，**成熟度中等（下载量 2.7 万，早期项目）**——需要做 test vector 验证（仓库自带 RFC test vectors）。

---

## 4. C —— 端到端加密与身份

### 4.1 DTLS-SRTP：它保护什么、不保护什么

**机制**：WebRTC 媒体走 DTLS-SRTP (RFC 5764)。DTLS 握手导出 SRTP 密钥材料（RFC 3711），媒体用 SRTP 加密。身份绑定靠 **SDP 里的 `a=fingerprint`**（证书指纹）——两端通过信令交换指纹。

**关键结论（这是 SecRelay 安全模型的核心）**：
> DTLS-SRTP **只保证"你连上的是 SDP 里声明指纹的那一方"**。它**不保证那个指纹属于你想连的人**。
> **如果信令服务器被攻破，它可以做完整的中间人攻击**：给 A 发自己的指纹、给 B 发自己的指纹，然后解密再加密转发。DTLS-SRTP 对此**毫无抵抗**。

**分场景**：
| 拓扑 | DTLS-SRTP 的安全性 |
|---|---|
| 纯 P2P 直连 | 端到端，但**指纹认证依赖信令** → 信令被控 = MITM（见 §4.5 对策） |
| 经 TURN | **仍是端到端** —— TURN 只转发加密包，不解密。这是 TURN 相对 SFU 的巨大优势 |
| 经 SFU | **降级为跳跳**。SFU 解密媒体 → 必须额外做 SFrame 才能恢复端到端 |

### 4.2 Noise 协议族

**Noise 是 SecRelay 控制通道/信令通道的正确选择**，原因：
- 比 TLS 1.3 更**轻量、无 CA 依赖、模式可裁剪**（SecRelay 是"已知对端身份"的场景，不需要 PKI）；
- **公钥即身份**，与"设备 ID = 公钥"的模型天然一致；
- Sans-I/O 友好（`snow` 是纯状态机，适合 with §0 的架构）。

**模式选择**：

| 模式 | 相互认证 | 身份隐藏 | 前向保密 | 适用 |
|---|---|---|---|---|
| `Noise_XX` | ✅ 双向 | ✅（双方身份都加密） | ✅ | **首次配对的引导握手**（此前不知道对端密钥） |
| `Noise_IK` | ✅ 单向预知 | 发起方隐藏，响应方暴露 | ✅ | **已配对设备重连**（发起方已知响应方静态公钥）——**最快（1-RTT），推荐日常使用** |
| `Noise_KK` | ✅ 双向预知 | ✅ 双方都隐藏 | ✅ | 双方都已缓存对方静态公钥 |
| `Noise_XXpsk3` | ✅ + PSK | ✅ | ✅ | 想在配对时把"短码/QR"作为 PSK 混入，防止纯密钥交换被替换 |

**Rust 实现**：`snow` **0.10.0（2025-07）**，Apache-2.0 OR MIT，**下载量 2,791 万**——成熟可靠，是本项目最不需要担心的依赖。

**Noise vs TLS 1.3**：
- 如果 SecRelay 决定**全部走 QUIC**，那 QUIC 自带 TLS 1.3，且支持**自签证书 + 证书指纹 pinning**（等价于 Noise 的静态公钥预知）。这时 `quinn` + 自定义 `ServerCertVerifier` 就是够的。
- 如果要**脱离 QUIC 做独立控制通道**（比如信令走 WebSocket/HTTP），用 Noise_IK 更干净。
- **建议**：控制/信令通道**复用 QUIC**（quinn + rustls + 指纹 pinning），把 `snow` 留作**跨 relay 的端到端加密层**（因为 relay 可能被攻破，需要在应用层再包一层）。

### 4.3 双棘轮（Double Ratchet / Signal Protocol）

**问题：SecRelay 真的需要双棘轮吗？** 分两类流量：

| 流量类型 | 需要双棘轮吗 | 理由 |
|---|---|---|
| **实时媒体**（屏幕/摄像头/语音） | ❌ **不需要** | 媒体是**同步、在线、短会话**的。会话建立时的 X25519 握手已提供前向保密；会话结束密钥即弃。"每条消息棘轮"对 60fps 视频毫无意义（每秒 60 次 DH 运算）。 |
| **文字消息**（异步、离线可达） | ✅ **需要** | 这是双棘轮真正解决的问题：**前向保密（FS）+ 后妥协安全（PCS）**。服务器存了密文，将来密钥泄露不能解历史消息；设备被盗后攻击者无法解密未来消息。 |

**Rust 实现**：

| crate | 版本 | 活跃度 | 风险 |
|---|---|---|---|
| `vodozemac` (Matrix Olm/Megolm 实现) | **0.11.1（2026-09）** | 活跃，Apache-2.0，下载 137 万 | ✅ **推荐**。独立、审计过（NCC Group 审计过 vodozemac）、API 简洁 |
| `libsignal-protocol` | **0.1.0（2019-07！）** | **已死**（crates.io 上 7 年未更新） | ❌ 不要用这个 crate |
| `signalapp/libsignal` (GitHub) | 活跃，但**未在 crates.io 发布稳定版** | 活跃 | 需 vendor 或有 git 依赖，Rust 绑定面向 Java/Swift，Rust API 不友好 |

> **建议**：文字消息用 `vodozemac`（Olm = 双棘轮，Megolm = 群组棘轮）。**如果 MVP 阶段的"传递话语"只是同账号设备间的短消息，可以先用简化的 per-session ratchet，把 vodozemac 排到 M2**——但要在协议里预留版本字段，避免将来不兼容。

### 4.4 设备配对 UX：四种方案的安全性与选择

| 方案 | 安全前提 | 已知攻击 | 用户体验 | 适用 |
|---|---|---|---|---|
| **二维码配对** | 扫码时**显示端未被攻破** | 若攻击者控制了显示端屏幕（或替换了 QR 图片），可替换为自己的公钥 → **MITM**。因此 QR **不能单独作为信任根**，必须配合一个确认步骤 | 最好（一扫即连） | **桌面→手机 的首选** |
| **一次性短数字码（SAS）** | 需要**足够熵 + 限速** | 在线暴力猜测。6 位数字 = 20 bit ≈ 100 万组合。**必须限速**（如 3 次失败即锁定 5 分钟），否则可被爆破 | 好（念/输入 6 位） | **手机↔手机、无摄像头场景** |
| **PAKE（SPAKE2 / OPAQUE / CPace）** | 只需**低熵共享秘密**（如 6 位码）即可抵抗离线字典攻击 | 需选对标准与实现；SPAKE2 有"不安全的群"陷阱（必须用安全曲线/固定生成元） | 好（用户输入同一个短码，协议本身完成认证） | **最佳：把"输 6 位码"从"验证"升级为"认证"** |
| **SAS 数字/emoji 双向比对** | 需要**独立信道**（两台设备各自显示） | 用户可能"无条件点确认"（**这是真实的 UX 攻击面**：用户会习惯性点"相同"） | 中（要用户真的去比） | 作为 QR/PAKE 之后的**纵深防御** |

**PAKE 标准化现状（RFC）**：
- **SPAKE2**：RFC 9382（IRTF/CFRG）
- **SPAKE2+**：RFC 9383
- **OPAQUE**：RFC 9807（增强型 PAKE，同时解决"服务器存密码"问题，用于**账号密码**场景而非设备配对）
- **CPace**：CFRG 已选出，RFC 编号在推进中

**Rust 实现**：

| crate | 版本 | 说明 |
|---|---|---|
| `spake2` | 0.4.0（稳定）/ 0.5.0-pre.0 | MIT OR Apache-2.0，下载 139 万。**推荐**，但**注意 0.5 是 pre-release**，生产 pin 0.4.0 |
| `opaque-ke` | 4.0.1（稳定）/ 4.1.0-pre.2 | Apache-2.0 OR MIT，下载 72 万。用于账号密码，不是设备配对 |

> ⚠️ **SPAKE2 的实现陷阱**：SPAKE2 要求群是"安全的"（无小子群、生成元固定）。历史上多个实现出过问题。**必须用 crate 提供的 `Spake2::<Ed25519Group>::start_*` 官方 API，不要自己构造群元素**，并跑通官方 test vectors。

### 4.5 ★ "服务器零知识"下的密钥交换设计

**威胁模型**：SecRelay 的自建中继服务器**可能被攻破**（或被运营方自己恶意窥探）。要求：服务器**即使在信令阶段全权控制**，也无法解密内容、无法 MITM。

**核心原则**：**服务器可以中转，但不能是信任根。信任根是"设备首次配对时，人用带外信道（眼睛/耳朵）确认的那一次"。**

#### 身份与密钥层次

| 密钥 | 算法 | 生命周期 | 存放位置 |
|---|---|---|---|
| **设备身份密钥 (DIK)** | Ed25519 | 永久（重装即换身份） | 平台安全存储（见 §4.7） |
| **设备身份密钥 (X25519)** | X25519 | 永久，由 DIK 确定性派生或并行生成 | 同上 |
| **会话临时密钥 (ESK)** | X25519，每会话新生成 | 单次会话 | 内存 |
| **预共享密钥 (PSK)** | 32 字节随机 | 配对时创建，每设备一份 | 安全存储 |
| **SFrame 发送密钥** | HKDF-SHA256(会话密钥, "sframe-send", sender_id) | 每会话 / 每次成员变化 | 内存 |
| **文件内容密钥** | 每文件随机 32 字节 | 单文件 | 随文件元数据（被会话密钥加密后）传输 |

#### 配对流程（推荐：QR + PAKE + SAS 三层）

```
阶段 0：设备生成 DIK（Ed25519）+ DHK（X25519），公钥上传统信令服务器（服务器只知道"有个设备存在"）

阶段 1（带外）：设备 A 显示二维码，内容 =
    { protocol_ver, A_ed25519_pub, A_x25519_pub, ephemeral_nonce, relay_hint }
  设备 B 扫码（B 此刻直接获得了 A 的真实公钥，不经服务器）

阶段 2（PAKE 认证）：B 生成 6 位随机码并显示在 B 屏幕上
    A 输入该 6 位码，双方执行 SPAKE2（6 位码作为 PAKE 的 password）
    → 双方得到一个 session_key，且"知道对方知道同一个 6 位码"
    这一步的作用：即使二维码被替换（画面劫持），攻击者也不知道 A 输入的 6 位码 → 无法完成 PAKE

阶段 3（SAS 比对，纵深防御）：双方各自计算
    SAS = HKDF(session_key, "secrelay-sas-v1", 双方公钥排序拼接) → 截断为 6 位数字
    A 与 B 屏幕上显示相同数字，用户肉眼确认（这一步是防"用户习惯性确认"的唯一手段：
    因为 SAS 是从 session_key 派生的，中间人无法让两端显示出相同数字）

阶段 4（固化信任）：双方把对方的 {Ed25519_pub, X25519_pub} 存入本地信任库，
    并各自生成一个 PSK 上传给服务器（服务器只存 PSK 的哈希用于路由，不存 PSK 本体）

阶段 5（后续连接）：直接用 Noise_IK（发起方已知响应方静态公钥）+ PSK，
    服务器只做信令转发（交换临时公钥和候选），全程无法解密
```

**为什么这个设计能满足"服务器零知识"**：
- 服务器**从未见过** A 的 Ed25519 私钥、会话密钥、PSK；
- 服务器**可以**做一个"错误的信令"（把 B 的公钥换成自己的），但**阶段 2 的 PAKE 会失败**（服务器不知道 6 位码），**阶段 3 的 SAS 会不一致**（用户会发现数字不同）；
- 服务器**不能重放**旧会话（临时密钥 + nonce）；
- 服务器**不能解密**媒体（DTLS-SRTP 密钥由端点协商；SFrame 密钥由端点从 session_key 派生）。

#### 明确的 MITM 攻击面清单与对策

| # | 攻击面 | 攻击者能力要求 | 对策 | 残留风险 |
|---|---|---|---|---|
| M1 | 信令服务器替换公钥 | 控制服务器 | 阶段 3 SAS 比对（用户看到数字不同） | **用户忽略警告/习惯性点确认** → 需 UI 上强对比（红色 + 需要真的输入而非点击） |
| M2 | 二维码被替换（画面劫持 / 恶意 App 覆盖） | 控制显示端 | 阶段 2 PAKE（攻击者不知 6 位码） | 无（若 PAKE 实现正确） |
| M3 | 重放旧配对请求 | 控制服务器 | 临时 nonce + 时间窗 + 已配对设备拒绝重复配对 | 无 |
| M4 | 中间人做"双份 PAKE"（对 A 用码 X，对 B 用码 Y） | 控制服务器 + 知道两个码 | 阶段 3 SAS 派生自 session_key，两端数值不同 | 无 |
| M5 | 恶意服务器静默降级（去掉 E2EE 层） | 控制服务器 | **协议版本 + 能力协商必须被 E2E 覆盖**（在 Noise 握手内确认"E2EE 已启用"），而不是在明文信令里协商 | 需仔细设计：不能在明文信令里说"我们支持 E2EE 吗" |
| M6 | 首次使用无带外信道（"首次使用信任"） | 控制服务器 | **必须禁止纯在线首次配对**。要么扫 QR，要么输 6 位码 | 若产品允许"输入设备 ID 直接连"则完全破防 |
| M7 | 无人值守设备被物理接触 | 攻击者拿到设备 | 平台安全存储（§4.7）+ 可选硬件安全模块 | 有 root 权限的攻击者可提取软件密钥（除非用 Secure Enclave/StrongBox/TPM） |
| M8 | SFrame 的"无逐发送者认证"（RFC 9605 §7.2） | 会话内成员 | 在 SFrame 之上加**每帧签名**（成本高）或**接受该风险**（一对一场景风险低） | 一对一场景可接受；多人场景需重新评估 |

#### 密钥透明性（Key Transparency）需要吗？

- 相关方案：CONIKS、WhatsApp Key Transparency、Matrix Cross-Signing、Signal 的 approach。
- **对 SecRelay 的判断：MVP 阶段不需要。** 理由：
  - Key Transparency 解决的是"**如何让用户发现服务器偷偷替换了别人的长期公钥**"这一规模化问题（WhatsApp 有 20 亿用户，用户不可能逐个比对）。
  - SecRelay 是**一对一、小规模、用户自己配对自己的设备**（<10 台）。**配对时的一次 SAS 比对就已经提供了同等强度的保证，而且是"人在回路"的。**
  - 引入 KT 需要服务器维护**可验证的追加日志（Merkle tree）**，复杂度和运营成本都不低。
- **替代方案（低成本，推荐）**：本地 **"信任的设备"列表 UI**——显示每台已配对设备的名称、配对时间、公钥指纹后 8 位，用户可随时查看/撤销。这比 KT 更贴合小规模场景。

### 4.6 文件传输的加密与完整性

- **算法选择**：
  - `ChaCha20-Poly1305`：`chacha20poly1305` **0.11.0（2026-08）** —— 无 AES-NI 的 ARM 设备上更快（移动端优先）。
  - `AES-256-GCM`：`aes-gcm` **0.11.1（2026-08）** —— 有 AES-NI/ARMv8 Crypto 扩展时更快，且有硬件卸载。
  - **建议**：两者都实现，**运行时按 CPU 能力选择**（`cpufeatures` crate 检测）。协议里带 cipher suite ID。
- **哈希**：`blake3` **1.8.7（2026-08）**，CC0/Apache-2.0，下载 1.9 亿。**BLAKE3 相比 SHA-256 的最大优势不只是速度，而是它原生支持 verified streaming（Merkle 树）**——见 §5.2。
- **⚠️ 收敛加密（convergent encryption）与去重的安全陷阱**：
  - 收敛加密 = 用 `H(明文内容)` 作为加密密钥 → 相同内容产生相同密文 → 服务器可去重。
  - **攻击**：攻击者若猜到文件内容（如"某部已知电影的某个版本"、"某个常见文档"），可算出密钥并**确认用户拥有该文件**（内容确认攻击 / confirmation-of-a-file attack）。
  - **SecRelay 的对策**：**去重只在客户端之间做，不在服务器上做**。文件用**随机 per-file 密钥**加密；去重靠"客户端本地已有块的哈希索引"（见 §5.4），而不是靠"服务器看到相同密文"。这样既得到去重收益，又不泄露内容。**这是设计上的硬约束。**

### 4.7 各平台密钥存储

| 平台 | 方案 | Rust crate | 备注 |
|---|---|---|---|
| Windows | **DPAPI**（`CryptProtectData`）或 Credential Manager | `keyring` (通过 `windows` feature) / `windows` crate 直调 DPAPI | DPAPI 绑定用户账户，重装系统/换用户即失效（需重新配对） |
| macOS | **Keychain**（`SecItemAdd` 等） | `security-framework` | 需要 codesign + notarization 才能稳定访问 |
| Linux | **Secret Service**（gnome-keyring/KWallet）via D-Bus | `keyring` (secret-service feature) / `secret-service` | **无桌面会话/headless 服务器上会不可用**——必须有加密文件兜底（如 `age` 或自研，用机器绑定密钥） |
| Android | **Keystore** / **StrongBox**（硬件） | 通过 JNI 调 `AndroidKeyStore`（Rust 无成熟直接绑定） | 需写少量 Kotlin/JNI 桥接层。StrongBox 只在部分机型 |
| iOS | **Keychain** + **Secure Enclave**（P-256 only！） | `security-framework` (iOS target) | ⚠️ **Secure Enclave 只支持 P-256，不支持 Ed25519/X25519**。若要用 SE 保护身份密钥，必须改用 P-256（或用 SE 保护的对称密钥来加密 Ed25519 私钥） |
| **通用** | `keyring` crate 抽象 | `keyring-rs` | 但**接口在各平台上语义不一致**（Linux headless 失败），必须做降级 |

> **iOS 的 P-256 限制是一个容易被忽略的架构约束**：如果 SecRelay 想把身份密钥放进 Secure Enclave，那 Noise 就得换用 P-256 的 DH（Noise 支持 `25519` 和 `448` 以及 `P256` 曲线）。**建议**：身份密钥用 Ed25519/X25519（软件 + Keychain 保护），**在 SE 里存一个包装密钥**（P-256，用 `SecKeyCreateEncryptedData`）来加密 Ed25519 私钥。这样既满足"硬件保护"，又不改 Noise 套件。

---

## 5. D —— 编码与媒体处理

### 5.1 编解码取舍

#### 5.1.1 平台与浏览器支持矩阵

| 编解码 | 硬编（5 平台） | 硬解（5 平台） | 浏览器 WebRTC | 专利/许可 | 带宽效率（相对 H.264 同质量） |
|---|---|---|---|---|---|
| **H.264/AVC** | ✅ NVENC/QSV/AMF/VideoToolbox/MediaCodec | ✅ 全覆盖（含老设备） | ✅ 全支持，最稳 | ⚠️ **Via LA（原 MPEG LA）AVC 池**。2026 起新许可 Streaming Fee 分层：Tier 1 OTT/FAST/社交/云游戏年费 **$4,500,000**，Tier 2 $3,375,000，Tier 3 $2,250,000，仅"small or nascent"保留 $100,000。**但 2025 年底前已有许可的按原条款锁定** | 1.0（基线） |
| **H.265/HEVC** | ✅ 好 | ✅ 桌面/新手机好，**老 Android/Chrome 差** | ⚠️ 支持参差（Chrome 需平台支持） | ⚠️ **多个池**（Access Advance HEVC Advance、Via LA、Nokia 单独许可）。2025-07 公布价格延至 2030 | 0.5–0.7（省 30–50%） |
| **VP9** | ⚠️ 少（Intel/部分 ARM） | ✅ 广（Android 全系、Chrome） | ✅ 支持好 | ✅ 免版税（Google） | 0.6–0.8 |
| **AV1** | ✅ 新硬件（RTX 40/50、Intel Arc、M3/M4、部分骁龙 8 Gen 3+） | ⚠️ **覆盖率不足**：大量在役 Windows PC（GTX 10/16 系、老 Intel 核显）**无 AV1 硬解**，只能软解 = CPU 爆炸 | ⚠️ **支持但仍不稳**：Chrome 支持 AV1；Firefox 支持；Safari 仅部分硬件。**屏幕共享场景有实际 bug**：LiveKit 有 [Screen sharing intermittently does not work, possibly due to AV1 (issue #3430)](https://github.com/livekit/livekit/issues/3430)；Chromium 有 [Video of shared screen in Google Meet freezes occasionally](https://issues.chromium.org/issues/459528902) | ✅ 免版税（AOMedia），但 Avanci Video 等新池在尝试对 AV1 主张内容版税 | 0.4–0.6 |

> **H.264 许可的重要澄清**：网上流传"H.264 专利已过期所以免费"是**不准确的**。Via LA 明确表示"pool covers the large majority of essential patents"，且有律师指出：**专利是按国家/地区独立的**，且"profile 规避"（只用 Baseline/Main）**不是可靠的法律策略**（"patent claims do not necessarily map neatly to profile labels"）。来源：[H.264 Streaming Fees: What Changed, Who's Affected, and What It Means — Streaming Media, 2026-03-17](https://www.streamingmedia.com/Articles/ReadArticle.aspx?ArticleID=173935)
> **但对 SecRelay 的实际情况**：分层费用只对 **OTT/FAST/社交/云游戏**（100M+ 订阅者量级）生效。**SecRelay 属于"small or nascent"，$100,000/年档甚至可能不触发**。**不要把 H.264 换成 AV1 仅仅为了躲许可**——技术风险和兼容性损失远大于这点费用。**但要在产品立项时把编解码许可做成一个显式的法务决策点（对应需求文档 D7）。**

#### 5.1.2 ★ AV1 在 WebRTC 屏幕共享的实际支持情况

**结论：2026 年仍不推荐在 SecRelay 的屏幕共享路径上启用 AV1。**

理由（按严重性排序）：
1. **编码延迟**：AV1 的实时编码器（SVT-AV1 低延迟模式 / rav1e）在 1080p60 下仍显著慢于 NVENC H.264/HEVC，"低延迟"模式需要牺牲压缩效率，且质量-延迟曲线在 <100ms 目标下很差。
2. **硬解覆盖率**：这是**致命项**。屏幕共享的**观看端**可能是任何一台 5 年前的笔记本。没有 AV1 硬解 ⇒ 软解 1080p60 AV1 在现代 CPU 上占用 30–60% 单核以上，笔记本会狂转风扇 + 掉帧。
3. **屏幕内容编码特性未普及**：AV1 规范里有很好的屏幕内容工具（palette mode、intra block copy），但**实时编码器对这些工具的实现与调优尚不成熟**。
4. **生态 bug**：如上表，Chrome/LiveKit 都有 AV1 屏幕共享相关的实际故障报告。
5. **浏览器互通的"Safari 问题"**：Safari 的 AV1 支持依赖硬件，Mac 上意味着 M3+ 才行。

**建议路线**：
```
M0-M2：只用 H.264 High Profile（硬编优先，软编 openh264 兜底）
M2-M3：HEVC 作为"双方能力协商一致且带宽受限"时的可选档（省 30–50% 码率）
M3+ ：为"双方都是新硬件"的路径**实验性**启用 AV1，默认关闭，且必须有可观测的开关与回退
```

#### 5.1.3 ★ "远程桌面文字清晰度"这个特殊需求

**这是整个调研里最重要的技术洞察之一。**

**核心矛盾**：通用视频编码的目标函数是 **"感知质量 vs 码率"**，它**允许**在文本区域产生模糊、振铃、色度偏移——因为人眼看视频时不会去读每一像素。但**远程桌面的用户会逐字阅读**，任何模糊都是"不可用"。

**实测参考**：远程串流方案（Sunshine/Moonlight）在 1080p60 下，普通游戏内容 10–20 Mbps 已很清晰，但**桌面文字场景即使 20–50 Mbps 的 H.264 也会有可见的文字模糊**，因为编码器的 RD 优化会牺牲高频细节。这是**为什么 Sunshine/Moonlight 用户会去调 `qpmax`/`qpmin` 或改用 HEVC/AV1** —— 但本质上是"用码率硬扛"。

**正确的方案是：不要用"整帧视频编码"来做远程桌面。** 采用**混合模型**：

| 层 | 技术 | 负责什么 |
|---|---|---|
| **① 变更检测层** | **脏矩形（dirty rectangle）/ 瓦片（tile）划分** | 屏幕大部分区域是静止的（文档、代码、静态 UI）。只对**发生变化的区域**编码。这能带来 **5–50×** 的码率节省（取决于使用模式） |
| **② 编码层** | 对变化区域**按内容类型分派编码器**：<br>· **文本/UI 区域** → 准无损编码（H.264 High Profile，`qp=0..10`，或专用的无损/准无损模式）<br>· **视频/游戏区域** → 常规有损 H.264/HEVC | 同一帧的不同区域用不同策略 |
| **③ 传输层** | 变化区域作为**独立的小帧**经 RTP 发送；未变区域**不重传**（接收端保留上一帧） | 码率只与"变化量"挂钩，与分辨率无关 |

**这正是 RDP/GFX、VNC、Sunshine/Moonlight 在做的事**，只是各家侧重不同：
- **RDP（Windows）**：完全不使用视频编解码作为主路径，走 **Graphics Pipeline Extension (GFX)**，基于 **H.264/AV1 编码的"区域"** + 脏矩形 + 图元级指令（RemoteFX / Progressive Codec）。文本用"无损位图区域"。
- **VNC 系（含 TigerVNC/TightVNC）**：脏矩形 + 多种编码（Tight/JPEG/ZRLE）。ZRLE 是无损的，文字完美，但视频性能差。
- **Sunshine/Moonlight**：**纯视频编码**（H.264/HEVC/AV1），靠**高码率 + 低 QP + 高帧率**硬扛文字清晰度。这是"游戏串流"思路，不是"远程桌面"思路。
- **RustDesk**：支持 VP8/VP9/H.264/AV1，有 "Auto" 模式；社区里有大量"Auto 模式总是选 AV1 导致问题"和"H.264/H.265 跨 iPadOS→macOS 不兼容"的讨论 —— 说明**编解码自动协商在真实世界里很容易出错**。来源：[RustDesk Discussion #5961](https://github.com/rustdesk/rustdesk/discussions/5961)、[#9236](https://github.com/rustdesk/rustdesk/discussions/9236)

> **★ 所以回答"有没有更适合的方案，比如 H.264 高质量 + 差分/局部更新"：有，而且这是唯一正确的方案。**
> **推荐给 SecRelay 的桌面场景**：
> 1. **第一阶段（M1）**：脏矩形 + H.264 高质量（QP 上限 20，允许关键帧无损）。先做对"只在变化区域产生码流"，这一步的收益比换编解码器大一个数量级。
> 2. **第二阶段（M2）**：把变化区域按"文本块 vs 视频块"分类（可用简单的启发式：边缘密度 + 颜色数量），文本块走准无损，视频块走有损。
> 3. **第三阶段（M3+）**：评估是否引入**屏幕内容编码扩展**（HEVC RExt SCC 的 transform skip / palette / intra block copy；AV1 的 palette / IBC）。但这些扩展的**实时编解码器支持很差**，可能不划算。
> 4. **摄像头场景**（FR-3/FR-4）用常规视频编码即可，不需要脏矩形。

**给"文字可读"定一个可验收的指标**（需求文档只写了"'可读'为硬指标"，需要量化）：
- 建议指标：**1080p、静态文档场景，≤ 2 Mbps 时，14px 中文/英文正文 100% 可辨（OCR 准确率 >99%、人工复核 0 处误读）**；滚动/打字时 ≤ 8 Mbps。
- 这个指标比"看起来清楚"可测得多，且能直接对比不同方案。

### 5.2 Rust 编解码 crate 现状（逐项核实）

**版本/日期来自 crates.io API 实测**（2026 年查询）：

| crate | 最新版本 | 最后更新 | 许可 | 编 | 解 | 成熟度评估 |
|---|---|---|---|---|---|---|
| **`ffmpeg-next`** | **9.0.0** | 2026-08 | WTFPL | ✅ | ✅ | ✅ **最实用**。下载 759 万。安全包装 ffmpeg-sys-next。**代价**：需要 FFmpeg 动态/静态库，**移动端交叉编译是主要工作量**（iOS/Android 需预编译 FFmpeg） |
| **`ffmpeg-sidecar`** | 2.6.0 | 2026-09 | MIT | ✅ | ✅ | ✅ **强烈推荐用于 MVP**。把 ffmpeg 当**子进程**调用，通过管道读写裸流。**零交叉编译地狱**（随包分发 static ffmpeg 二进制即可）。代价：进程 IPC 开销，不适合超低延迟逐帧 |
| **`x264`** / `x264-sys` | 0.5.0 / 0.2.3 | **2022-12** / 2026-04 | MIT / — | ✅ | ❌ | ⚠️ crate 本体 3 年未更新。GPL 许可问题（x264 是 GPL，**与 SecRelay 的 GPLv3 兼容，但若将来闭源就麻烦**）。**不推荐直接绑，走 FFmpeg** |
| **`openh264`** | **0.9.8** | 2026-08 | BSD-2 | ✅ | ✅ | ✅ **推荐做软编兜底**。Cisco 开源（BSD，专利由 Cisco 承担）。下载 133 万，维护活跃（2026 还在降 MSRV 以适配发行版打包）。**只支持 H.264 Baseline/Main/High，不支持 10-bit** |
| **`rav1e`** | **0.8.1** | **2025-06** | BSD-2 | ✅ | ❌ | ⚠️ 纯 Rust AV1 编码器，质量好但**慢**（设计目标是"比 x264 慢但比 libaom 快"）。**不适合实时屏幕共享**。且 1 年多未发新版 |
| **`svt-av1`（Rust 绑定）** | **crates.io 上不存在** | — | — | — | — | ❌ 没有可用的 Rust crate。要用只能自己写 FFI 绑 `SvtAv1Enc` C API（Intel 的 SVT-AV1 是实时 AV1 的最佳选择，但绑定要自研） |
| **`aom-sys` / `libaom-sys`** | 0.3.3 (2023-12) / 0.17.2+libaom.3.11.0 (2025-03) | 旧 | BSD | ✅ | ✅ | ⚠️ libaom 实时编码性能差（av1 实时模式仍慢）。**不推荐** |
| **`vpx-encode`** | **0.6.2** | **2022-08** | MIT | ✅ | ❌ | ❌ **4 年未更新**。`vpx-sys` 更是 2021 年的。**VP8/VP9 在 Rust 里没有好的现代绑定** |
| **`dav1d`** | **0.11.1** | 2025-11 | MIT | ❌ | ✅ | ✅ **AV1 解码的最佳选择**（最快软解）。另有 `shiguredo_dav1d`（2026-02 仍在更新，含预编译二进制，交叉编译友好） |
| **纯 Rust 编解码器** | — | — | — | — | — | ❌ **目前没有生产可用的纯 Rust H.264/HEVC/AV1 实时编解码器**。不要指望 |

#### ★ 关于 ffmpeg-next 的交叉编译现实（这是最重要的工程判断）

`ffmpeg-next` 是"正确"的答案，但在 5 平台（尤其 iOS/Android）上，**你需要为每个平台/架构预编译 FFmpeg 并配好 pkg-config**。这是数周的持续维护工作（FFmpeg 版本升级、NDK 版本升级、Xcode 版本升级都会打破它）。

**推荐策略（分阶段降低风险）**：
```
M0 探针：用 ffmpeg-sidecar（子进程 + 静态 ffmpeg 二进制）
         → 快速验证"抓屏 → 编码 → 传输 → 显示"整条链路
         → 桌面端足够（进程 IPC 延迟 <5ms，可接受）
M1 桌面：仍用 sidecar；同时并行验证 ffmpeg-next 在 Win/macOS/Linux 的构建
M2 移动：Android 用 MediaCodec（硬编，走 JNI）；iOS 用 VideoToolbox（走 objc2）
         → 移动端**不引入 FFmpeg**，规避交叉编译
         → 软件兜底：Android 可带 openh264（BSD，易编）；iOS 只有 VideoToolbox
```

> **这个"桌面用 FFmpeg、移动用系统硬编 API"的分叉是必要的**，因为移动端引入 FFmpeg 的成本（构建 + 包体积 + 上架审核）远大于收益，而移动端本来就有优秀的系统硬编 API。

### 5.3 硬件加速绑定现状

| 平台/API | Rust 生态 | 可用性 |
|---|---|---|
| **NVIDIA NVENC/NVDEC** | `nvenc`（0.1.0，2026-01，**下载量仅 2,036**）、`nvcodec`（0.1.1，2024，下载 2,710）、`cudarc`（0.19.10，成熟，但那是 CUDA 不是 NVENC 封装） | ⚠️ **crate 都很不成熟（下载量极低）**。**实用做法：通过 FFmpeg 的 `h264_nvenc` / `hevc_nvenc` 使用**（ffmpeg-next 或 sidecar 都能指定编码器名） |
| **Intel QSV** | 无成熟 Rust crate | ⚠️ 同样通过 FFmpeg `h264_qsv` / `hevc_qsv` |
| **AMD AMF** | 无成熟 Rust crate | ⚠️ 通过 FFmpeg `h264_amf` / `hevc_amf` |
| **Apple VideoToolbox** | `objc2` 系列 + 手写绑定；`screencapturekit`（11.0.0，2026-09，下载 247 万）证明 objc2 生态可行 | ✅ 可行，但要写一定量 unsafe FFI |
| **Android MediaCodec** | 通过 JNI（`jni` crate）+ 少量 Kotlin | ✅ 可行，是移动端唯一正解 |
| **Windows Media Foundation** | `windows` crate（官方） | ✅ 可行，但 DXGI 抓屏 + MF 编码的胶水代码不少 |

> **结论**：**硬编硬解统一走 FFmpeg 的编码器名字符串**（桌面）和**系统 API**（移动），不要直接绑 nvenc/qsv/amf。

### 5.4 屏幕采集 API（各平台现状）

| 平台 | API | OS 要求 | Rust 可用性 |
|---|---|---|---|
| **Windows** | **DXGI Desktop Duplication**（Win8+，最常用） / **Windows.Graphics.Capture**（Win10 1803+，现代 WinRT API，支持单窗口捕获 + 更好的 HDR/光标） | DXGI: Win8+；WGC: Win10 1803+ | `windows-capture` **2.0.1（2026-08，下载 190 万）** —— ✅ **推荐**，封装 Windows.Graphics.Capture |
| **macOS** | **ScreenCaptureKit**（macOS 12.3+，Apple 官方推荐；`CGDisplayStream` 已废弃） | macOS 12.3+ | `screencapturekit` **11.0.0（2026-09，下载 247 万）** —— ✅ **推荐**。需 TCC 屏幕录制授权 |
| **Linux (X11)** | XShm / XComposite | — | `xcap` **0.9.8（2026-08，下载 227 万）** —— ✅ 跨平台（Win/macOS/Linux）统一 API，**适合做抽象层的默认实现** |
| **Linux (Wayland)** | **PipeWire + xdg-desktop-portal**（`org.freedesktop.portal.ScreenCast`） | 需要 portal 实现（GNOME/KDE 均有） | `pipewire` **0.10.1（2026-08）** + `ashpd` **0.13.13（2026-07，下载 1,658 万）** —— ✅ 可行。**但用户每次要选共享哪个屏幕**（除非持久化授权，各 compositor 支持不一） |
| **Android** | **MediaProjection**（Android 5+） | Android 14+ **每次会话重新授权**（无法持久化） | 需 JNI + Kotlin。`xcap` 部分支持 |
| **iOS** | **ReplayKit**（`RPScreenRecorder` 前台 / `RPBroadcastSampleHandler` 广播扩展后台） | iOS 12+ | 需 objc2 手写。**广播扩展内存上限约 50MB**（需求文档已注意） |

> **`scrap`（0.5.0，2018）已死，不要用**。`xcap` 是它的现代替代。

### 5.5 丢包恢复、抖动缓冲、低延迟调参

#### 丢包恢复优先级（WebRTC 标准做法）

```
1. NACK + RTX  —— 首选。接收端检测到丢包 → RTCP NACK → 发送端重传（用独立 SSRC 的 RTX 流）
                  延迟代价 = 1 RTT。局域网/好网络下足够。
2. (ULP)FEC    —— 次选。RFC 5109（ULP FEC）/ RFC 8627（FlexFEC）。
                  在 NACK 来不及（高 RTT）或重传也丢时有用。代价：固定 ~10-30% 带宽开销。
                  自适应：只在观测到高丢包时开启。
3. 关键帧请求  —— 兜底。丢太多就连发 PLI/FIR 请求 I 帧。
                  代价大（I 帧是 P 帧的 5-20 倍），要限速（如 >500ms 间隔）。
```
**注意**：`str0m` 支持 NACK 但**没有自适应抖动缓冲**；`webrtc-rs` 相对完整。**如果选 str0m 做客户端，抖动缓冲要自研**。

#### 抖动缓冲深度

| 场景 | 建议深度 | 依据 |
|---|---|---|
| 局域网 / P2P 直连 | **20–40 ms** | 需求文档目标"局域网 ≤150ms 端到端" |
| 公网 P2P | **40–80 ms** | 需求文档目标"公网 P2P ≤300ms" |
| 经 TURN 中继 | **60–120 ms** | 中继增加抖动 |
| 移动网络（4G/5G） | **80–150 ms** | 切换基站会突增抖动 |
| **自适应** | 按"到达间隔的滑动分位数"（如 P95）+ 安全系数动态调整 | 这是 WebRTC NetEq 的做法，**必须实现**否则卡顿或延迟二选一 |

**预算分配（校验需求文档的 ≤150ms 局域网目标）**：
```
采集 DXGI/WGC           5–16 ms
编码 (NVENC 1080p60)    5–15 ms
打包 + 发送             1–3 ms
网络（局域网）           1–5 ms
抖动缓冲                20–40 ms
解码 (硬解)             3–8 ms
渲染 + 合成             8–33 ms（受 vsync 支配，60Hz = 16.7ms 一帧）
────────────────────────────────
合计                    43–120 ms  ✅ 满足 ≤150ms
```
> **注意**：**渲染/合成的 vsync 是最大的不可控项**。如果观看端用 60Hz 屏幕，最坏情况会引入 16.7ms。**建议：用"立即呈现 + 撕裂换低延迟"或"帧率匹配"策略**，而不是强制三缓冲。

#### ★ 低延迟编码参数（这是能立刻见效的调优清单）

| 参数 | 推荐值 | 原因 |
|---|---|---|
| **B 帧** | **禁用**（`bframes=0` / `-bf 0`） | B 帧需要未来的参考帧 → 必然引入编码延迟。**这是低延迟的第一铁律** |
| **参考帧数** | `refs=1`（H.264） | 减少 DPB 深度 = 减少延迟 |
| **GOP / 关键帧间隔** | **不用固定 GOP**。改用 **intra-refresh（周期内刷新）** 或**按需关键帧** | 固定长 GOP 会导致"新加入者等 I 帧"或"丢包后等 I 帧"。**intra-refresh 把 I 帧成本摊到多帧上**，避免码率尖峰 |
| **前瞻（lookahead）** | `rc-lookahead=0` | 前瞻 = 延迟 |
| **码率控制** | **CBR**（低延迟场景）或 **capped VBR**（有明确峰值上限）；**不要 ABR** | CBR 与 GCC 带宽估计配合最稳。ABR 会在场景切换时码率尖峰导致丢包 |
| **`-tune`** | `zerolatency`（x264）/ `ull`（NVENC 的 `-preset p1`+`-tune ull`） | 官方低延迟预设 |
| **slice/条带** | 开 `slices`（如 4）或 `slice-max-size` | 让丢包只影响部分条带，提高抗丢包性 |
| **熵编码** | H.264: `cabac=1` 但注意 `cabac` + 无 B 帧组合；HEVC: `cabac` | 压缩效率 |
| **质量上限** | **桌面文字场景：`qpmin=0`, `qpmax=18`**；摄像头场景 `qpmax=30` | 限制最差质量，保文字可读 |
| **多 pass** | 禁用 | 延迟 |

**x264 低延迟参考**：
```
-preset ultrafast -tune zerolatency -bf 0 -refs 1 -rc-lookahead 0
-keyint 0 (禁用固定 GOP) -x264-params "intra-refresh=1:qpmin=0:qpmax=18"
-b:v 8M -maxrate 8M -bufsize 8M (CBR, 1 秒缓冲)
```
**NVENC 低延迟参考**：
```
-preset p1 -tune ull -bf 0 -rc cbr -b:v 8M -maxrate 8M
```

#### A/V 同步

- 靠 **RTCP Sender Report (SR)** 里的 NTP 时间戳 + RTP 时间戳建立映射。
- 接收端用 SR 把音频和视频的呈现时间对齐到同一个**本地墙钟**。
- **WebRTC 里 RTCP SR 是自动处理的**（webrtc-rs 完整支持 SR/RR）；**str0m 也支持**（"Send/Recv Reports ✅"）。
- **注意 str0m 文档里的警告**：远端 SR 的 NTP 时间戳"can be very wrong compared to real NTP time"，且远端的时间流逝速率未必与本地一致。**生产级实现需要多源融合估计远端墙钟**（str0m FAQ 明确说"A production worthy SFU probably needs an even more sophisticated strategy"）。
- **对 SecRelay 的简化**：一对一场景，**把视频作为主时钟，音频跟随视频**（或反之），比分发多源简单得多。

---

## 6. E —— 文件传输设计

### 6.1 传输基底：QUIC stream vs WebRTC DataChannel

| 维度 | QUIC stream（`quinn`） | DataChannel（SCTP over DTLS） |
|---|---|---|
| 实现成熟度 | ✅ quinn 下载 3.36 亿，非常活跃 | ⚠️ webrtc-rs 的 SCTP 有历史 bug（流 ID 分配、零窗口死锁、慢消费者静默丢消息）；str0m 的 SCTP 较新 |
| 吞吐能力 | ✅ 好，QUIC 的流控 + BBR 可打满链路 | ⚠️ **受 GCC 限制**（为实时媒体调优）。**实测通常在链路带宽的 30–60%** |
| 多路复用 | ✅ 原生，每流独立流控，**流级 HOL 只在本流内** | ✅ 每条 DataChannel 是独立 SCTP stream |
| 背压 | ✅ QUIC 流控窗口天然背压，`write` 返回 pending | ⚠️ **必须自己写**；历史上慢消费者会被静默丢弃 |
| 部分可靠 | ❌（QUIC 有 DATAGRAM 但不保证） | ✅（`maxRetransmits` / `maxPacketLifeTime`） |
| 断点续传 | ✅ 流可重开，offset 由应用管理 | ⚠️ 需自建 |
| 跨 relay/NAT | 需自建（见 §2.2） | ✅ 复用 WebRTC 的 ICE/TURN |
| 浏览器互通 | ⚠️ WebTransport（Chrome 支持，Safari/Firefox 差） | ✅ 原生 |

> **★ 建议（与需求文档 §6.3 的"文件走可靠字节流"一致，但更明确）**：
> - **控制/消息/剪贴板小对象（< 8 MB）** → **DataChannel**（reliable, ordered，独立 channel，避免与小消息互相阻塞）
> - **大文件（> 8 MB）** → **独立 QUIC 连接**（`quinn`），与媒体连接并列
> - **中间地带（8 MB–1 GB）** → 也可走 QUIC；阈值可配置，按实测调
>
> **为什么不合并到一条 QUIC 连接**：媒体需要 QUIC DATAGRAM（不可靠、低延迟），文件需要 STREAM（可靠、高吞吐）。同一条 QUIC 连接里，DATAGRAM 与 STREAM 共享同一拥塞控制器——**文件流会把 cwnd 推高导致排队延迟上升，伤害媒体**。分成两条连接 = 两个独立拥塞控制域 = 可以显式做优先级。

### 6.2 ★ 分块与可验证流式传输

**核心设计：用 BLAKE3 的 Merkle 树做 verified streaming。**

BLAKE3 不只是"更快的 SHA-256"，它的**树形结构**让你可以：
- 把文件切成固定大小的 **chunk**（BLAKE3 的 chunk 是 1024 字节）
- 对 chunk 构建二叉 Merkle 树
- **接收端可以在收到一部分数据后，就验证这一部分确实属于目标文件**（不需要等整个文件）
- **支持"只下载文件的一部分并验证"**——这是断点续传/去重的密码学基础

**Rust 实现**：
- `blake3` **1.8.7** —— 官方实现，含 `bao` 风格的 verified streaming 支持
- `bao-tree` **0.16.1（2026-08）** —— 专做"可验证流式传输 + range 请求"的 crate（iroh 生态用）
- `iroh-blobs` **0.103.0** —— 更上层的封装（内容寻址 blob store + 传输协议），**如果想省事可以直接用**

**推荐的块结构**（融合 BLAKE3 与 Syncthing BEP 的经验）：

```
文件元数据 (经会话密钥加密后传输)：
  file_id          = BLAKE3(整个文件) 的 root hash  ← 内容寻址，天然去重
  size             = u64               ← 注意：必须 u64，不能 u32
  block_size       = 128 KiB .. 16 MiB（见下表）
  blocks[]         = { offset: u64, size: u32, hash: [u8; 32] }
                       ↑ 每个块的 BLAKE3 哈希（不是 BLAKE3 chunk 的哈希，是"块"的哈希）
  mtime, permissions, symlink_target, ...
```

#### ★ 块大小选择（直接采用 Syncthing 的成熟规则）

Syncthing BEP v1 的规则：**"最小的能满足块数 < 2000 的 2 的幂"**，得到这张表：

| 文件大小 | 块大小 |
|---|---|
| 0 – 250 MiB | **128 KiB** |
| 250 MiB – 500 MiB | 256 KiB |
| 500 MiB – 1 GiB | 512 KiB |
| 1 GiB – 2 GiB | 1 MiB |
| 2 GiB – 4 GiB | 2 MiB |
| 4 GiB – 8 GiB | 4 MiB |
| 8 GiB – 16 GiB | 8 MiB |
| 16 GiB – 更大 | 16 MiB |

来源：[Syncthing Block Exchange Protocol v1](https://docs.syncthing.net/specs/bep-v1.html)

**为什么这个规则好**：
- **块数上限 2000** ⇒ 元数据（块哈希列表）大小可控（2000 × 32 B = 64 KB，加上 offset/size 约 100 KB），**可一次性传输**；
- **128 KiB 起步** ⇒ 小于此的文件只有 1 个块，哈希开销可忽略；
- **按 2 的幂增长** ⇒ 与 Merkle 树的层级对齐，便于做"部分验证"；
- **块大小在整个文件内固定**（除最后一块）⇒ 支持**随机偏移读取**（这是断点续传的前提）。

> **验证**：>4 GiB 文件（需求文档 FR-7 明确要求）在 2–4 GiB 档用 2 MiB 块 ⇒ 一个 5 GiB 文件 = 2560 块 × 2 MiB，元数据 = 2560 × 40 B ≈ 100 KB。**完全可接受。**

### 6.3 ★ 断点续传 / 恢复

**状态模型**（每文件一个）：

```
TransferState {
  file_id:        [u8; 32],       // BLAKE3 根哈希（也用于校验"源文件是否变了"）
  size:           u64,
  block_size:     u32,
  total_blocks:   u32,
  received:       BitVec,          // 位图：哪些块已经收齐且校验通过
  partial_path:   PathBuf,         // 临时文件（.secrelay-part）
}
```

**关键设计点**：

1. **持久化时机**：每收到 N 块（建议 N = 32，或每 4 MB，或每 2 秒）**flush 一次位图**。移动端用 SQLite（或带 fsync 的 append-only 日志）。**崩溃后最多重传 4 MB。**
2. **源文件变更检测**：
   - 重连时先比对 `(size, mtime_ns, file_id)`。
   - 若 `size` 或 `mtime` 变了 → **不信任位图，重新计算块哈希**，与旧位图对比，只重传真正变化的块（这比全量重传好得多）。
   - 若 `file_id`（BLAKE3 根哈希）变了 → 视为新文件，**另起一个传输**（保留旧的部分文件以便潜在复用，取决于磁盘策略）。
3. **临时文件写入 + 原子重命名**：
   - 写入 `.secrelay-part` → 全部块校验通过 → `rename()` 成最终文件名。**rename 在 POSIX 和 Windows（同卷）上都是原子的**。
   - **这避免了"用户看到一个半截文件"**，也避免了下一次同步把它当成真实文件。
4. **块请求策略（SyncThing 启发）**：
   - **滑动窗口 + 并发请求**（如窗口 64 块、并发 16 个请求），而不是严格顺序。这样能容忍个别块丢失而不停等。
   - **优先请求"稀有块"**（多源场景）；一对一场景退化为顺序 + 小窗口。
5. **主动进度通告**：Syncthing 每 5 秒发一次 `DownloadProgress`（APPEND/FORGET），且**只在状态变化时发**。这允许**对端**知道"我有哪些块可以提供"，从而支持"多源同时下载"和"从部分文件续传"。**SecRelay 一对一场景可简化，但保留该消息类型以便将来扩展。**

**参考**：Syncthing BEP 的消息集（ClusterConfig / Index / IndexUpdate / Request / Response / DownloadProgress / Ping / Close）+ **Delta Index Exchange** 机制（用 `{index_id, max_sequence}` 告知对端"我上次同步到的位置"，只传增量）。来源：[Syncthing BEP v1](https://docs.syncthing.net/specs/bep-v1.html)

> **给 SecRelay 的建议**：**不要重新发明 BEP。直接参考它的消息结构（用 prost/protobuf 定义），但简化到一对一场景**：去掉 introducer、多设备 version vector、folder sharing mode 等。这样能得到一个经过 10 年实战检验的协议骨架。

### 6.4 ★ 去重

**两种去重，用途不同**：

| 类型 | 算法 | 块边界 | 优点 | 缺点 | 用途 |
|---|---|---|---|---|---|
| **固定块去重** | 按块大小切分 | 固定 offset | 实现简单、可随机访问、与 BEP 兼容 | **插入一个字节会导致后续所有块都不匹配**（边界偏移问题） | ✅ **传输去重**（配合 BEP 的固定块） |
| **内容定义分块（CDC）** | Rabin 指纹 / **FastCDC** / gear hash / buzhash | 由内容决定 | 插入/删除只影响局部块，**去重率高得多** | 块大小可变、需索引、随机访问需索引 | ✅ **本地存储去重**（同一文件多次同步、相似文件的重复内容） |

**Rust 实现**：`fastcdc` **5.0.0（2026-08）**，MIT，下载 212 万 —— **FastCDC 是当前最佳选择**（比 Rabin 快、比原始 CDC 去重率高）。目标块大小建议 **64 KiB–256 KiB**（均值），最小 16 KiB，最大 1 MiB（用 `fastcdc::v2020::FastCDC` 的 min/avg/max 配置）。

**去重的作用域（安全约束，见 §4.6）**：
```
✅ 允许：客户端本地维护"我已拥有的块哈希集合"
        → 传输前先算源文件块哈希，与本地集合比对
        → 只请求缺失的块（可减少 30-90% 的传输量，尤其"同一文件传到多台设备"）

❌ 禁止：服务器端看到密文相同就判定重复
        → 这需要收敛加密 → 内容确认攻击
```

**多设备场景的优化**：
- 当把同一文件从 A 传到 B、C、D 时，**A 只需上传一次**（如果中继/其他设备协助）。
- 但 SecRelay 是一对一为主，**MVP 只需"本地块索引"级别的去重**：A→B 传完，B 的记录里有这些块哈希；下次 B→C 传同一文件时，A 可以提供（如果 A 也在线）。

### 6.5 目录同步

**模型选择**：
- **完整版（Syncthing 式）**：每个文件/目录有 **version vector**（`{device_id → counter}`），冲突用"更高版本向量胜出"。
- **SecRelay 一对一版（推荐，简化）**：用**单调递增的逻辑时钟 + 设备 ID 的 tie-break**：
  - 每个条目存 `(lamport_clock: u64, last_writer: DeviceId)`
  - 冲突（两端都改了同一文件）→ 比较 `(clock, device_id)` 字典序，大的胜出
  - **败者不删除，重命名为 `filename.conflict-{device}-{timestamp}`**（这是 Syncthing 的做法，避免静默数据丢失）

**需要处理的操作**：
| 操作 | 表示 | 注意 |
|---|---|---|
| 新建文件 | FileInfo with blocks | — |
| 修改文件 | 新 version + 新块列表 | 与旧块列表对比，只传变化的块 |
| **重命名** | **BEP 里没有 rename 操作** → 表现为"删除旧的 + 新建新的"。但**块哈希相同 ⇒ 接收端可本地 rename，不重传数据** | 优化点：检测"删除 + 新增且块列表相同" ⇒ 本地 rename |
| 删除 | `deleted=true` + 空块列表 | 必须保留 tombstone（否则"删除"会被对端的旧索引"复活"） |
| 目录 | `type=DIRECTORY` | — |
| 符号链接 | `type=SYMLINK` + `symlink_target` | ⚠️ **Windows 需要管理员权限或开发者模式**；Android/iOS 基本不支持。**建议：默认不跟随、不创建 symlink，仅记录并在 UI 提示** |

**元数据的跨平台差异（必须显式处理）**：
| 元数据 | Windows | Linux | macOS | Android | iOS |
|---|---|---|---|---|---|
| Unix 权限位 | ❌（用只读位模拟） | ✅ | ✅ | ⚠️ | ❌ |
| 修改时间精度 | 100 ns | ns | ns（HFS+ 是秒） | ms | ns |
| 文件名大小写 | 不敏感（但保留大小写） | 敏感 | **默认不敏感**（APFS 可配） | 敏感 | 不敏感 |
| 文件名非法字符 | `\ / : * ? " < > \|` | 只有 `/` 和 NUL | `:`（旧）/ `/` | 同 Linux | 同 macOS |
| 路径长度上限 | 260（可解除） | 4096 | 1024 | 4096 | 1024 |
| 保留名 | `CON PRN AUX NUL COM1..9 LPT1..9` | — | — | — | — |
| 稀疏文件 | ✅ | ✅ | ✅ | ❌ | ❌ |
| 扩展属性 | ADS | xattr | xattr + resource fork | ❌ | ❌ |

> **`block_size` 用 `int32`（Syncthing 那样）够用**（最大 16 MiB），但 **`size` 和 `offset` 必须是 `int64`**。Syncthing 的 BEP 用的是 `int64 size` 和 `int64 offset` 和 `uint64 modified_by`——**照抄这个位宽**，不要图省事用 u32。

### 6.6 大文件（> 4 GB）与移动端存储

#### 32 位溢出陷阱（清单）

| 陷阱 | 后果 | 对策 |
|---|---|---|
| 文件大小用 `u32`/`i32` | >4 GiB 溢出为 0 或负数 | **全部用 `u64`**。Rust 的 `std::fs::metadata().len()` 已经是 u64 ✅ |
| 块 offset 用 `u32` | 同上 | `u64` |
| **文件读写 offset 用 `u32`** | `seek(SeekFrom::Start(x as u32))` 截断 | 用 `u64`。**Rust 的 `Seek` trait 已经是 u64** ✅ |
| 位图索引用 `u32` | 块数超 40 亿才溢出，实际不会 | u32 可接受，但用 `u64` 更稳 |
| 进度用 `f32` | >16M 后精度丢失 | 用 `u64` 字节数 + 计算百分比 |
| **JSON/protobuf 里用 `int32`** | 序列化截断。**protobuf 的 `int32` 对 >2GiB 的值会出错** | 用 `int64`/`uint64`；protobuf 中**绝不用 `int32` 表示 size/offset** |
| FAT32/exFAT 单文件 4 GiB 上限 | 写入失败 | 见下 |

#### 文件系统限制

| 文件系统 | 单文件上限 | 出现场景 |
|---|---|---|
| **FAT32** | **4 GiB**（硬限制） | U 盘、SD 卡、部分 Android 外置存储、相机卡 |
| exFAT | 16 EiB | SD 卡（>32GB 通常格式化为此）、跨平台移动盘 |
| NTFS | 16 EiB | Windows |
| APFS / HFS+ | 8 EiB / 8 EiB | macOS |
| ext4 | 16 TiB | Linux |
| Android 内部存储 | 通常 ext4/f2fs，无 <4GiB 限制 | — |

> **对策**：**接收前先检查目标文件系统的可用空间 + 单文件上限**（`statfs`/`GetVolumeInformation`）。若目标是 FAT32 且文件 >4 GiB → **在传输开始前就报错并提示用户换目标位置**，不要传了 4 GiB 才失败。这是"浪费用户几十分钟流量"的体验杀手。

#### ★ Android 存储权限（关键政策风险）

| 方案 | API | 限制 |
|---|---|---|
| **App 私有目录** | `getExternalFilesDir()` | ✅ 无需权限。但**用户文件管理器看不到**，卸载即删 |
| **MediaStore** | `MediaStore.Downloads` / `MediaStore.Images` | ✅ **推荐用于"保存收到的文件"**。无需 MANAGE_EXTERNAL_STORAGE。Android 10+ 可用 |
| **SAF（Storage Access Framework）** | `ACTION_OPEN_DOCUMENT_TREE` + `DocumentFile` | ✅ **推荐用于"用户指定任意目录"**。返回一个可持久化 URI 权限（`takePersistableUriPermission`） |
| **`MANAGE_EXTERNAL_STORAGE`**（"所有文件访问权限"） | `ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION` | ⚠️ **Google Play 严格限制**：只有文件管理器、备份/恢复、杀毒、文档管理等核心功能类应用才获批。**SecRelay 拿到这个权限的把握不大。** 来源：[Android "Manage all files on a storage device"](https://developer.android.com/training/data-storage/manage-all-files) |

> **Android 存储策略（推荐）**：
> 1. **默认目标**：`MediaStore.Downloads`（收文件）+ App 私有目录（临时/缓存）
> 2. **高级选项**：让用户用 SAF 选一个目录作为"SecRelay 下载目录"，持久化 URI 权限
> 3. **不要依赖 `MANAGE_EXTERNAL_STORAGE`**——上架风险太高
> 4. **发送端选择文件**用 SAF（`ACTION_OPEN_DOCUMENT`），这样不需要读存储权限
> 5. **注意 `DocumentFile` 的性能**：SAF 的随机访问很慢（每次 `openInputStream` 可能是新的 fd）。大文件用**顺序读写 + 大 buffer**，避免频繁 seek

#### ★ iOS 存储与后台

| 限制 | 具体 | 对策 |
|---|---|---|
| **无通用文件系统访问** | 沙盒 + 用户通过 `UIDocumentPickerViewController` 显式选择 | 接收文件存到 App 沙盒的 Documents（开启 `UIFileSharingEnabled` + `LSSupportsOpeningDocumentsInPlace` 让用户在"文件"App 里看到）；导出用 `UIActivityViewController` 或 `UIDocumentPicker` |
| **安全作用域书签** | 访问用户选的文件后，需 `bookmarkData(options: .minimalBookmark)` 保存，下次用 `URL(resolvingBookmarkData:)` 恢复 | **必须做**，否则 App 重启后失去访问权限 |
| 照片库 | `PHPhotoLibrary` 保存图片/视频 | 需要 `NSPhotoLibraryAddUsageDescription` |
| **后台执行严格受限** | `BGTaskScheduler`（`BGAppRefreshTask` / `BGProcessingTask`），**由系统决定何时运行，不保证**；`beginBackgroundTask` 只给约 30 秒 | ⚠️ **iOS 无法可靠地在后台做长时 socket/QUIC 传输** |
| 后台传输 | `URLSession` 的 `background` configuration + `URLSessionDownloadTask` | ✅ **唯一可靠的长时后台下载机制**。但**它是 HTTP(S) 的**，不是 QUIC/自定义协议 |
| 保持前台 | 音频后台模式、VoIP push、`UIApplication.beginBackgroundTask` | ⚠️ 滥用会被拒审 |

> **★ iOS 后台传输的现实结论**（这会影响需求文档 §5 的"后台续传"承诺）：
> - **iOS 上的大文件传输应该在应用处于前台时进行**，并给用户明确的"请保持 App 在前台"提示 + 进度条 + 屏幕常亮（`isIdleTimerDisabled`）。
> - 如果必须支持后台，**唯一可行路径是服务器端提供一个 HTTPS 的中转端点**，让 iOS 用 `URLSession` background 从服务器拉（或推）。但这就**打破了"服务器零知识"**（服务器能看到密文——不过如果文件是端到端加密的，服务器仍然只是转发密文，**零知识可以保持**！只是多一次服务器往返 + 存储）。
> - **可选的折中**：iOS 后台时，让**接收端的桌面设备先把文件存到中继的临时暂存区**（服务端只见密文），iOS 前台后从暂存区续传。**这需要中继提供存储**，是产品决策（成本 + 隐私）。
> - **不要向用户承诺"iOS 后台无缝续传"**。

**Android 后台**：
- 前台服务 + 常驻通知（`FOREGROUND_SERVICE_DATA_SYNC`，Android 14+ 需要声明 service type）可以让传输持续，但**厂商省电策略（小米/华为/OPPO/vivo）会杀**。
- **必须做**：引导用户把 App 加入"电池优化白名单"（`REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`），并在被后台杀死后支持恢复（靠 §6.3 的持久化位图）。

### 6.7 传输与媒体的带宽公平

**问题**：传一个 10 GB 文件时，不能让屏幕流卡成幻灯片。

**设计（分层）**：

```
1. 两条独立连接（媒体 / 文件） ⇒ 独立拥塞控制域，互不干扰 cwnd
   ⚠️ 但它们共享同一条物理链路 ⇒ 在瓶颈路由器上仍会互相排队

2. 应用层限速器（token bucket）
   · 监控：媒体流的 RTT / 丢包率 / 实际码率（从 webrtc-rs 的 stats 拿）
   · 若媒体丢包 > 2% 或 RTT 上升 > 50%  ⇒ 文件流限速到当前 30%
   · 若媒体稳定 10 秒  ⇒ 文件流逐步提速（+20%/秒，上限链路估计）
   · 用户可以手动设"文件传输不限速"，但要显示警告

3. QoS / DSCP 标记（可选，企业网络有效）
   · 媒体 RTP：EF (46)
   · 文件：AF11 (10) 或 BE (0)
   · 需要 OS 支持（Windows QoS API / Linux SO_PRIORITY / macOS）

4. 用户可见的优先级控制
   "传文件时降低画质" / "优先文件传输" / "平衡"（默认平衡）
```

**移动端额外的考量**：
- **蜂窝网络下默认限制文件传输**（除非用户在 WiFi 上），或至少**明确的确认对话框 + 流量估算**。
- **电池/热**：编码是耗电大户。建议：
  - 移动端推流时**检测热状态**（iOS `ProcessInfo.thermalState`，Android `PowerManager.getCurrentThermalStatus`），到 `.serious`/`THERMAL_STATUS_SEVERE` 时**主动降帧率（60→30）而非降分辨率**（降分辨率会让文字不可读，降帧率只影响流畅度）。
  - **充电时才允许 60fps**；电池供电默认 30fps（可配）。

---

## 7. ★ 最终推荐方案与风险

### 7.1 推荐的一套组合（可直接作为架构决策）

```
┌────────────────────────────────────────────────────────────────────┐
│ SecRelay 传输与媒体栈 —— 推荐组合                                    │
├────────────────────────────────────────────────────────────────────┤
│                                                                    │
│ 【媒体通道】WebRTC 语义（webrtc-rs 0.21.x，不用 0.17 分支）          │
│   · ICE 打洞（host/srflx/relay 候选，IPv6 优先，并行赛跑）           │
│   · 失败 → 自建 coturn 集群（中继零知识，支持 TCP/443 回退）         │
│   · DTLS-SRTP（跳跳） + SFrame RFC 9605（端到端，满长 tag）          │
│   · 拥塞控制 GCC/TWCC（白拿）+ NACK/RTX + 自适应 FEC                │
│   · 抖动缓冲：20–40ms(局域网) / 40–80ms(公网) / 80–150ms(移动)      │
│                                                                    │
│ 【文件通道】独立 QUIC 连接（quinn 0.11.x）                           │
│   · 双流向流 + 128KiB–16MiB 块（Syncthing 规则）                     │
│   · BLAKE3 块哈希 + Merkle 验证流 + 位图断点续传                     │
│   · FastCDC 本地去重（64–256KiB 均值）+ 临时文件原子 rename          │
│   · 应用层限速器保证不饿死媒体                                        │
│                                                                    │
│ 【控制/消息通道】同一 QUIC 连接上的小 stream                         │
│   · 信令、能力协商、剪贴板、桌面提示、文字消息                        │
│   · Noise_IK（snow 0.10）用于跨 relay 的端到端加密                   │
│   · 文字消息若需异步可达 → vodozemac 0.11（Olm 双棘轮）              │
│                                                                    │
│ 【编解码】                                                          │
│   桌面：H.264 High Profile（NVENC/QSV/AMF/VideoToolbox 硬编优先）    │
│         + openh264 0.9.8 软编兜底                                   │
│         + ★脏矩形/瓦片差分 + 准无损文本区域（这是文字清晰度的正解）  │
│   移动：MediaCodec(Android) / VideoToolbox(iOS) 硬编，不引入 FFmpeg  │
│   MVP 快捷路径：ffmpeg-sidecar（子进程，零交叉编译）                 │
│   参数：无 B 帧、refs=1、CBR/capped-VBR、intra-refresh、qp≤18(桌面)  │
│   可选增强：HEVC（双方协商一致且带宽受限时）；AV1 暂缓               │
│                                                                    │
│ 【身份与配对】                                                      │
│   Ed25519(身份) + X25519(密钥交换)，存平台安全存储                   │
│   配对 = QR(带外公钥分发) + SPAKE2(6位码 PAKE，RFC 9382)            │
│        + SAS 6位数字双向比对（派生自 session_key）+ 本地信任列表      │
│   禁止纯在线首次配对（必须带外）                                      │
│                                                                    │
│ 【中继兜底】自建 coturn 集群 + 自研信令/消息 relay                    │
│   · 多区域 + DNS 加权；UDP 中继端口池；TLS/DTLS 证书                 │
│   · TCP/443 回退（企业防火墙场景必需）                               │
│   · 服务器不解密（TURN 天然如此；自研 relay 只转发 E2E 密文）         │
│   · 目标：中继占比 < 15%（靠 IPv6 优先 + 并行直连升级 + NAT-PMP）    │
│                                                                    │
│ 【不上】SFU（LiveKit/mediasoup）—— 一对一不需要，只在多人时再评估    │
│ 【不上】libp2p / WireGuard / 纯自建 UDP / 自研 QUIC 媒体层            │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

### 7.2 分阶段落地建议（与需求文档的 M0–M3 对齐）

| 阶段 | 传输/媒体要做的事 | 判据 |
|---|---|---|
| **M0 探针（1–2周）** | ① 部署 coturn，跨运营商/4G 实测打洞成功率<br>② ffmpeg-sidecar + DXGI/WGC 抓屏 → 编码 → 本地回环 → 显示，测端到端延迟<br>③ 测 1080p 文字的"可读最低码率"<br>④ **测 webrtc-rs 0.21 的连接创建/销毁内存曲线**（验证泄漏是否真的修好） | 打洞 >80%；局域网延迟 <150ms；找到文字可读码率；内存无线性增长 |
| **M1 桌面 MVP** | 打通 webrtc-rs 媒体 + quinn 文件双栈；配对（QR+SPAKE2+SAS）；脏矩形差分 | 三端互看桌面可用；文件续传可用；中继回退 <3s |
| **M2 移动** | MediaCodec/VideoToolbox 硬编；SAF/MediaStore/iOS 沙盒；移动端抖动缓冲加宽；热降帧 | 手机能看能收；推流 30 分钟不过热降频到不可用 |
| **M3+** | HEVC 协商；ios 后台中转方案；多人/SFU 评估；vodozemac 消息 | — |

### 7.3 ★ 最大的 3 个技术风险

#### 风险 1：WebRTC Rust 生态仍在动荡期，选型可能"押错版本"

- **事实**：`webrtc-rs` 在 2026 年初经历了一次**架构级重写**（0.17 特性冻结 → sans-I/O `rtc` crate → 0.21 整合）。官方承认旧架构有**每连接 ~109 KiB 的线性内存泄漏**（实测 `111 KiB × N + 172 KiB`）。0.21.0 修了 SCTP 流 ID、零窗口死锁、慢消费者静默丢消息、Android 后台 ICE restart 卡死等问题——**说明这个栈的坑仍在被逐个发现**。
- **另一侧**：`str0m` 虽然代码质量最高、最"Rust 味"，但**官方明确不支持 TURN、不支持网卡枚举、没有自适应抖动缓冲，且 iOS/Android 仅编译未测试**——对 SecRelay 的 5 平台要求是硬伤。
- **影响**：可能在 M2 阶段遇到"必须 fork 库或换库"的重大返工。
- **缓解**：
  1. **在 `secrelay-transport` 之上做自己的 trait 抽象层**，把 `webrtc-rs` 的用法收敛到一个 crate 里（需求文档 §6.4 的结构已经对了，要强化这条）。
  2. M0 就用**连接创建/销毁 1000 次的内存曲线**作为框架选型的硬判据。
  3. pin 到 `webrtc = "0.21"`，**不要用 `0.17`**，并在 CI 里监控依赖更新。
  4. 备选方案预先验证：如果 `webrtc-rs` 出问题，**fallback 是 `iroh` + 自研媒体层**（代价是放弃浏览器互通）。

#### 风险 2：远程桌面"文字清晰度"与通用视频编码的目标函数冲突（唯一必须自研的核心）

- **事实**：通用视频编码器的 RD 优化**主动牺牲文本高频细节**以换码率。远程桌面用户**逐字阅读**，任何模糊都不可用。参考实现的现状：Sunshine/Moonlight 是"纯视频编码 + 高码率硬扛"（游戏串流思路）；RustDesk 社区里大量"Auto 模式选错编解码器"的抱怨说明自动协商不可靠；RDP 走的是**完全不同的路**（脏矩形 + 图元/无损区域）。
- **影响**：如果 M1 只做"整帧 H.264 编码"，用户会立刻反馈"字看不清"，而这**不是调参数能解决的**——需要**重做采集→编码之间的管线**（加入变更检测、区域分类、混合编码）。
- **缓解**：
  1. **把脏矩形/瓦片差分作为 M1 的必做项**，不是优化项。它的收益（5–50× 码率）比"换编解码器"（1.5–2×）大一个数量级。
  2. M0 就**量化"文字可读最低码率"**（建议指标：1080p 静态文档 ≤2 Mbps 时 OCR 准确率 >99%），作为后续所有优化的对比基线。
  3. **不要相信"AV1 的屏幕内容工具能解决"**——实时编码器对 palette/IBC 的支持很差，且观看端硬解覆盖率不足（老 PC 软解 AV1 会 CPU 爆炸）。已在 §5.1.2 列出 LiveKit/Chromium 的实际 AV1 屏幕共享 bug。
  4. 接受"桌面场景和摄像头场景是两条不同的编码路径"，不要试图统一。

#### 风险 3：NAT 穿透失败率 × 视频中继带宽成本 = 商业模式风险

- **事实**：
  - 通用 WebRTC 产品的 TURN 回退率约 **15–25%**，企业 WiFi 与受限移动运营商更高；**IoT/CGNAT 场景接近 100%**。
  - **1080p 中继一小时 ≈ 1.8 GB（单向）/ 3.6 GB（双向计费）**。
  - 云出口价的**市场价差达 8 倍**（Twilio $0.40/GB vs Cloudflare $0.05/GB）。规模化示例：90 TB/月的中继流量在 Twilio 是 **$36,000/月**，在自建 coturn 约 **$4,650/月**。
  - **"换更大的 TURN 服务器"完全不是成本杠杆**——服务器成本几乎固定且可忽略，成本全在带宽。
  - Cisco 独立报告（业界共识）指出 **AVC/HEVC/VP9/VVC/AV1 的版税可能把大型平台推向九位数年支出**；H.264 的 Via LA 2026 新费率 Tier 1 达 $4.5M/年。
- **影响**：如果中继占比失控（比如移动端场景大量回退），**带宽成本可能超过订阅收入**。同时编解码许可是一个**法务**风险而非技术风险，但需要**在架构里留好切换空间**。
- **缓解**：
  1. **把"中继占比"做成产品级 KPI，从 M0 就开始埋点**。目标 **<15%**；每降 10 个百分点，中继账单降约 40%。
  2. **IPv6 优先**是零成本杠杆（全球 IPv6 访问已于 2026-03 首次突破 50%）——AAA A 候选必须并行尝试。
  3. 采用 **Tailscale 式"总是先经中继建连、再后台并行升级直连"**：用户永不因打洞等待，且直连成功后立即切走。
  4. **中继自建在带宽便宜的地方**（自建机房/大流量包云），而不是标准云出口价；多区域 + DNS 加权。
  5. 实现 **NAT-PMP/PCP/UPnP 端口映射**并监控映射 epoch（路由重启自动重建）。
  6. **编解码许可作为显式法务决策点**（需求文档 D7）：SecRelay 大概率落在"small or nascent"档（$100K/年），但要**在协议里保留编解码可切换能力**，以便将来用 HEVC/AV1 规避。
  7. **用户在移动网络（蜂窝）下传文件/推流要给明确的流量提示**，避免"用户被扣光流量然后卸载 App"。

---

## 8. 参考来源汇总

**传输与 NAT**
- [webrtc v0.17.0: Feature Freeze and Shifting to Sans-I/O (rtc crate)](https://webrtc.rs/blog/2026/01/31/webrtc-v0.17.0-feature-freeze-sansio-shift.html)
- [Announcing rtc 0.3.0: Sans-I/O WebRTC Stack for Rust](https://webrtc.rs/blog/2026/01/04/announcing-rtc-v0.3.0)
- [webrtc-rs/webrtc v0.21.0 release notes](https://newreleases.io/project/github/webrtc-rs/webrtc/release/v0.21.0)
- [docs.rs/str0m 0.24.1（能力矩阵与项目状态）](https://docs.rs/str0m/latest/str0m/)
- [iroh 1.0.2 – iroh-relay Security Fix](https://www.iroh.computer/blog/iroh-1-0-2)
- [iroh Dedicated Hosting 定价](https://docs.iroh.computer/iroh-services/relays/managed)
- [Switch from webrtc-rs to str0m · rust-libp2p #3659](https://github.com/libp2p/rust-libp2p/issues/3659)
- [How Tailscale is improving NAT traversal (pt 1)](https://tailscale.com/blog/nat-traversal-improvements-pt-1)
- [TURN Bandwidth Calculator: What Your WebRTC Relay Traffic Actually Costs](https://www.forasoft.com/learn/video-streaming/articles-streaming/turn-bandwidth-calculator)
- [18 Years Later, IPv6 Reaches Majority — ISOC Pulse](https://pulse.internetsociety.org/en/blog/2026/04/18-years-later-ipv6-reaches-majority/)
- RFC 8445 (ICE) / RFC 8489 (STUN) / RFC 8656 (TURN) / RFC 9221 (QUIC DATAGRAM)

**加密与身份**
- [RFC 9605 Secure Frame (SFrame)](https://datatracker.ietf.org/doc/rfc9605/)
- [LiveKit Encryption Overview](https://docs.livekit.io/transport/encryption/)
- [RFC 9382 SPAKE2](https://www.rfc-editor.org/rfc/rfc9382.html)
- RFC 9383 (SPAKE2+) / RFC 9807 (OPAQUE) / RFC 5764 (DTLS-SRTP) / RFC 3711 (SRTP) / RFC 8723 (SRTP double encryption) / RFC 9420 (MLS)

**编解码与媒体**
- [H.264 Streaming Fees: What Changed, Who's Affected, and What It Means — Streaming Media](https://www.streamingmedia.com/Articles/ReadArticle.aspx?ArticleID=173935)
- [LiveKit issue #3430: Screen sharing intermittently does not work, possibly due to AV1](https://github.com/livekit/livekit/issues/3430)
- [Chromium issue 459528902: Video of shared screen in Google Meet freezes occasionally](https://issues.chromium.org/issues/459528902)
- [RustDesk Discussion #5961（Auto codec 选到 AV1）](https://github.com/rustdesk/rustdesk/discussions/5961)
- [RustDesk Discussion #9236（H.264/H.265 跨 iPadOS→macOS）](https://github.com/rustdesk/rustdesk/discussions/9236)
- [Access Advance HEVC Advance / VVC Advance 定价（至 2030）](https://accessadvance.com/2025/07/21/access-advance-announces-hevc-advance-and-vvc-advance-pricing-through-2030/)
- RFC 5109 (ULP FEC) / RFC 8627 (FlexFEC)

**文件传输**
- [Syncthing Block Exchange Protocol v1](https://docs.syncthing.net/specs/bep-v1.html)
- [Android: Manage all files on a storage device](https://developer.android.com/training/data-storage/manage-all-files)

**crates.io 版本数据**（2026 年实测查询，见 §5.2 / §2 各表）

---

## 9. 需要在动工前额外拍板的决策点（补充需求文档 §9）

| # | 问题 | 为什么重要 |
|---|---|---|
| D11 | **是否接受"文件走独立 QUIC 连接"（意味着要自建这条路的打洞/中继）** | 若坚持"全部复用 WebRTC 一条连接"，文件吞吐会受限且与媒体互相干扰 |
| D12 | **是否需要浏览器端观看**（Web 端只是"看"，不作源） | 决定 WebRTC 是否是硬约束。若要，iroh 出局；若不要，iroh + 自研媒体是更省事的路 |
| D13 | **中继是否提供临时存储**（用于 iOS 后台传输 / 离线消息） | 引入存储 = 成本 + 隐私 + 合规（"零知识"仍可保持，但用户会质疑） |
| D14 | **文字消息是否需要"异步可达"**（对方离线时能收到） | 决定是否引入 vodozemac 双棘轮 + 服务端密文队列 |
| D15 | **编解码许可的承担意愿**（H.264 $100K/年档 vs 转 HEVC/AV1） | 法务决策，但影响编解码架构（是否要保留多编解码器切换） |
| D16 | **是否允许用户在蜂窝网络下传大文件** | 流量成本与差评风险 |
