//! 跑在真实网络上的传输：ICE 打洞 + DTLS/SCTP DataChannel。
//!
//! 一帧编码成一条二进制 DataChannel 消息 —— SCTP 本身面向消息，不需要再自己分帧。
//! 所有频道共用这一条 DataChannel：频道信息已经在帧头里。
//!
//! SDP 怎么交换由调用方决定（当前不依赖信令服务器），传输层只提供三步：
//! [`WebRtcTransport::create_offer`] → [`WebRtcTransport::accept_offer`] →
//! [`WebRtcTransport::accept_answer`]。

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use secrelay_protocol::Frame;
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
    pub async fn new_with_bind(
        config: &IceConfig,
        bind: Vec<String>,
    ) -> Result<Self, TransportError> {
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
        self.wait_for_gathering().await
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
        self.wait_for_gathering().await
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
        let deadline = Instant::now() + DEFAULT_GATHER_TIMEOUT;
        loop {
            // 状态由事件回调写入：候选收集一完成就退出，不用轮询整份统计。
            if self.handler.gather_state() == GatherState::Complete {
                break;
            }
            if Instant::now() >= deadline {
                return Err(TransportError::WebRtc(format!(
                    "ICE 候选收集超时（{DEFAULT_GATHER_TIMEOUT:?}）"
                )));
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
    bind: Vec<String>,
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

    let builder = PeerConnectionBuilder::new()
        .with_configuration(rtc_config)
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .with_handler(handler as Arc<dyn PeerConnectionEventHandler>)
        .with_runtime(runtime)
        .with_udp_addrs(bind)
        .with_data_channel_send_buffer_limit(SEND_BUFFER_LIMIT);

    builder
        .build()
        .await
        .map(|pc| Arc::new(pc) as Arc<dyn PeerConnection>)
        .map_err(webrtc_error)
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
}
