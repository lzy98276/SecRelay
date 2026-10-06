//! 连接抽象。
//!
//! 需求分析 §6.3 定了三条数据路径：**ICE 直连**、**TURN 中继兜底**、
//! **原生之间的独立 QUIC 文件通道**。它们必须收敛到同一个 [`Transport`] trait 后面，
//! 原因是两处已识别的高风险：
//!
//! 1. `webrtc-rs` 生态仍在动荡（历史上每连接约 109 KiB 的线性内存泄漏），
//!    万一要换库，改动必须被限制在这一层。
//! 2. 文件通道的实现路径（复用 ICE + `quinn` vs 直接用 `iroh`）还没定（D24），
//!    定下来之前上层代码不该知道它用的是哪个。
//!
//! 本 crate 提供两种实现：
//!
//! - [`WebRtcTransport`]：真实网络上的 ICE 打洞 + DataChannel，ICE 服务器列表由
//!   [`IceConfig`] 传入（STUN/TURN + 短期凭据）。
//! - [`loopback_pair`]：进程内回环，让协议与会话逻辑能脱离真实网络被测试。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use secrelay_protocol::Frame;
use thiserror::Error;
use tokio::sync::{mpsc, Mutex};

pub mod ice;
mod webrtc_transport;

pub use ice::{parse_bind_addr, IceConfig, IceServerConfig};
pub use webrtc_transport::{
    CandidatePair, WebRtcTransport, DATA_CHANNEL_LABEL, DEFAULT_CONNECT_TIMEOUT,
    DEFAULT_GATHER_TIMEOUT, MAX_DATACHANNEL_MESSAGE,
};

/// 回环通道的缓冲帧数。
const LOOPBACK_CAPACITY: usize = 64;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("连接已关闭")]
    Closed,

    #[error("底层 I/O 失败：{0}")]
    Io(String),

    #[error("WebRTC 失败：{0}")]
    WebRtc(String),

    #[error("协议错误：{0}")]
    Protocol(#[from] secrelay_protocol::ProtocolError),
}

/// 连接统计的只读快照。
///
/// 直连成功率与**中继占比**是项目的核心 KPI（需求分析 §7，目标中继占比 <15%），
/// 所以统计口径从第一版连接抽象就要有。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub frames_sent: u64,
    pub frames_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

impl StatsSnapshot {
    /// 累计往返字节数。
    pub fn total_bytes(&self) -> u64 {
        self.bytes_sent.saturating_add(self.bytes_received)
    }
}

#[derive(Debug, Default)]
pub struct TransportStats {
    frames_sent: AtomicU64,
    frames_received: AtomicU64,
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
}

impl TransportStats {
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            frames_sent: self.frames_sent.load(Ordering::Relaxed),
            frames_received: self.frames_received.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
        }
    }
}

/// 一条已建立的设备间连接。
///
/// 实现者负责：帧的可靠投递、关闭语义、以及如实汇报统计。
/// 加密不在这里做 —— 端到端加密是会话层的事，传输层只搬字节。
#[async_trait]
pub trait Transport: Send + Sync + 'static {
    /// 发送一帧。
    async fn send(&self, frame: Frame) -> Result<(), TransportError>;

    /// 接收下一帧。
    ///
    /// 连接被对端正常关闭、且缓冲已排空时返回 `Ok(None)`。
    async fn recv(&self) -> Result<Option<Frame>, TransportError>;

    /// 关闭连接。幂等。
    async fn close(&self) -> Result<(), TransportError>;

    /// 取统计快照。
    fn stats(&self) -> StatsSnapshot;

    /// 这条连接是否经过中继。
    ///
    /// 用于计算中继占比。直连必须返回 `false`。
    fn is_relayed(&self) -> bool;
}

/// 回环传输：两端在同一个进程内直连。
///
/// 它的用途只有一个 —— 让协议与会话逻辑**可以脱离真实网络被测试**。
/// 它不做任何加密，也**不是** P2P 的替代品。
pub struct LoopbackTransport {
    name: &'static str,
    /// `None` 表示已关闭（drop 掉发送端会让对端 `recv` 返回 `Ok(None)`）。
    outbound: Mutex<Option<mpsc::Sender<Frame>>>,
    inbound: Mutex<mpsc::Receiver<Frame>>,
    stats: TransportStats,
    closed: AtomicBool,
}

impl std::fmt::Debug for LoopbackTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopbackTransport")
            .field("name", &self.name)
            .field("closed", &self.closed.load(Ordering::Relaxed))
            .field("stats", &self.stats.snapshot())
            .finish()
    }
}

