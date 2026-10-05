//! 会话编排：握手、能力协商、频道管理、事件流。
//!
//! 这是需求分析 §6.1 三层架构里的**会话编排层**：纯 Rust，不依赖任何 UI 框架。
//! UI 层只通过它发意图、收事件，不认识 WebRTC，也不认识编码器。
//!
//! ```text
//! UI 层（可替换）
//!    │  稳定的 API：connect / accept / send / next_event
//! 会话编排层（本 crate）
//!    │  Transport trait
//! 能力实现层（采集 / 编解码 / 传输 / 加密）
//! ```

use std::sync::Arc;
use std::time::Duration;

use secrelay_protocol::{Channel, ControlMessage, DeviceId, Frame, PROTOCOL_VERSION};
use secrelay_transport::{StatsSnapshot, Transport, TransportError};
use thiserror::Error;

pub use secrelay_protocol;
pub use secrelay_transport;

/// 默认握手超时。
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 能力标识常量。
///
/// 能力清单决定协商出哪些频道：双方都声明了 `screen`/`camera`/`mic` 才会有 `Media` 频道，
/// 都声明了 `file` 才会有 `File` 频道。`Control` 频道永远存在。
pub mod capabilities {
    pub const SCREEN: &str = "screen";
    pub const CAMERA: &str = "camera";
    pub const MIC: &str = "mic";
    pub const FILE: &str = "file";
    pub const TEXT: &str = "text";
    pub const CLIPBOARD: &str = "clipboard";

    /// 默认声明"什么都能做"，由对端协商裁剪。
    pub fn default_all() -> Vec<String> {
        [
            SCREEN, CAMERA, MIC, FILE, TEXT, CLIPBOARD,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }
}

/// 会话配置。
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// 本机设备 ID（长期身份公钥的短标识，不是账号）。
    pub device_id: DeviceId,
    /// 本机能力清单。
    pub capabilities: Vec<String>,
    /// 握手超时。
    pub handshake_timeout: Duration,
}

impl SessionConfig {
    pub fn new(device_id: DeviceId) -> Self {
        Self {
            device_id,
            capabilities: capabilities::default_all(),
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
        }
    }

    /// 只声明指定能力。
    pub fn with_capabilities(mut self, capabilities: Vec<String>) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }
}

/// 会话状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// 尚未开始握手。
    Init,
    /// 握手中。
    Handshaking,
    /// 已就绪，可以收发数据。
    Ready,
    /// 已关闭。
    Closed,
}

impl SessionState {
    fn name(self) -> &'static str {
        match self {
            SessionState::Init => "Init",
            SessionState::Handshaking => "Handshaking",
            SessionState::Ready => "Ready",
            SessionState::Closed => "Closed",
        }
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// 协商结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiation {
    /// 双方共同声明的能力。
    pub capabilities: Vec<String>,
    /// 据此可用的频道（至少含 `Control`）。
    pub channels: Vec<Channel>,
}

/// 计算双方能力的交集与可用频道。
///
/// 纯函数，方便单独测试 —— 协商规则是整个会话模型的核心。
pub fn negotiate(local: &[String], remote: &[String]) -> Negotiation {
    let mut common: Vec<String> = local
        .iter()
        .filter(|cap| remote.contains(cap))
        .cloned()
        .collect();
    common.sort();
    common.dedup();

    let has_media = common.iter().any(|c| {
        c == capabilities::SCREEN || c == capabilities::CAMERA || c == capabilities::MIC
    });
    let has_file = common.iter().any(|c| c == capabilities::FILE);

    let mut channels = Vec::with_capacity(3);
    if has_media {
        channels.push(Channel::Media);
    }
    if has_file {
        channels.push(Channel::File);
    }
    // 控制频道永远可用：没有它连"关闭会话"都发不出去。
    channels.push(Channel::Control);
    channels.sort_by_key(|c| c.as_u8());

    Negotiation {
        capabilities: common,
        channels,
    }
}

/// 对端信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub device_id: DeviceId,
    pub capabilities: Vec<String>,
    pub channels: Vec<Channel>,
}

