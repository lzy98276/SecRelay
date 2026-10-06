//! 跑在真实网络上的传输：ICE 打洞 + DTLS/SCTP DataChannel。
//!
//! 一帧编码成一条二进制 DataChannel 消息 —— SCTP 本身面向消息，不需要再自己分帧。
//! 所有频道共用这一条 DataChannel：频道信息已经在帧头里。
//!
//! SDP 怎么交换由调用方决定（当前不依赖信令服务器），传输层只提供三步：
//! [`WebRtcTransport::create_offer`] → [`WebRtcTransport::accept_offer`] →
//! [`WebRtcTransport::accept_answer`]。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use secrelay_protocol::Frame;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch, Mutex, Notify};
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit, RTCDataChannelState};
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCConfigurationBuilder, RTCIceCandidateType, RTCIceGatheringState, RTCIceServer,
    RTCPeerConnectionState, RTCSessionDescription, RTCStatsReport, RTCStatsReportEntry, Registry,
    StatsSelector, register_default_interceptors,
};
use webrtc::runtime::default_runtime;

use crate::ice::IceConfig;
use crate::{StatsSnapshot, Transport, TransportError, TransportStats};

/// DataChannel 单条消息的上限。
///
/// SCTP 的 `max-message-size` 取 RFC 8841 的默认值 64 KiB，也就是 webrtc 在 SDP 里
/// 协商出来的上限。编码后超过这个大小 `send` 会明确报错，不做截断。
/// 大对象要由上层切块，不要指望单帧扛完。
pub const MAX_DATACHANNEL_MESSAGE: usize = 64 * 1024;

/// DataChannel 标签，两端必须一致。
pub const DATA_CHANNEL_LABEL: &str = "secrelay";

/// 候选收集与建连的默认超时。
pub const DEFAULT_GATHER_TIMEOUT: Duration = Duration::from_secs(15);
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// DataChannel 发送缓冲上限：超过就让 `send` 等待，而不是无限吃内存。
const SEND_BUFFER_LIMIT: usize = 1024 * 1024;

/// 端到端的读写超时。
const IO_TIMEOUT: Duration = Duration::from_secs(20);

/// 关闭时留给 DCEP CLOSE 上线的时间。
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// 数据通道关闭完成后再等一小会儿才拆 DTLS。
const CLOSE_FLUSH: Duration = Duration::from_millis(300);

/// 接收队列长度。满了会让接收任务等待，从而对 SCTP 形成背压。
const INBOX_CAPACITY: usize = 64;

/// 探测一台 ICE 服务器的应答超时。只用来判断"这台还有没有人应答"，
/// 因此取小值：不响应的服务器在这一步就被剔掉，不参与后面的候选收集。
///
/// 探测是并发做的，所以建连最多多等这一个时长。
pub const ICE_PROBE_TIMEOUT: Duration = Duration::from_millis(800);

/// 等候选收集的实际预算。
///
/// 比 [`DEFAULT_GATHER_TIMEOUT`] 短：够等回 STUN 的 srflx，又不至于让一台
/// 事先探测不出来的死服务器（TURN）把每次建连都拖满 15 秒。
/// 到点还没收集完时，用已经进 SDP 的候选继续，不再失败。
const GATHER_BUDGET: Duration = Duration::from_secs(3);

/// 网卡列表缓存有效期。枚举要起子进程，不能每次建连都做一遍。
pub const IFACE_CACHE_TTL: Duration = Duration::from_secs(30);

/// 本机网卡列表的一次快照。
struct IfaceSnapshot {
    taken: Instant,
    addrs: Vec<IpAddr>,
}

static IFACE_CACHE: OnceLock<std::sync::Mutex<Option<IfaceSnapshot>>> = OnceLock::new();

/// 按优先级排好的本机地址，`usable` 为假表示应当退回原来的绑定。
struct PreparedBind {
    addrs: Vec<SocketAddr>,
    usable: bool,
}

/// STUN 的 Binding 请求与成功响应，以及报文头里的 magic cookie。
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const MAGIC_COOKIE: u32 = 0x2112_A442;

/// 选中候选对的快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidatePair {
    /// 本端地址。
    pub local: String,
    /// 对端地址。
    pub remote: String,
    /// 本端候选类型：`host` / `srflx` / `prflx` / `relay`。
    pub local_type: &'static str,
    /// 对端候选类型。
    pub remote_type: &'static str,
}

impl CandidatePair {
    /// 任一端是中继候选。
    pub fn is_relayed(&self) -> bool {
        self.local_type == "relay" || self.remote_type == "relay"
    }
}

/// 一条真实的 WebRTC 连接。
///
/// 建连顺序（两侧都必须先 `new`，事件回调才接得上）：
///
/// ```text
/// 发起方: new → create_offer ──offer──▶ 应答方: new → accept_offer → answer
/// 发起方: accept_answer ◀──answer──┘
/// ```
pub struct WebRtcTransport {
    peer: Arc<dyn PeerConnection>,
    handler: Arc<Handler>,
    stats: TransportStats,
    /// 本端调用过 `close`。
    local_closed: AtomicBool,
    /// 最近一次探测到的候选对，由 `is_relayed`（同步）读取。
    pair: std::sync::Mutex<Option<CandidatePair>>,
    /// 数据通道就绪状态，由接收任务写入。
    ready: watch::Receiver<bool>,
    /// 收到的帧；接收任务写、`recv` 读。
    inbox: Mutex<mpsc::Receiver<Frame>>,
    /// 接收任务是否已启动。
    pump_started: AtomicBool,
}

impl std::fmt::Debug for WebRtcTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebRtcTransport")
            .field("pair", &self.pair.lock().map(|p| p.clone()).ok())
            .field("local_closed", &self.local_closed.load(Ordering::Relaxed))
            .field("peer_closed", &self.handler.closed.load(Ordering::Relaxed))
            .field("stats", &self.stats.snapshot())
            .finish()
    }
}

impl WebRtcTransport {
    /// 建一条尚未协商的连接，绑所有网卡的随机端口。
    pub async fn new(config: &IceConfig) -> Result<Self, TransportError> {
        Self::new_with_bind(config, crate::ice::default_udp_addrs()).await
    }