impl LoopbackTransport {
    fn new(
        name: &'static str,
        outbound: mpsc::Sender<Frame>,
        inbound: mpsc::Receiver<Frame>,
    ) -> Self {
        Self {
            name,
            outbound: Mutex::new(Some(outbound)),
            inbound: Mutex::new(inbound),
            stats: TransportStats::default(),
            closed: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl Transport for LoopbackTransport {
    async fn send(&self, frame: Frame) -> Result<(), TransportError> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(TransportError::Closed);
        }

        // 真的编码一次：这样统计出来的字节数与真实传输层一致，
        // 也顺带在测试里覆盖了协议校验。
        let encoded_len = frame.encode()?.len() as u64;

        let sender = {
            let guard = self.outbound.lock().await;
            guard.clone()
        };
        let Some(sender) = sender else {
            return Err(TransportError::Closed);
        };

        sender
            .send(frame)
            .await
            .map_err(|_| TransportError::Closed)?;

        self.stats.frames_sent.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_sent
            .fetch_add(encoded_len, Ordering::Relaxed);
        Ok(())
    }

    async fn recv(&self) -> Result<Option<Frame>, TransportError> {
        let mut inbound = self.inbound.lock().await;
        match inbound.recv().await {
            Some(frame) => {
                let encoded_len = frame.encode().map(|b| b.len() as u64).unwrap_or(0);
                self.stats.frames_received.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .bytes_received
                    .fetch_add(encoded_len, Ordering::Relaxed);
                Ok(Some(frame))
            }
            None => Ok(None),
        }
    }

    async fn close(&self) -> Result<(), TransportError> {
        self.closed.store(true, Ordering::Relaxed);
        // drop 发送端：对端的 recv 在排空缓冲后会得到 Ok(None)。
        let taken = {
            let mut guard = self.outbound.lock().await;
            guard.take()
        };
        drop(taken);
        tracing::debug!(endpoint = self.name, "回环传输已关闭");
        Ok(())
    }

    fn stats(&self) -> StatsSnapshot {
        self.stats.snapshot()
    }

    fn is_relayed(&self) -> bool {
        false
    }
}

/// 创建一对互连的回环传输。
///
/// 返回的 `(a, b)` 中，`a.send` 的帧会出现在 `b.recv` 上，反之亦然。
pub fn loopback_pair() -> (Arc<dyn Transport>, Arc<dyn Transport>) {
    let (tx_a, rx_a) = mpsc::channel(LOOPBACK_CAPACITY);
    let (tx_b, rx_b) = mpsc::channel(LOOPBACK_CAPACITY);

    let a = Arc::new(LoopbackTransport::new("A", tx_a, rx_b));
    let b = Arc::new(LoopbackTransport::new("B", tx_b, rx_a));
    (a, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrelay_protocol::{Channel, ControlMessage, DeviceId, PROTOCOL_VERSION};

    fn hello(tag: &str) -> Frame {
        Frame::control(ControlMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId::new(tag).unwrap(),
            capabilities: vec![],
        })
    }

    #[tokio::test]
    async fn 双向投递且保序() {
        let (a, b) = loopback_pair();

        for i in 0..8u8 {
            a.send(Frame::raw(Channel::Media, vec![i]).unwrap())
                .await
                .unwrap();
        }
        for i in 0..8u8 {
            let frame = b.recv().await.unwrap().expect("应当收到帧");
            assert_eq!(frame.as_raw(), Some(&[i][..]));
        }

        b.send(hello("dev-b")).await.unwrap();
        let frame = a.recv().await.unwrap().expect("A 应当收到帧");
        assert!(matches!(
            frame.as_control(),
            Some(ControlMessage::Hello { .. })
        ));
    }

    #[tokio::test]
    async fn 统计如实计数() {
        let (a, b) = loopback_pair();
        let frame = Frame::raw(Channel::File, vec![7u8; 100]).unwrap();
        let expected_bytes = frame.encode().unwrap().len() as u64;

        a.send(frame).await.unwrap();
        let _ = b.recv().await.unwrap().unwrap();

        assert_eq!(a.stats().frames_sent, 1);
        assert_eq!(a.stats().bytes_sent, expected_bytes);
        assert_eq!(a.stats().frames_received, 0);
        assert_eq!(b.stats().frames_received, 1);
        assert_eq!(b.stats().bytes_received, expected_bytes);
        assert!(!a.is_relayed(), "回环不是中继");
    }

    #[tokio::test]
    async fn 一端关闭后对端读到_none() {
        let (a, b) = loopback_pair();
        a.send(Frame::raw(Channel::Media, vec![1]).unwrap())
            .await
            .unwrap();
        a.close().await.unwrap();

        // 缓冲里那一帧仍然读得到
        assert!(b.recv().await.unwrap().is_some());
        // 之后就是正常关闭
        assert!(b.recv().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn 关闭后不能再发送() {
        let (a, _b) = loopback_pair();
        a.close().await.unwrap();
        let err = a
            .send(Frame::raw(Channel::Media, vec![1]).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(err, TransportError::Closed));
    }

    #[tokio::test]
    async fn 关闭是幂等的() {
        let (a, _b) = loopback_pair();
        a.close().await.unwrap();
        a.close().await.unwrap();
    }

    #[tokio::test]
    async fn 非法帧在发送时就被拒绝() {
        let (a, _b) = loopback_pair();
        // 控制频道不接受原始字节
        let bad = Frame {
            channel: Channel::Control,
            payload: secrelay_protocol::Payload::Raw(vec![1, 2, 3]),
        };
        let err = a.send(bad).await.unwrap_err();
        assert!(matches!(
            err,
            TransportError::Protocol(secrelay_protocol::ProtocolError::PayloadMismatch { .. })
        ));
        assert_eq!(a.stats().frames_sent, 0, "失败的发送不应计入统计");
    }
}