/// 会话事件。
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// 未在会话层内部消化的控制消息（交给上层处理）。
    Control(ControlMessage),
    /// 数据帧（媒体或文件）。
    Frame(Frame),
    /// 对端报错。
    PeerError { code: String, message: String },
    /// 对端已关闭会话。
    PeerClosed,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("传输错误：{0}")]
    Transport(#[from] TransportError),

    #[error("协议错误：{0}")]
    Protocol(#[from] secrelay_protocol::ProtocolError),

    #[error("协议版本不兼容：本端 {local}，对端 {remote}")]
    VersionMismatch { local: u16, remote: u16 },

    #[error("握手超时（{0:?}）")]
    HandshakeTimeout(Duration),

    #[error("握手阶段收到意外消息：{0}")]
    UnexpectedMessage(String),

    #[error("对端在握手完成前关闭了连接")]
    PeerClosedDuringHandshake,

    #[error("对端拒绝会话：{code} {message}")]
    PeerRejected { code: String, message: String },

    #[error("会话状态不正确：期望 {expected}，实际 {actual}")]
    BadState {
        expected: &'static str,
        actual: SessionState,
    },

    #[error("频道 {0:?} 未被协商开启")]
    ChannelNotNegotiated(Channel),
}

/// 一条设备间会话。
pub struct Session {
    transport: Arc<dyn Transport>,
    config: SessionConfig,
    state: SessionState,
    peer: Option<PeerInfo>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("device_id", &self.config.device_id)
            .field("state", &self.state)
            .field("peer", &self.peer)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub fn new(transport: Arc<dyn Transport>, config: SessionConfig) -> Self {
        Self {
            transport,
            config,
            state: SessionState::Init,
            peer: None,
        }
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn peer(&self) -> Option<&PeerInfo> {
        self.peer.as_ref()
    }

    pub fn local_device_id(&self) -> &DeviceId {
        &self.config.device_id
    }

    /// 本端声明的能力。
    pub fn local_capabilities(&self) -> &[String] {
        &self.config.capabilities
    }

    /// 这条会话是否经过中继（KPI 口径，需求分析 §7）。
    pub fn is_relayed(&self) -> bool {
        self.transport.is_relayed()
    }

    pub fn stats(&self) -> StatsSnapshot {
        self.transport.stats()
    }

    /// 作为发起方完成握手。
    pub async fn connect(&mut self) -> Result<PeerInfo, SessionError> {
        self.ensure_state(SessionState::Init)?;
        self.state = SessionState::Handshaking;

        tracing::debug!(device_id = %self.config.device_id, "发送握手问候");
        self.send_control(ControlMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: self.config.device_id.clone(),
            capabilities: self.config.capabilities.clone(),
        })
        .await?;

        let reply = self.recv_control_with_timeout().await?;
        let peer = match reply {
            ControlMessage::HelloAck {
                protocol_version,
                device_id,
                capabilities,
            } => self.negotiate_with(protocol_version, device_id, capabilities)?,
            ControlMessage::Error { code, message } => {
                return Err(SessionError::PeerRejected { code, message })
            }
            other => {
                return Err(SessionError::UnexpectedMessage(format!("{other:?}")));
            }
        };

        self.peer = Some(peer.clone());
        self.state = SessionState::Ready;
        tracing::info!(
            peer = %peer.device_id,
            channels = ?peer.channels,
            "会话已就绪"
        );
        Ok(peer)
    }

    /// 作为应答方完成握手。
    pub async fn accept(&mut self) -> Result<PeerInfo, SessionError> {
        self.ensure_state(SessionState::Init)?;
        self.state = SessionState::Handshaking;

        let greeting = self.recv_control_with_timeout().await?;
        let peer = match greeting {
            ControlMessage::Hello {
                protocol_version,
                device_id,
                capabilities,
            } => self.negotiate_with(protocol_version, device_id, capabilities)?,
            other => {
                return Err(SessionError::UnexpectedMessage(format!("{other:?}")));
            }
        };

        self.send_control(ControlMessage::HelloAck {
            protocol_version: PROTOCOL_VERSION,
            device_id: self.config.device_id.clone(),
            capabilities: self.config.capabilities.clone(),
        })
        .await?;

        self.peer = Some(peer.clone());
        self.state = SessionState::Ready;
        tracing::info!(
            peer = %peer.device_id,
            channels = ?peer.channels,
            "会话已就绪"
        );
        Ok(peer)
    }

    /// 发送控制消息（握手阶段也允许）。
    pub async fn send_control(&self, message: ControlMessage) -> Result<(), SessionError> {
        if self.state == SessionState::Closed {
            return Err(SessionError::BadState {
                expected: "Init / Handshaking / Ready",
                actual: self.state,
            });
        }
        self.transport.send(Frame::control(message)).await?;
        Ok(())
    }

    /// 发送数据帧。必须先握手完成、且频道已协商开启。
    pub async fn send_raw(&self, channel: Channel, bytes: Vec<u8>) -> Result<(), SessionError> {
        self.ensure_state(SessionState::Ready)?;
        let negotiated = self
            .peer
            .as_ref()
            .map(|p| p.channels.contains(&channel))
            .unwrap_or(false);
        if !negotiated {
            return Err(SessionError::ChannelNotNegotiated(channel));
        }
        self.transport.send(Frame::raw(channel, bytes)?).await?;
        Ok(())
    }

    /// 发送心跳。
    pub async fn ping(&self, nonce: u64) -> Result<(), SessionError> {
        self.send_control(ControlMessage::Ping { nonce }).await
    }

    /// 取下一个事件。
    ///
    /// 心跳在内部自动应答，不会冒泡到调用方；媒体与文件帧原样返回。
    pub async fn next_event(&mut self) -> Result<SessionEvent, SessionError> {
        self.ensure_state(SessionState::Ready)?;

        loop {
            let frame = match self.transport.recv().await? {
                Some(frame) => frame,
                None => {
                    self.state = SessionState::Closed;
                    return Ok(SessionEvent::PeerClosed);
                }
            };

            let Some(message) = frame.as_control().cloned() else {
                return Ok(SessionEvent::Frame(frame));
            };

            match message {
                ControlMessage::Ping { nonce } => {
                    tracing::trace!(nonce, "自动应答心跳");
                    self.send_control(ControlMessage::Pong { nonce }).await?;
                }
                ControlMessage::Pong { nonce } => {
                    tracing::trace!(nonce, "收到心跳应答");
                }
                ControlMessage::Bye { reason } => {
                    tracing::info!(%reason, "对端主动结束会话");
                    self.state = SessionState::Closed;
                    return Ok(SessionEvent::PeerClosed);
                }
                ControlMessage::Error { code, message } => {
                    return Ok(SessionEvent::PeerError { code, message });
                }
                other => return Ok(SessionEvent::Control(other)),
            }
        }
    }

    /// 有序关闭会话。
    pub async fn close(&mut self, reason: &str) -> Result<(), SessionError> {
        if self.state == SessionState::Closed {
            return Ok(());
        }
        // 尽力而为地告别：对端可能已经走了，失败不阻塞本地关闭。
        if let Err(err) = self
            .send_control(ControlMessage::Bye {
                reason: reason.to_string(),
            })
            .await
        {
            tracing::debug!("发送告别消息失败（对端可能已断开）：{err}");
        }
        self.transport.close().await?;
        self.state = SessionState::Closed;
        Ok(())
    }

    fn ensure_state(&self, expected: SessionState) -> Result<(), SessionError> {
        if self.state != expected {
            return Err(SessionError::BadState {
                expected: expected.name(),
                actual: self.state,
            });
        }
        Ok(())
    }

    fn negotiate_with(
        &self,
        remote_version: u16,
        remote_id: DeviceId,
        remote_caps: Vec<String>,
    ) -> Result<PeerInfo, SessionError> {
        if remote_version != PROTOCOL_VERSION {
            return Err(SessionError::VersionMismatch {
                local: PROTOCOL_VERSION,
                remote: remote_version,
            });
        }
        let negotiation = negotiate(&self.config.capabilities, &remote_caps);
        Ok(PeerInfo {
            device_id: remote_id,
            capabilities: negotiation.capabilities,
            channels: negotiation.channels,
        })
    }

    /// 只在握手阶段使用：跳过数据帧，直到拿到一条控制消息。
    async fn recv_control(&self) -> Result<ControlMessage, SessionError> {
        loop {
            match self.transport.recv().await? {
                Some(frame) => {
                    if let Some(message) = frame.as_control() {
                        return Ok(message.clone());
                    }
                    // 握手阶段的数据帧按无效处理（正常情况下不会出现）。
                    tracing::debug!("握手阶段忽略了数据帧");
                }
                None => return Err(SessionError::PeerClosedDuringHandshake),
            }
        }
    }

    async fn recv_control_with_timeout(&self) -> Result<ControlMessage, SessionError> {
        tokio::time::timeout(self.config.handshake_timeout, self.recv_control())
            .await
            .map_err(|_| SessionError::HandshakeTimeout(self.config.handshake_timeout))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrelay_transport::loopback_pair;

    fn config(tag: &str) -> SessionConfig {
        SessionConfig::new(DeviceId::new(tag).unwrap())
    }

    fn pair() -> (Session, Session) {
        let (ta, tb) = loopback_pair();
        (
            Session::new(ta, config("dev-a")),
            Session::new(tb, config("dev-b")),
        )
    }

    /// 并发完成握手；任一侧失败即 panic（测试里失败就是失败）。
    async fn handshake(a: &mut Session, b: &mut Session) {
        let (ra, rb) = tokio::join!(a.connect(), b.accept());
        ra.expect("A 侧握手应当成功");
        rb.expect("B 侧握手应当成功");
    }

    #[tokio::test]
    async fn 握手成功且双方都知道对方身份() {
        let (mut a, mut b) = pair();

        let (ra, rb) = tokio::join!(a.connect(), b.accept());
        let info_a = ra.expect("A 握手应当成功");
        let info_b = rb.expect("B 握手应当成功");

        assert_eq!(info_a.device_id.as_str(), "dev-b");
        assert_eq!(info_b.device_id.as_str(), "dev-a");
        assert_eq!(a.state(), SessionState::Ready);
        assert_eq!(b.state(), SessionState::Ready);
        assert_eq!(a.peer().unwrap().device_id.as_str(), "dev-b");
    }

    #[tokio::test]
    async fn 握手后三个频道都可用() {
        let (mut a, mut b) = pair();
        let (ra, _) = tokio::join!(a.connect(), b.accept());
        let info = ra.unwrap();

        assert_eq!(
            info.channels,
            vec![Channel::Media, Channel::File, Channel::Control],
            "默认能力清单应当协商出全部三个频道"
        );
    }

    #[tokio::test]
    async fn 数据帧双向可达() {
        let (mut a, mut b) = pair();
        handshake(&mut a, &mut b).await;

        a.send_raw(Channel::Media, vec![1, 2, 3]).await.unwrap();
        match b.next_event().await.unwrap() {
            SessionEvent::Frame(frame) => {
                assert_eq!(frame.channel, Channel::Media);
                assert_eq!(frame.as_raw(), Some(&[1u8, 2, 3][..]));
            }
            other => panic!("期望数据帧，收到 {other:?}"),
        }

        b.send_raw(Channel::File, vec![9; 200]).await.unwrap();
        match a.next_event().await.unwrap() {
            SessionEvent::Frame(frame) => assert_eq!(frame.channel, Channel::File),
            other => panic!("期望数据帧，收到 {other:?}"),
        }
    }

    #[tokio::test]
    async fn 能力协商取交集() {
        let negotiation = negotiate(
            &["screen".into(), "camera".into(), "file".into()],
            &["camera".into(), "file".into(), "voice".into()],
        );
        assert_eq!(negotiation.capabilities, vec!["camera", "file"]);
        assert_eq!(
            negotiation.channels,
            vec![Channel::Media, Channel::File, Channel::Control]
        );
    }

    #[tokio::test]
    async fn 无共同能力时只剩控制频道() {
        let negotiation = negotiate(&["screen".into()], &["file".into()]);
        assert!(negotiation.capabilities.is_empty());
        assert_eq!(negotiation.channels, vec![Channel::Control]);
    }

    #[tokio::test]
    async fn 单方声明_file_不会开启文件频道() {
        let negotiation = negotiate(&["file".into(), "text".into()], &["text".into()]);
        assert_eq!(negotiation.capabilities, vec!["text"]);
        assert!(
            !negotiation.channels.contains(&Channel::File),
            "只有一方声明 file 时不应开启文件频道"
        );
    }

    #[tokio::test]
    async fn 未协商的频道不能发送() {
        let (ta, tb) = loopback_pair();
        let mut a = Session::new(
            ta,
            config("dev-a").with_capabilities(vec!["text".into()]),
        );
        let mut b = Session::new(
            tb,
            config("dev-b").with_capabilities(vec!["text".into()]),
        );
        handshake(&mut a, &mut b).await;

        let err = a
            .send_raw(Channel::Media, vec![1])
            .await
            .expect_err("媒体频道未被协商，应当拒绝");
        assert!(matches!(
            err,
            SessionError::ChannelNotNegotiated(Channel::Media)
        ));

        // 控制频道已被协商，但原始字节不能走控制频道 —— 应当以协议错误被拒。
        let err = a
            .send_raw(Channel::Control, vec![1])
            .await
            .expect_err("控制频道不接受原始字节");
        assert!(matches!(
            err,
            SessionError::Protocol(secrelay_protocol::ProtocolError::PayloadMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn 版本不匹配被拒绝() {
        let (ta, tb) = loopback_pair();
        let mut b = Session::new(tb, config("dev-b"));

        // 手工伪造一个版本过高的 Hello
        ta.send(Frame::control(ControlMessage::Hello {
            protocol_version: PROTOCOL_VERSION + 1,
            device_id: DeviceId::new("dev-old").unwrap(),
            capabilities: vec![],
        }))
        .await
        .unwrap();

        let err = b.accept().await.expect_err("版本不匹配应当失败");
        assert!(matches!(err, SessionError::VersionMismatch { .. }));
    }

    #[tokio::test]
    async fn 心跳被自动应答且不上抛() {
        let (mut a, mut b) = pair();
        handshake(&mut a, &mut b).await;

        a.ping(7).await.unwrap();
        // B 侧 next_event 会消化 Ping 并回 Pong；这里应读到 A 发来的 Pong。
        // 先让 B 处理一次事件：它会自动回 Pong 并继续等待，因此用超时保护。
        let b_task = tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_millis(200), b.next_event()).await;
            b
        });

        // A 侧应当收到 Pong，且不会把它当作事件上抛
        let pong = a.recv_control().await.unwrap();
        assert_eq!(pong, ControlMessage::Pong { nonce: 7 });

        let _b = b_task.await.unwrap();
    }

    #[tokio::test]
    async fn 对端告别产生_peer_closed() {
        let (mut a, mut b) = pair();
        handshake(&mut a, &mut b).await;

        a.close("测试结束").await.unwrap();
        assert_eq!(a.state(), SessionState::Closed);

        assert_eq!(b.next_event().await.unwrap(), SessionEvent::PeerClosed);
        assert_eq!(b.state(), SessionState::Closed);
    }

    #[tokio::test]
    async fn 握手超时会报错() {
        let (ta, _tb) = loopback_pair();
        let mut a = Session::new(
            ta,
            config("dev-a").with_handshake_timeout(Duration::from_millis(50)),
        );
        let err = a.connect().await.expect_err("没人应答应当超时");
        assert!(matches!(err, SessionError::HandshakeTimeout(_)));
    }

    #[tokio::test]
    async fn 状态机拒绝乱序调用() {
        let (a, _b) = pair();
        // 还没握手就发数据
        let err = a.send_raw(Channel::Media, vec![1]).await.unwrap_err();
        assert!(matches!(err, SessionError::BadState { .. }));

        // 重复 connect
        let (mut c, _d) = pair();
        c.state = SessionState::Ready;
        assert!(matches!(
            c.connect().await.unwrap_err(),
            SessionError::BadState { .. }
        ));
    }

    #[tokio::test]
    async fn 关闭是幂等的() {
        let (mut a, mut b) = pair();
        handshake(&mut a, &mut b).await;
        a.close("第一次").await.unwrap();
        a.close("第二次").await.unwrap();
        assert_eq!(a.state(), SessionState::Closed);
    }
}