    /// 同上，但指定本机 UDP 绑定地址（测试里用 `127.0.0.1:0` 把流量关在本机）。
    ///
    /// 传通配地址时会先枚举网卡，只绑可用的那几个：通配地址会让每种网卡都产生 host 候选，
    /// 虚拟网卡（VMware、Hyper-V、蓝牙 PAN 等）的候选永远打不通，却和有效候选同优先级，
    /// 只会拖长协商。
    pub async fn new_with_bind(
        config: &IceConfig,
        bind: Vec<String>,
    ) -> Result<Self, TransportError> {
        let bind = prepare_bind_addrs(&bind);
        // 一次探测所有 ICE 服务器，不响应的提前摘掉。
        let config = &filter_responsive_ice_servers(config).await;
        let handler = Arc::new(Handler::new());
        let peer = build_peer_connection(config, bind, handler.clone()).await?;
        // 数据通道要在生成 offer 之前就建好，否则 SDP 里不会有 m=application 段。
        let channel = peer
            .create_data_channel(
                DATA_CHANNEL_LABEL,
                Some(RTCDataChannelInit {
                    ordered: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(webrtc_error)?;
        handler.install(channel).await;

        let (frame_tx, frame_rx) = mpsc::channel(INBOX_CAPACITY);
        let (ready_tx, ready_rx) = watch::channel(false);
        let transport = Self {
            peer,
            handler,
            stats: TransportStats::default(),
            local_closed: AtomicBool::new(false),
            pair: std::sync::Mutex::new(None),
            ready: ready_rx,
            inbox: Mutex::new(frame_rx),
            pump_started: AtomicBool::new(false),
        };
        transport.start_pump(frame_tx, ready_tx).await;
        Ok(transport)
    }

    /// 起接收任务：它是 DataChannel 事件流唯一的消费者。
    ///
    /// 事件回调不能等回驱动的操作，所以 poll 与就绪判断都放在这里，不放在回调里。
    async fn start_pump(&self, frames: mpsc::Sender<Frame>, ready: watch::Sender<bool>) {
        if self.pump_started.swap(true, Ordering::Relaxed) {
            return;
        }
        let channel = self.handler.channel.lock().await.clone();
        let Some(channel) = channel else {
            self.pump_started.store(false, Ordering::Relaxed);
            return;
        };
        let closed = self.handler.closed.clone();
        let notify = self.handler.notify.clone();
        tokio::spawn(async move {
            loop {
                match channel.poll().await {
                    Some(DataChannelEvent::OnOpen) => {
                        let _ = ready.send(true);
                    }
                    Some(DataChannelEvent::OnMessage(message)) => {
                        match Frame::decode(&message.data) {
                            Ok(frame) => {
                                if frames.send(frame).await.is_err() {
                                    break;
                                }
                            }
                            Err(err) => {
                                tracing::warn!("收到无法解码的帧，已丢弃：{err}");
                            }
                        }
                    }
                    Some(DataChannelEvent::OnClose | DataChannelEvent::OnClosing) | None => break,
                    Some(DataChannelEvent::OnError) => break,
                    Some(_) => continue,
                }
            }
            let _ = ready.send(false);
            closed.store(true, Ordering::Relaxed);
            notify.notify_waiters();
        });
    }

    /// 发起方第一步：生成 offer（已含收集到的候选，因此不需要 trickle）。
    pub async fn create_offer(&self) -> Result<String, TransportError> {
        let offer = self.peer.create_offer(None).await.map_err(webrtc_error)?;
        self.peer
            .set_local_description(offer)
            .await
            .map_err(webrtc_error)?;
        Ok(prefer_ipv6_candidates(self.wait_for_gathering().await?))
    }

    /// 发起方第二步：接受 answer。数据通道在建连时已经建好了。
    pub async fn accept_answer(&self, sdp: String) -> Result<(), TransportError> {
        let answer = RTCSessionDescription::answer(sdp)
            .map_err(|e| TransportError::WebRtc(format!("answer 解析失败：{e}")))?;
        self.peer
            .set_remote_description(answer)
            .await
            .map_err(webrtc_error)
    }

    /// 应答方：接受 offer，返回 answer（已含收集到的候选）。
    pub async fn accept_offer(&self, sdp: String) -> Result<String, TransportError> {
        let offer = RTCSessionDescription::offer(sdp)
            .map_err(|e| TransportError::WebRtc(format!("offer 解析失败：{e}")))?;
        self.peer
            .set_remote_description(offer)
            .await
            .map_err(webrtc_error)?;
        let answer = self.peer.create_answer(None).await.map_err(webrtc_error)?;
        self.peer
            .set_local_description(answer)
            .await
            .map_err(webrtc_error)?;
        Ok(prefer_ipv6_candidates(self.wait_for_gathering().await?))
    }

    /// 等数据通道真正打开，并顺带探测一次候选对。
    pub async fn wait_connected(&self, timeout: Duration) -> Result<(), TransportError> {
        let deadline = Instant::now() + timeout;
        let mut ready = self.ready.clone();
        loop {
            if self.local_closed.load(Ordering::Relaxed) {
                return Err(TransportError::Closed);
            }
            if *ready.borrow() {
                self.refresh_pair().await;
                return Ok(());
            }
            if self.handler.closed.load(Ordering::Relaxed) {
                return Err(TransportError::Closed);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(TransportError::WebRtc(format!(
                    "等待数据通道打开超时（{timeout:?}）"
                )));
            }
            let _ = tokio::time::timeout(left, ready.changed()).await;
        }
    }

    /// 最近一次探测到的选中候选对。
    pub fn selected_pair(&self) -> Option<CandidatePair> {
        self.pair.lock().ok().and_then(|p| p.clone())
    }

    /// 重新探测选中的候选对，同时刷新 [`Transport::is_relayed`] 的结果。
    pub async fn refresh_pair(&self) -> Option<CandidatePair> {
        let report = self
            .peer
            .get_stats(Instant::now(), StatsSelector::None)
            .await;
        let pair = selected_pair_from_stats(&report);
        if let Ok(mut slot) = self.pair.lock() {
            *slot = pair.clone();
        }
        pair
    }

    async fn wait_for_gathering(&self) -> Result<String, TransportError> {
        let budget = GATHER_BUDGET;
        let deadline = Instant::now() + budget;
        loop {
            // 状态由事件回调写入：候选收集一完成就退出，不用轮询整份统计。
            if self.handler.gather_state() == GatherState::Complete {
                break;
            }
            if Instant::now() >= deadline {
                // 候选已经进到本地 SDP 里，继续等只会拖慢协商：一台不响应的服务器
                // （尤其是没法提前探测的 TURN）不该让整轮收集失败。
                let sdp = match self.peer.local_description().await {
                    Some(desc) if sdp_has_candidate(&desc.sdp) => desc.sdp,
                    _ => {
                        return Err(TransportError::WebRtc(format!(
                            "ICE 候选收集超时（{budget:?}）"
                        )));
                    }
                };
                tracing::warn!("ICE 候选收集未在 {budget:?} 内完成，先用已收到的候选继续");
                return Ok(sdp);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.peer
            .local_description()
            .await
            .map(|desc| desc.sdp)
            .ok_or_else(|| TransportError::WebRtc("本地 SDP 不存在".into()))
    }
}

#[async_trait::async_trait]
impl Transport for WebRtcTransport {
    async fn send(&self, frame: Frame) -> Result<(), TransportError> {
        if self.local_closed.load(Ordering::Relaxed) {
            return Err(TransportError::Closed);
        }

        // 先编码一次：字节数与真实上线的一致，非法的频道/载荷组合也在这里被拒。
        let encoded = frame.encode()?;
        if encoded.len() > MAX_DATACHANNEL_MESSAGE {
            return Err(TransportError::WebRtc(format!(
                "帧编码后 {} 字节，超过 DataChannel 单条消息上限 {} 字节",
                encoded.len(),
                MAX_DATACHANNEL_MESSAGE
            )));
        }

        let channel = self.handler.channel.lock().await.clone();
        let Some(channel) = channel else {
            return Err(TransportError::WebRtc("数据通道尚未建立".into()));
        };

        let len = encoded.len() as u64;
        send_with_timeout(&channel, encoded).await?;

        self.stats.frames_sent.fetch_add(1, Ordering::Relaxed);
        self.stats.bytes_sent.fetch_add(len, Ordering::Relaxed);
        Ok(())
    }

    async fn recv(&self) -> Result<Option<Frame>, TransportError> {
        let mut inbox = self.inbox.lock().await;
        match tokio::time::timeout(IO_TIMEOUT, inbox.recv()).await {
            Err(_) => Err(TransportError::WebRtc(format!(
                "接收超时（{IO_TIMEOUT:?}）"
            ))),
            Ok(Some(frame)) => {
                self.stats.frames_received.fetch_add(1, Ordering::Relaxed);
                let encoded_len = frame.encode().map(|b| b.len() as u64).unwrap_or(0);
                self.stats
                    .bytes_received
                    .fetch_add(encoded_len, Ordering::Relaxed);
                Ok(Some(frame))
            }
            // 队列关闭：接收任务已经退出，也就是对端关了或数据通道断了。
            Ok(None) => Ok(None),
        }
    }

    async fn close(&self) -> Result<(), TransportError> {
        if self.local_closed.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        let channel = self.handler.channel.lock().await.clone();
        if let Some(channel) = channel {
            let _ = channel.close().await;
            // DTLS 一拆，DCEP 的 CLOSE 就再也发不出去，对端只能等超时。
            // 先等本端数据通道走完关闭流程，再多给一点时间让 CLOSE 上线。
            let deadline = Instant::now() + CLOSE_GRACE;
            while Instant::now() < deadline
                && channel.ready_state().await != Ok(RTCDataChannelState::Closed)
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            tokio::time::sleep(CLOSE_FLUSH).await;
        }
        let _ = self.peer.close().await;
        self.handler.closed.store(true, Ordering::Relaxed);
        self.handler.notify.notify_waiters();
        tracing::debug!("WebRTC 传输已关闭");
        Ok(())
    }

    fn stats(&self) -> StatsSnapshot {
        self.stats.snapshot()
    }

    fn is_relayed(&self) -> bool {
        self.selected_pair().map(|p| p.is_relayed()).unwrap_or(false)
    }
}

impl Drop for WebRtcTransport {
    fn drop(&mut self) {
        // 唤醒可能还挂在 wait_connected 里的等待者，其余交给 close()。
        self.handler.notify.notify_waiters();
    }
}

/// 候选收集状态，由 ICE 事件回调写入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatherState {
    New,
    Gathering,
    Complete,
}

impl GatherState {
    fn as_u8(self) -> u8 {
        match self {
            GatherState::New => 0,
            GatherState::Gathering => 1,
            GatherState::Complete => 2,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => GatherState::Gathering,
            2 => GatherState::Complete,
            _ => GatherState::New,
        }
    }
}

/// 事件回调：接住对端创建的 DataChannel，并记录候选收集进度。
///
/// 回调由驱动线程调用，所以这里绝不等待会回到驱动的操作（如 `poll`、`ready_state`），
/// 只用锁交换指针和写原子量。
struct Handler {
    /// 当前使用的数据通道；建连时先放本端创建的那条。
    channel: Mutex<Option<Arc<dyn DataChannel>>>,
    notify: Arc<Notify>,
    gather_state: AtomicU8,
    /// 连接不可用（对端关闭或建连失败）。
    closed: Arc<AtomicBool>,
}

impl Handler {
    fn new() -> Self {
        Self {
            channel: Mutex::new(None),
            notify: Arc::new(Notify::new()),
            gather_state: AtomicU8::new(GatherState::New.as_u8()),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 安装数据通道。对端创建的那条才是双方共用的那条，因此总是覆盖。
    async fn install(&self, channel: Arc<dyn DataChannel>) {
        *self.channel.lock().await = Some(channel);
        self.notify.notify_one();
    }

    fn gather_state(&self) -> GatherState {
        GatherState::from_u8(self.gather_state.load(Ordering::Relaxed))
    }
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_data_channel(&self, data_channel: Arc<dyn DataChannel>) {
        self.install(data_channel).await;
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        let state = match state {
            RTCIceGatheringState::Gathering => GatherState::Gathering,
            RTCIceGatheringState::Complete => GatherState::Complete,
            _ => GatherState::New,
        };
        self.gather_state.store(state.as_u8(), Ordering::Relaxed);
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if matches!(
            state,
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
        ) {
            self.closed.store(true, Ordering::Relaxed);
            self.notify.notify_waiters();
        }
    }
}

async fn build_peer_connection(
    config: &IceConfig,
    bind: PreparedBind,
    handler: Arc<Handler>,
) -> Result<Arc<dyn PeerConnection>, TransportError> {
    if !bind.usable {
        return build_peer_connection_with(config, bind.addrs, handler).await;
    }
    match build_peer_connection_with(config, bind.addrs.clone(), handler.clone()).await {
        Ok(peer) => Ok(peer),
        Err(err) => {
            // 枚举出来的地址可能在绑定前就消失了（网卡切换）。退回通配地址，
            // 让库自己去列接口，总比连接建不起来强。
            tracing::warn!("按网卡地址绑定失败，退回通配地址：{err}");
            build_peer_connection_with(config, vec![wildcard_bind(&bind.addrs)], handler).await
        }
    }
}

async fn build_peer_connection_with(
    config: &IceConfig,
    bind: Vec<SocketAddr>,
    handler: Arc<Handler>,
) -> Result<Arc<dyn PeerConnection>, TransportError> {
    let runtime = default_runtime().ok_or_else(|| {
        TransportError::WebRtc("webrtc 运行时未启用，需要 runtime-tokio 特性".into())
    })?;

    let mut media = MediaEngine::default();
    media
        .register_default_codecs()
        .map_err(|e| TransportError::WebRtc(format!("注册默认编解码器失败：{e}")))?;
    let registry = register_default_interceptors(Registry::new(), &mut media)
        .map_err(|e| TransportError::WebRtc(format!("注册默认拦截器失败：{e}")))?;

    let servers: Vec<RTCIceServer> = config.to_ice_servers();
    let rtc_config = RTCConfigurationBuilder::new()
        .with_ice_servers(servers)
        .with_ice_transport_policy(config.transport_policy())
        .build();

    let owner = Arc::clone(&handler);
    let inner: Arc<dyn PeerConnectionEventHandler> = handler;
    let builder = PeerConnectionBuilder::new()
        .with_configuration(rtc_config)
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .with_handler(inner)
        .with_runtime(runtime)
        .with_udp_addrs(bind)
        .with_data_channel_send_buffer_limit(SEND_BUFFER_LIMIT);

    let peer = builder
        .build()
        .await
        .map(|pc| Arc::new(pc) as Arc<dyn PeerConnection>)
        .map_err(webrtc_error)?;

    // 回调是按 Arc 存的强引用，拆掉这层才不会让 Handler 一直挂在 PeerConnection 上。
    let _ = Arc::try_unwrap(owner);
    Ok(peer)
}

/// 从已绑定的地址里推一个同族的通配地址。
fn wildcard_bind(addrs: &[SocketAddr]) -> SocketAddr {
    if addrs.iter().any(|addr| addr.is_ipv6()) {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
    }
}

/// SDP 里有没有候选行。
fn sdp_has_candidate(sdp: &str) -> bool {
    sdp.lines().any(|line| line.starts_with("a=candidate:"))
}

/// 通配地址换成枚举出来的具体网卡；已经写死的地址一律照原样用。
///
/// 库对通配地址的处理是"每块网卡一个 socket"，于是虚拟网卡的 host 候选也会进 SDP。
/// 那些候选和真实网卡的候选同优先级，却永远打不通，ICE 要挨个试过去。
fn prepare_bind_addrs(bind: &[String]) -> PreparedBind {
    let parsed: Vec<SocketAddr> = bind.iter().filter_map(|raw| raw.parse().ok()).collect();
    // 有写死的地址就说明调用方自己选好了网卡，不动它。
    let has_explicit = parsed.iter().any(|addr| !addr.ip().is_unspecified());
    if has_explicit || parsed.len() != bind.len() {
        return PreparedBind {
            addrs: parsed,
            usable: false,
        };
    }

    let addrs = iface_addrs().unwrap_or_default();
    if addrs.is_empty() {
        // 全被过滤掉时不能变成零候选：退回通配地址，让库自己列接口。
        return PreparedBind {
            addrs: parsed,
            usable: false,
        };
    }

    let port = parsed.first().map(|addr| addr.port()).unwrap_or(0);
    let expanded = addrs
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect();
    PreparedBind {
        addrs: expanded,
        usable: true,
    }
}

/// 枚举网卡并挑出可用的地址（带缓存）。
fn iface_addrs() -> Option<Vec<IpAddr>> {
    let cache = IFACE_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    {
        let guard = cache.lock().ok()?;
        if let Some(snapshot) = guard.as_ref() {
            if snapshot.taken.elapsed() < IFACE_CACHE_TTL {
                return Some(snapshot.addrs.clone());
            }
        }
    }

    let text = enumerate_iface_table().ok()?;
    let addrs = filter_iface_addrs(parse_iface_table(&ipconfig_to_table(&text)));
    let mut guard = cache.lock().ok()?;
    *guard = Some(IfaceSnapshot {
        taken: Instant::now(),
        addrs: addrs.clone(),
    });
    Some(addrs)
}

/// 一块网卡上的一个地址。
#[derive(Debug, Clone, PartialEq, Eq)]
struct IfaceEntry {
    name: String,
    description: String,
    addr: IpAddr,
    is_virtual: bool,
    is_up: bool,
    has_default_route: bool,
}

/// 挑出能当 host 候选的地址：IPv6 在前。
///
/// 分级过滤，任何一级有结果就停：先用"在线的物理网卡 + 有默认路由"，
/// 再退到"在线的物理网卡"，最后才考虑虚拟网卡 —— 全被过滤掉时不能变成零候选。
fn filter_iface_addrs(entries: Vec<IfaceEntry>) -> Vec<IpAddr> {
    let usable = |entry: &IfaceEntry| -> bool {
        entry.is_up
            && !is_link_local(&entry.addr)
            && entry.addr != IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            && !entry.addr.is_loopback()
            && entry.addr != IpAddr::V6(Ipv6Addr::UNSPECIFIED)
    };

    let mut tiers: [Vec<IpAddr>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for entry in &entries {
        if !usable(entry) {
            continue;
        }
        let tier = if is_virtual_iface(&entry.name, &entry.description, entry.is_virtual) {
            2
        } else if entry.has_default_route {
            0
        } else {
            1
        };
        tiers[tier].push(entry.addr);
    }

    let picked = tiers
        .iter()
        .find(|tier| !tier.is_empty())
        .cloned()
        .unwrap_or_default();
    sort_ipv6_first(picked)
}

/// 虚拟网卡的名字特征。这类网卡的地址永远打不通。
fn is_virtual_iface(name: &str, description: &str, flagged: bool) -> bool {
    if flagged {
        return true;
    }
    const HINTS: [&str; 15] = [
        "vmware",
        "virtualbox",
        "vbox",
        "hyper-v",
        "vethernet",
        "docker",
        "wsl",
        "bluetooth",
        "wi-fi direct",
        "tap-windows",
        "wintun",
        "wireguard",
        "teredo",
        "isatap",
        "virtual",
    ];
    let haystack = format!("{name} {description}").to_ascii_lowercase();
    HINTS.iter().any(|hint| haystack.contains(hint))
}

fn is_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// IPv6 在前，同族保持原顺序。
fn sort_ipv6_first(addrs: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut sorted = addrs;
    sorted.sort_by_key(|ip| !ip.is_ipv6());
    sorted
}

/// 起一次 `ipconfig /all` 把网卡表读出来。
///
/// 不用 PowerShell 的 NetTCPIP cmdlet：那套在一台装了虚拟网卡的机器上单次要 5 秒以上，
/// `ipconfig` 是同一个数据源的百毫秒级入口。
/// 输出是本地化的，所以标签一律按"去掉点号后是不是 Description / Default Gateway"
/// 这类结构特征识别，不匹配任何一种翻译。
#[cfg(windows)]
fn enumerate_iface_table() -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    // CREATE_NO_WINDOW：不要在用户桌面上闪一个控制台窗口。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // 适配器名可能带非 ASCII 字符，让输出按 UTF-8 来。
    const COMMAND: &str = "chcp 65001 >nul & ipconfig /all";

    let output = Command::new("cmd.exe")
        .args(["/C", COMMAND])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("起 ipconfig 失败：{e}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(not(windows))]
fn enumerate_iface_table() -> Result<String, String> {
    Err("非 Windows 平台不枚举网卡".to_string())
}

/// 把 `ipconfig /all` 的输出转成 [`parse_iface_table`] 认的行。
///
/// 每个地址给出一行：`名字|描述|地址|是否虚拟|是否在线|是否有默认网关`。
/// `ipconfig` 不报虚拟标记，因此这里固定给 0，交给名字判定。
fn ipconfig_to_table(text: &str) -> String {
    struct Block {
        kind: String,
        description: String,
        ipv4: Vec<String>,
        ipv6: Vec<String>,
        gateway_v4: bool,
        gateway_v6: bool,
    }
    let mut block = Block {
        kind: String::new(),
        description: String::new(),
        ipv4: Vec::new(),
        ipv6: Vec::new(),
        gateway_v4: false,
        gateway_v6: false,
    };
    let mut rows = Vec::new();

    let flush = |block: &mut Block, rows: &mut Vec<String>| {
        for (addrs, has_gateway) in [
            (&block.ipv4, block.gateway_v4),
            (&block.ipv6, block.gateway_v6),
        ] {
            for addr in addrs {
                rows.push(format!(
                    "{}|{}|{}|0|up|{}",
                    block.kind,
                    block.description,
                    addr,
                    if has_gateway { 1 } else { 0 }
                ));
            }
        }
        block.ipv4.clear();
        block.ipv6.clear();
        block.gateway_v4 = false;
        block.gateway_v6 = false;
    };

    for line in text.lines() {
        let trimmed = line.trim_end();
        // 适配器块头：不缩进、以冒号结尾。
        if !trimmed.starts_with(' ') && trimmed.ends_with(':') {
            flush(&mut block, &mut rows);
            block.kind = trimmed.trim_end_matches(':').to_string();
            block.description.clear();
            continue;
        }
        let Some((label, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key: String = label
            .trim_matches(|c: char| c == '.' || c.is_whitespace())
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect();
        let value = value.split('(').next().unwrap_or(value).trim();
        if key.eq_ignore_ascii_case("description") {
            block.description = value.to_string();
        } else if key.eq_ignore_ascii_case("ipv4address") && !value.is_empty() {
            block.ipv4.push(value.to_string());
        } else if key.eq_ignore_ascii_case("ipv6address") && !value.is_empty() {
            block.ipv6.push(value.to_string());
        } else if key.eq_ignore_ascii_case("defaultgateway") {
            if value.parse::<Ipv4Addr>().is_ok() {
                block.gateway_v4 = true;
            } else if value
                .split('%')
                .next()
                .unwrap_or(value)
                .parse::<Ipv6Addr>()
                .is_ok()
            {
                block.gateway_v6 = true;
            }
        }
    }
    flush(&mut block, &mut rows);
    rows.join("\n")
}

/// 解析 `名字|描述|地址|虚拟|状态|有默认路由` 的行。
///
/// 解析不出来的行直接跳过：拿不到网卡信息时退回原来的绑定，不影响连接。
fn parse_iface_table(text: &str) -> Vec<IfaceEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split('|').map(|field| field.trim()).collect();
        if fields.len() != 6 {
            continue;
        }
        let Ok(addr) = fields[2].parse::<IpAddr>() else {
            continue;
        };
        entries.push(IfaceEntry {
            name: fields[0].to_string(),
            description: fields[1].to_string(),
            addr,
            is_virtual: fields[3] == "1",
            is_up: fields[4].eq_ignore_ascii_case("up"),
            has_default_route: fields[5] == "1",
        });
    }
    entries
}

/// 把候选行重排成 IPv6 在前。
///
/// 候选的本地优先级由 webrtc 内部算好，公开 API 改不了，能调的就是对端看到的顺序：
/// 对端按这个顺序建 checklist。IPv6 下没有 NAT，能拿到就该先试。
/// 只重排，不丢候选 —— 对方或本机没有 IPv6 时行为不变。
fn prefer_ipv6_candidates(sdp: String) -> String {
    if !sdp.contains("a=candidate:") {
        return sdp;
    }

    let mut lines: Vec<String> = sdp.lines().map(|line| line.to_string()).collect();
    let mut slots: Vec<usize> = Vec::new();
    let mut groups: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with("a=candidate:") {
            slots.push(index);
            groups[candidate_group(line)].push(line.clone());
        }
    }

    // 只换候选行，非候选行原地不动。
    let mut ordered = groups.into_iter().flatten();
    for slot in slots {
        if let Some(candidate) = ordered.next() {
            lines[slot] = candidate;
        }
    }
    lines.join("\n") + "\n"
}

/// 候选行的分组下标：0 = IPv6，1 = 解析不出来的，2 = IPv4。
fn candidate_group(line: &str) -> usize {
    match candidate_ip(line) {
        Some(ip) if ip.is_ipv6() => 0,
        Some(_) => 2,
        None => 1,
    }
}

/// 取候选行的地址（`candidate:… 优先级 地址 端口 typ …`）。
fn candidate_ip(line: &str) -> Option<IpAddr> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let at = fields
        .iter()
        .position(|field| *field == "udp" || *field == "tcp")?;
    fields.get(at + 2)?.parse().ok()
}

/// 一次探测所有 ICE 服务器，把不响应的从配置里摘掉。
///
/// 库是把所有服务器一起等的：一台不响应，整轮收集就要等到超时。
/// 这里给每台单独计时、单独判成败，能用的先用。
/// 拿不到凭据就没法探测 TURN，因此带 TURN 的条目一律保留（它不响应时由收集预算兜底）。
async fn filter_responsive_ice_servers(config: &IceConfig) -> IceConfig {
    let mut kept = Vec::with_capacity(config.ice.len());
    for server in &config.ice {
        let probed: Vec<&String> = server
            .urls
            .iter()
            .filter(|url| url.trim_start().starts_with("stun:"))
            .collect();
        if probed.is_empty() {
            kept.push(server.clone());
            continue;
        }

        let (responded, unanswered) = probe_stun_urls(&probed).await;
        if responded {
            kept.push(server.clone());
            continue;
        }
        tracing::warn!("ICE 服务器 {unanswered:?} 无应答，已跳过");
        if unanswered.len() < server.urls.len() {
            // 同一条目里还混着 TURN：只摘掉没应答的那几条 stun URL。
            let mut reduced = server.clone();
            reduced.urls = unanswered;
            kept.push(reduced);
        }
    }

    IceConfig {
        ice: kept,
        stun_only: config.stun_only,
    }
}

/// 并发探测一组 stun URL，返回（是否有应答，没应答的 URL）。
async fn probe_stun_urls(urls: &[&String]) -> (bool, Vec<String>) {
    let mut tasks = Vec::with_capacity(urls.len());
    for url in urls {
        let url = (*url).clone();
        tasks.push(tokio::spawn(async move {
            let ok = probe_stun_url(&url).await;
            (url, ok)
        }));
    }

    let mut responded = false;
    let mut unanswered = Vec::new();
    for task in tasks {
        match task.await {
            Ok((url, true)) => {
                responded = true;
                tracing::debug!("ICE 服务器 {url} 有应答");
            }
            Ok((url, false)) => unanswered.push(url),
            Err(err) => tracing::debug!("ICE 探测任务失败：{err}"),
        }
    }
    (responded, unanswered)
}

/// 对一台 STUN 服务器发一次 Binding 请求，只看它在超时内回不回。
async fn probe_stun_url(url: &str) -> bool {
    let Ok(peer) = parse_stun_addr(url) else {
        return false;
    };

    let bind: SocketAddr = if peer.is_ipv6() {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
    };
    let Ok(socket) = UdpSocket::bind(bind).await else {
        return false;
    };
    if socket.connect(peer).await.is_err() {
        return false;
    }
    if socket.send(&binding_request()).await.is_err() {
        return false;
    }

    let mut buffer = [0u8; 1500];
    match tokio::time::timeout(ICE_PROBE_TIMEOUT, socket.recv(&mut buffer)).await {
        Ok(Ok(len)) => binding_success(&buffer[..len]),
        _ => false,
    }
}

/// `stun:host:port` → 地址；缺端口补 3478。
fn parse_stun_addr(url: &str) -> Result<SocketAddr, String> {
    let rest = url
        .trim()
        .strip_prefix("stun:")
        .ok_or_else(|| format!("不是 stun URL：{url}"))?;
    let rest = rest.split('?').next().unwrap_or(rest);
    let rest = rest.split('/').next().unwrap_or(rest);
    if let Ok(addr) = rest.parse::<SocketAddr>() {
        return Ok(addr);
    }
    // 主机名要查解析：交给标准库，失败就当这台不响应。
    use std::net::ToSocketAddrs;
    let with_port = if rest.contains("]:") || rest.matches(':').count() == 1 {
        rest.to_string()
    } else {
        format!("{rest}:3478")
    };
    with_port
        .to_socket_addrs()
        .map_err(|e| format!("解析 {rest} 失败：{e}"))?
        .next()
        .ok_or_else(|| format!("{rest} 没有解析结果"))
}

/// 构造一个 Binding 请求。
fn binding_request() -> [u8; 20] {
    let mut request = [0u8; 20];
    request[..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    request[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
    request[8..20].copy_from_slice(&transaction_id());
    request
}

/// 判断一个报文是不是成功的 Binding 响应。
fn binding_success(message: &[u8]) -> bool {
    if message.len() < 20 {
        return false;
    }
    let kind = u16::from_be_bytes([message[0], message[1]]);
    let cookie = u32::from_be_bytes([message[4], message[5], message[6], message[7]]);
    kind == BINDING_SUCCESS && cookie == MAGIC_COOKIE
}

/// 事务 ID：纳秒时间 + 一个计数器，不需要密码学随机。
fn transaction_id() -> [u8; 12] {
    static SEQUENCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut id = [0u8; 12];
    id[..8].copy_from_slice(&nanos.to_be_bytes());
    id[8..].copy_from_slice(&SEQUENCE.fetch_add(1, Ordering::Relaxed).to_be_bytes());
    id
}

async fn send_with_timeout(
    channel: &Arc<dyn DataChannel>,
    encoded: Vec<u8>,
) -> Result<(), TransportError> {
    match tokio::time::timeout(
        IO_TIMEOUT,
        channel.send(BytesMut::from(&encoded[..])),
    )
    .await
    {
        Err(_) => Err(TransportError::WebRtc(format!("发送超时（{IO_TIMEOUT:?}）"))),
        Ok(Err(webrtc::error::Error::ErrDataChannelClosed)) => Err(TransportError::Closed),
        Ok(Err(e)) => Err(webrtc_error(e)),
        Ok(Ok(())) => Ok(()),
    }
}

/// 从统计里挑出选中（nominated）的候选对。
///
/// 报告里可能只有本端候选、没有对端候选，因此对端地址允许缺失。
fn selected_pair_from_stats(report: &RTCStatsReport) -> Option<CandidatePair> {
    let pair = report.candidate_pairs().find(|p| p.nominated)?;
    let local = candidate_addr(find_candidate(report, &pair.local_candidate_id)?)?;
    let remote = find_candidate(report, &pair.remote_candidate_id).and_then(candidate_addr);
    Some(CandidatePair {
        local: format!("{}:{}", local.0, local.1),
        remote: match &remote {
            Some(remote) => format!("{}:{}", remote.0, remote.1),
            None => "未知".to_string(),
        },
        local_type: candidate_type_name(local.2),
        remote_type: match remote {
            Some(remote) => candidate_type_name(remote.2),
            None => "unknown",
        },
    })
}

/// 候选条目里的 ID 带类型前缀（`RTCLocalIceCandidate_…`），候选对引用的是去前缀的 ID。
fn strip_stats_prefix(id: &str) -> &str {
    const PREFIXES: [&str; 3] = [
        "RTCLocalIceCandidate_",
        "RTCRemoteIceCandidate_",
        "RTCIceCandidatePair_",
    ];
    for prefix in PREFIXES {
        if let Some(rest) = id.strip_prefix(prefix) {
            return rest;
        }
    }
    id
}

fn find_candidate<'a>(report: &'a RTCStatsReport, id: &str) -> Option<&'a RTCStatsReportEntry> {
    let wanted = strip_stats_prefix(id);
    report.iter().find(|entry| {
        let entry_id = match entry {
            RTCStatsReportEntry::LocalCandidate(c) => &c.stats.id,
            RTCStatsReportEntry::RemoteCandidate(c) => &c.stats.id,
            _ => return false,
        };
        strip_stats_prefix(entry_id) == wanted
    })
}

/// 候选的地址与端口。
fn candidate_addr(entry: &RTCStatsReportEntry) -> Option<(String, u16, RTCIceCandidateType)> {
    match entry {
        RTCStatsReportEntry::LocalCandidate(c) => Some((
            c.address.clone().unwrap_or_else(|| "?".into()),
            c.port,
            c.candidate_type,
        )),
        RTCStatsReportEntry::RemoteCandidate(c) => Some((
            c.address.clone().unwrap_or_else(|| "?".into()),
            c.port,
            c.candidate_type,
        )),
        _ => None,
    }
}

fn candidate_type_name(kind: RTCIceCandidateType) -> &'static str {
    match kind {
        RTCIceCandidateType::Host => "host",
        RTCIceCandidateType::Srflx => "srflx",
        RTCIceCandidateType::Prflx => "prflx",
        RTCIceCandidateType::Relay => "relay",
        RTCIceCandidateType::Unspecified => "unspecified",
        _ => "unknown",
    }
}

fn webrtc_error(error: webrtc::error::Error) -> TransportError {
    TransportError::WebRtc(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ice::IceServerConfig;
    use secrelay_protocol::Channel;

    /// 真跑一遍 ICE/DTLS/SCTP，只是用 127.0.0.1 把候选关在本机。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 进程内两端能建连并双向互通() {
        let config = IceConfig::host_only();
        let offerer = WebRtcTransport::new_with_bind(&config, vec!["127.0.0.1:0".to_string()])
            .await
            .unwrap();
        let answerer = WebRtcTransport::new_with_bind(&config, vec!["127.0.0.1:0".to_string()])
            .await
            .unwrap();

        // 手工交换 SDP：等价于把 offer 贴给对端、再把 answer 贴回来。
        let offer = offerer.create_offer().await.unwrap();
        assert!(offer.contains("m=application"), "offer 应当带数据通道：{offer}");
        let answer = answerer.accept_offer(offer).await.unwrap();
        assert!(answer.contains("m=application"), "answer 应当带数据通道");
        offerer.accept_answer(answer).await.unwrap();

        offerer
            .wait_connected(Duration::from_secs(20))
            .await
            .unwrap();
        answerer
            .wait_connected(Duration::from_secs(20))
            .await
            .unwrap();

        offerer
            .send(Frame::raw(Channel::Media, vec![1, 2, 3]).unwrap())
            .await
            .unwrap();
        let frame = answerer.recv().await.unwrap().expect("应答方应当收到帧");
        assert_eq!(frame.as_raw(), Some(&[1u8, 2, 3][..]));

        answerer
            .send(Frame::raw(Channel::File, vec![7u8; 1000]).unwrap())
            .await
            .unwrap();
        let frame = offerer.recv().await.unwrap().expect("发起方应当收到帧");
        assert_eq!(frame.channel, Channel::File);
        assert_eq!(frame.as_raw().unwrap().len(), 1000);

        assert_eq!(offerer.stats().frames_sent, 1);
        assert_eq!(offerer.stats().frames_received, 1);
        assert_eq!(answerer.stats().frames_received, 1);
        assert_eq!(answerer.stats().frames_sent, 1);

        let pair = offerer.selected_pair().expect("应当探测到候选对");
        assert_eq!(pair.local_type, "host", "回环应当是 host 候选：{pair:?}");
        assert!(!pair.is_relayed(), "host 候选不是中继");
        assert!(!offerer.is_relayed());
        assert!(!answerer.is_relayed());

        // 关闭幂等；关闭后不能再发。
        offerer.close().await.unwrap();
        offerer.close().await.unwrap();
        let err = offerer
            .send(Frame::raw(Channel::Media, vec![1]).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(err, TransportError::Closed));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 对端关闭后本方_recv_读到_none() {
        let config = IceConfig::host_only();
        let offerer = WebRtcTransport::new_with_bind(&config, vec!["127.0.0.1:0".to_string()])
            .await
            .unwrap();
        let answerer = WebRtcTransport::new_with_bind(&config, vec!["127.0.0.1:0".to_string()])
            .await
            .unwrap();

        let offer = offerer.create_offer().await.unwrap();
        let answer = answerer.accept_offer(offer).await.unwrap();
        offerer.accept_answer(answer).await.unwrap();
        offerer.wait_connected(Duration::from_secs(20)).await.unwrap();
        answerer.wait_connected(Duration::from_secs(20)).await.unwrap();

        answerer.close().await.unwrap();
        assert_eq!(offerer.recv().await.unwrap(), None);
    }

    #[test]
    fn 默认绑定地址可用() {
        for addr in crate::ice::default_udp_addrs() {
            assert!(crate::ice::parse_bind_addr(&addr).is_ok(), "{addr}");
        }
    }

    /// 超过 DataChannel 单条消息上限的帧在发送前就被拒，不需要建连。
    #[tokio::test]
    async fn 超长帧被拒() {
        let config = IceConfig::host_only();
        let transport =
            WebRtcTransport::new_with_bind(&config, vec!["127.0.0.1:0".to_string()])
                .await
                .unwrap();
        let big = vec![0u8; MAX_DATACHANNEL_MESSAGE];
        let err = transport
            .send(Frame::raw(Channel::File, big).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(err, TransportError::WebRtc(_)), "应当是传输层错误");
        assert_eq!(transport.stats().frames_sent, 0, "被拒的帧不计入统计");
    }

    fn stun_server(url: &str) -> IceServerConfig {
        IceServerConfig {
            urls: vec![url.to_string()],
            username: String::new(),
            credential: String::new(),
        }
    }

    /// 本机起一个只会回 Binding 成功响应的假 STUN 服务器，返回它的地址。
    ///
    /// 回包要带 XOR-MAPPED-ADDRESS —— 库那边正是靠它算出 srflx 候选。
    async fn fake_stun_server() -> SocketAddr {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buffer = [0u8; 1500];
            loop {
                let Ok((len, from)) = socket.recv_from(&mut buffer).await else {
                    return;
                };
                if len < 20 {
                    continue;
                }
                let tid = &buffer[8..20];
                let mut response = Vec::with_capacity(64);
                response.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
                response.extend_from_slice(&0u16.to_be_bytes());
                response.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
                response.extend_from_slice(tid);
                let port = from.port() ^ (MAGIC_COOKIE >> 16) as u16;
                let octets = match from.ip() {
                    IpAddr::V4(v4) => v4.octets().to_vec(),
                    IpAddr::V6(v6) => v6.octets().to_vec(),
                };
                let mut mask = MAGIC_COOKIE.to_be_bytes().to_vec();
                mask.extend_from_slice(tid);
                let family: u16 = if from.is_ipv6() { 2 } else { 1 };
                let mut value = Vec::with_capacity(20);
                value.extend_from_slice(&family.to_be_bytes());
                value.extend_from_slice(&port.to_be_bytes());
                for (index, byte) in octets.iter().enumerate() {
                    value.push(byte ^ mask[index % mask.len()]);
                }
                response.extend_from_slice(&0x0020u16.to_be_bytes());
                response.extend_from_slice(&(value.len() as u16).to_be_bytes());
                response.extend_from_slice(&value);
                let attr_len = (response.len() - 20) as u16;
                response[2..4].copy_from_slice(&attr_len.to_be_bytes());
                let _ = socket.send_to(&response, from).await;
            }
        });
        addr
    }

    /// 一个本机没人监听的端口：探测必然没有应答。
    async fn dead_addr() -> SocketAddr {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        drop(socket);
        addr
    }

    #[tokio::test]
    async fn 不响应的_ice_服务器被剔除而不影响其余() {
        let alive = fake_stun_server().await;
        let dead = dead_addr().await;
        let config = IceConfig::from_servers(vec![
            stun_server("stun:192.0.2.1:3478"),
            stun_server(&format!("stun:{alive}")),
            stun_server(&format!("stun:{dead}")),
        ]);

        let started = Instant::now();
        let filtered = filter_responsive_ice_servers(&config).await;
        assert!(
            started.elapsed() < ICE_PROBE_TIMEOUT * 2,
            "探测应当并发且各自计时，实际 {:?}",
            started.elapsed()
        );

        let urls: Vec<String> = filtered
            .ice
            .iter()
            .flat_map(|server| server.urls.iter().cloned())
            .collect();
        assert_eq!(urls, vec![format!("stun:{alive}")], "只应留下有应答的那台");
    }

    /// 两台 ICE 服务器只有一台应答时，候选照样收集到，并且能真的建连。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 只有一台_ice_服务器应答时仍能建连() {
        let alive = fake_stun_server().await;
        let dead = dead_addr().await;
        let config = IceConfig::from_servers(vec![
            stun_server(&format!("stun:{alive}")),
            stun_server(&format!("stun:{dead}")),
        ]);
        let config = filter_responsive_ice_servers(&config).await;
        assert_eq!(config.ice.len(), 1, "死的那台应当已被剔除");

        let bind = vec!["127.0.0.1:0".to_string()];
        let offerer = WebRtcTransport::new_with_bind(&config, bind.clone())
            .await
            .unwrap();
        let answerer = WebRtcTransport::new_with_bind(&config, bind).await.unwrap();

        let started = Instant::now();
        let offer = offerer.create_offer().await.unwrap();
        assert!(offer.contains("typ srflx"), "应当收集到 srflx 候选：{offer}");
        let answer = answerer.accept_offer(offer).await.unwrap();
        offerer.accept_answer(answer).await.unwrap();
        offerer
            .wait_connected(Duration::from_secs(20))
            .await
            .unwrap();
        answerer
            .wait_connected(Duration::from_secs(20))
            .await
            .unwrap();
        assert!(
            started.elapsed() < DEFAULT_GATHER_TIMEOUT,
            "不该等到默认收集超时，实际 {:?}",
            started.elapsed()
        );

        offerer
            .send(Frame::raw(Channel::Media, vec![9]).unwrap())
            .await
            .unwrap();
        let frame = answerer.recv().await.unwrap().expect("应当收到帧");
        assert_eq!(frame.as_raw(), Some(&[9u8][..]));
    }

    fn entry(name: &str, description: &str, addr: &str, has_default_route: bool) -> IfaceEntry {
        IfaceEntry {
            name: name.to_string(),
            description: description.to_string(),
            addr: addr.parse().unwrap(),
            is_virtual: false,
            is_up: true,
            has_default_route,
        }
    }

    /// 用本机 ipconfig 的真实形状做输入。
    const IPCCONFIG: &str = "\
Windows IP Configuration

   Host Name . . . . . . . . . . . . : LZY

Ethernet adapter 以太网:

   Connection-specific DNS Suffix  . : 
   Description . . . . . . . . . . . : Realtek PCIe GbE Family Controller
   Physical Address. . . . . . . . . : A0-AD-9F-9B-4B-34
   Link-local IPv6 Address . . . . . : fe80::7e4b:16ed:8ee9:9582%18(Preferred) 
   IPv4 Address. . . . . . . . . . . : 192.168.0.100(Preferred) 
   Subnet Mask . . . . . . . . . . . : 255.255.255.0
   Default Gateway . . . . . . . . . : 192.168.0.1

Ethernet adapter VMware Network Adapter VMnet1:

   Description . . . . . . . . . . . : VMware Virtual Ethernet Adapter for VMnet1
   IPv4 Address. . . . . . . . . . . : 192.168.207.1(Preferred) 
   Subnet Mask . . . . . . . . . . . : 255.255.255.0
   Default Gateway . . . . . . . . . : 

Wireless LAN adapter 本地连接* 1:

   Media State . . . . . . . . . . . : Media disconnected
   Description . . . . . . . . . . . : Microsoft Wi-Fi Direct Virtual Adapter
   Physical Address. . . . . . . . . : 9E-C7-D3-09-1D-DE

Bluetooth 网络连接 2:

   Description . . . . . . . . . . . : Bluetooth Device (Personal Area Network) #2
   Link-local IPv6 Address . . . . . : fe80::1%5(Preferred) 
   IPv4 Address. . . . . . . . . . . : 169.254.42.237(Preferred) 
   Default Gateway . . . . . . . . . : 
";

    #[test]
    fn 虚拟网卡与链路本地地址不出现在候选里() {
        let addrs = filter_iface_addrs(parse_iface_table(&ipconfig_to_table(IPCCONFIG)));
        assert_eq!(addrs, vec![IpAddr::V4(Ipv4Addr::new(192, 168, 0, 100))]);
        for addr in &addrs {
            let text = addr.to_string();
            assert!(!text.starts_with("169.254"), "链路本地不该留下：{text}");
        }
    }

    #[test]
    fn 不误杀有效接口() {
        let entries = vec![
            entry("以太网", "Realtek PCIe GbE Family Controller", "192.168.0.100", true),
            entry("VMware Network Adapter VMnet1", "VMware Virtual Ethernet Adapter for VMnet1", "192.168.207.1", false),
            entry("蓝牙网络连接 2", "Bluetooth Device (Personal Area Network) #2", "169.254.42.237", false),
            entry("WLAN", "Realtek 8852CE WiFi 6E PCI-E NIC", "192.168.0.102", true),
        ];
        assert_eq!(
            filter_iface_addrs(entries),
            vec![
                IpAddr::V4(Ipv4Addr::new(192, 168, 0, 100)),
                IpAddr::V4(Ipv4Addr::new(192, 168, 0, 102)),
            ]
        );
    }

    #[test]
    fn 全被过滤时仍有候选() {
        // 只剩虚拟网卡：不能让候选变成零，退而用它们。
        let only_virtual = vec![entry(
            "VMware Network Adapter VMnet8",
            "VMware Virtual Ethernet Adapter for VMnet8",
            "192.168.145.1",
            false,
        )];
        assert_eq!(
            filter_iface_addrs(only_virtual),
            vec![IpAddr::V4(Ipv4Addr::new(192, 168, 145, 1))]
        );

        // 路上什么都没有时也不能把绑定地址判成空。
        let prepared = prepare_bind_addrs(&["0.0.0.0:0".to_string()]);
        assert!(!prepared.addrs.is_empty());
    }

    #[test]
    fn ipv6_候选排在前面() {
        let entries = vec![
            entry("以太网", "Realtek PCIe GbE Family Controller", "192.168.0.100", true),
            entry("以太网", "Realtek PCIe GbE Family Controller", "2409:8a00:1:2::5", true),
        ];
        assert_eq!(
            filter_iface_addrs(entries),
            vec![
                "2409:8a00:1:2::5".parse::<IpAddr>().unwrap(),
                IpAddr::V4(Ipv4Addr::new(192, 168, 0, 100)),
            ],
            "IPv6 要排在前面"
        );
    }

    /// 没有 IPv6 的环境下这一步必须是空的，不能报错。
    #[test]
    fn 没有_ipv6_时安全跳过() {
        let entries = vec![
            entry("以太网", "Realtek PCIe GbE Family Controller", "192.168.0.100", true),
            // 链路本地 IPv6 不算可用
            entry("以太网", "Realtek PCIe GbE Family Controller", "fe80::1", true),
        ];
        let addrs = filter_iface_addrs(entries);
        assert_eq!(addrs, vec![IpAddr::V4(Ipv4Addr::new(192, 168, 0, 100))]);
        assert!(addrs.iter().all(|addr| addr.is_ipv4()));
    }

    #[test]
    fn sdp_候选行重排成_ipv6_在前() {
        let sdp = "\
v=0
m=application 9 UDP/DTLS/SCTP webrtc-datachannel
a=mid:0
a=candidate:1 1 udp 2130706431 192.168.0.100 5000 typ host
a=candidate:2 1 udp 2130706431 2409:8a00:1:2::5 5001 typ host
a=candidate:3 1 udp 1694498815 117.188.29.165 5002 typ srflx raddr 192.168.0.100 rport 5000
a=end-of-candidates
";
        let reordered = prefer_ipv6_candidates(sdp.to_string());
        let candidates: Vec<&str> = reordered
            .lines()
            .filter(|line| line.starts_with("a=candidate:"))
            .collect();
        assert_eq!(candidates.len(), 3, "只重排，不丢候选");
        assert!(candidates[0].contains("2409:8a00"), "{candidates:?}");
        assert!(candidates[1].contains("192.168.0.100"), "{candidates:?}");
        assert!(candidates[2].contains("117.188.29.165"), "{candidates:?}");
        assert!(reordered.contains("a=end-of-candidates"), "其它行不动");
        assert!(reordered.contains("a=mid:0"), "其它行不动");
    }

    #[test]
    fn 没有候选行时_sdp_原样返回() {
        let sdp = "v=0\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\n";
        assert_eq!(prefer_ipv6_candidates(sdp.to_string()), sdp);
    }

    #[test]
    fn stun_地址解析() {
        assert_eq!(
            parse_stun_addr("stun:1.2.3.4:3478").unwrap(),
            "1.2.3.4:3478".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            parse_stun_addr("stun:1.2.3.4").unwrap(),
            "1.2.3.4:3478".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            parse_stun_addr("stun:[::1]:3478?transport=udp").unwrap(),
            "[::1]:3478".parse::<SocketAddr>().unwrap()
        );
        assert!(parse_stun_addr("turn:1.2.3.4:3478").is_err());
    }

    #[test]
    fn 只认成功响应() {
        let mut ok = [0u8; 20];
        ok[..2].copy_from_slice(&BINDING_SUCCESS.to_be_bytes());
        ok[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
        assert!(binding_success(&ok));

        let mut request = ok;
        request[..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
        assert!(!binding_success(&request), "请求不是响应");

        assert!(!binding_success(&ok[..12]), "短包不算响应");
    }
}
