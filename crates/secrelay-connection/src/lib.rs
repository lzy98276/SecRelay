//! 一条连接的编排：中继发现 → 信令交换 SDP → ICE 建连 → 会话握手。
//!
//! 直连优先，失败后回退中继：回退靠"第二轮只在 TURN 候选上重试"实现，
//! 会话码由发起方创建并展示给用户，两轮复用同一个会话码。
//!
//! 两件容易踩的事写在这里：
//!
//! - 服务端只把消息转给**当前在会话里**的连接，所以整个建连过程只维持一条信令连接，
//!   收发都走 [`SignalPump`]，中途不会掉线。
//! - ICE 候选随 SDP 一起交换（传输层收完候选才出 SDP），因此这里不透传 candidate。

pub mod error;
pub mod ports;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use secrelay_protocol::DeviceId;
use secrelay_relay_client::{
    ClientMsg, Discovery, Endpoint, IdCheck, ServerMsg, SignalSink, SignalSocket,
};
use secrelay_session::{PeerInfo, Session, SessionConfig, SessionError};
use secrelay_transport::ice::IceServerConfig;
use secrelay_transport::{IceConfig, Transport, WebRtcTransport};
use serde_json::{json, Value};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;

use crate::error::ConnectionError;

/// 一轮建连的超时。
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// 等对端加入会话的超时。
pub const DEFAULT_PEER_TIMEOUT: Duration = Duration::from_secs(120);
/// 会话握手的超时。
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// 回退到中继前等一小会儿，让上一轮的 DTLS 收尾。
const FALLBACK_GRACE: Duration = Duration::from_millis(500);
/// 中继占比的轮询间隔。
const RELAY_WATCH_INTERVAL: Duration = Duration::from_millis(200);
/// 检查取消标志的间隔。
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// SDP 载荷里的字段名。
const SDP_TYPE: &str = "type";
const SDP_VALUE: &str = "sdp";
const SDP_ANSWER: &str = "answer";

/// 一次建连的参数。
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// 用户选中的中继基址。
    pub endpoint: String,
    /// 本端在信令里的匿名身份。
    pub peer_id: String,
    /// 本机设备 ID（长期身份短标识）。
    pub device_id: DeviceId,
    /// 加入已有会话时用的短码；发起新会话时留空。
    pub session_code: String,
    /// 只当发起方：已经有会话码时也另建一个。
    pub force_new_session: bool,
    /// 本机 UDP 绑定地址。
    pub bind: Vec<String>,
    /// 声明的能力清单。
    pub capabilities: Vec<String>,
    /// 一轮建连的超时。
    pub connect_timeout: Duration,
    /// 等对端加入会话的超时。
    pub peer_timeout: Duration,
    /// 会话握手超时。
    pub handshake_timeout: Duration,
}

impl ConnectOptions {
    pub fn new(endpoint: impl Into<String>, peer_id: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            peer_id: peer_id.into(),
            device_id: DeviceId::new("secrelay-client").expect("固定设备 ID 合法"),
            session_code: String::new(),
            force_new_session: false,
            bind: secrelay_transport::ice::default_udp_addrs(),
            capabilities: secrelay_session::capabilities::default_all(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            peer_timeout: DEFAULT_PEER_TIMEOUT,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
        }
    }

    /// 加入已有的会话。
    pub fn joining(mut self, code: impl Into<String>) -> Self {
        self.session_code = code.into();
        self
    }

    /// 无论会话码是否为空都新建会话。
    pub fn as_offerer(mut self) -> Self {
        self.force_new_session = true;
        self
    }

    pub fn with_device_id(mut self, device_id: DeviceId) -> Self {
        self.device_id = device_id;
        self
    }

    pub fn with_capabilities(mut self, capabilities: Vec<String>) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn with_bind(mut self, bind: Vec<String>) -> Self {
        self.bind = bind;
        self
    }

    pub fn with_timeouts(mut self, connect: Duration, peer: Duration, handshake: Duration) -> Self {
        self.connect_timeout = connect;
        self.peer_timeout = peer;
        self.handshake_timeout = handshake;
        self
    }

    /// 本端是发起方（新建会话）还是应答方（加入会话）。
    pub fn role(&self) -> Role {
        if self.force_new_session || self.session_code.trim().is_empty() {
            Role::Offerer
        } else {
            Role::Answerer
        }
    }
}

/// 本端在一次建连里的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 新建会话、发 offer。
    Offerer,
    /// 加入会话、回 answer。
    Answerer,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Offerer => "发起方",
            Role::Answerer => "应答方",
        }
    }
}

/// 建连过程中的进度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// 正在从中继取配置。
    Discovering,
    /// 拿到了配置。
    Discovered { id_check: IdCheck, turn_ready: bool },
    /// 正在连信令。
    Signaling,
    /// 信令已连上，会话码已经确定。
    SessionReady { session_code: String, role: Role },
    /// 对端已进入会话。
    PeerJoined { peer_id: String },
    /// 正在交换 SDP 并打洞。
    Connecting { kind: AttemptKind },
    /// 会话已就绪。候选对变化时会再次上报。
    Connected { kind: AttemptKind, relayed: bool },
    /// 直连失败，改用中继候选重试。
    FallbackToRelay { reason: String },
    /// 建连失败。
    Failed { reason: String },
    /// 被调用方取消。
    Cancelled,
}

/// 一轮尝试用的是什么策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptKind {
    /// 直连优先：所有候选都可以用。
    Direct,
    /// 回退中继：只接受 TURN 候选。
    RelayOnly,
}

impl AttemptKind {
    pub fn label(self) -> &'static str {
        match self {
            AttemptKind::Direct => "直连尝试",
            AttemptKind::RelayOnly => "中继回退",
        }
    }

    fn plan(self) -> IcePlan {
        match self {
            AttemptKind::Direct => IcePlan::All,
            AttemptKind::RelayOnly => IcePlan::RelayOnly,
        }
    }
}

/// 一轮尝试的结局。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub kind: AttemptKind,
    /// 失败原因。
    pub error: String,
}

/// 建连成功的详情。
#[derive(Debug, Clone, PartialEq)]
pub struct Connected {
    /// 会话码，对端用它加入。
    pub session_code: String,
    /// 实际连上的策略。
    pub kind: AttemptKind,
    /// 建连时刻看到的中继占比读数。
    pub relayed: bool,
    /// 握手拿到的对端信息。
    pub peer: PeerInfo,
}

/// 建连失败：每一轮的记录，加上一句汇总。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub attempts: Vec<Attempt>,
    pub reason: String,
}

impl Failure {
    fn from_attempts(attempts: Vec<Attempt>, reason: String) -> Self {
        Self { attempts, reason }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// 建连的结局。
#[derive(Debug, PartialEq, Eq)]
pub enum ConnectError {
    /// 建立失败。
    Failed(Failure),
    /// 被调用方取消。
    Cancelled,
}

impl ConnectError {
    /// 失败原因的汇总，给界面显示。
    pub fn reason(&self) -> String {
        match self {
            ConnectError::Failed(failure) => failure.reason.clone(),
            ConnectError::Cancelled => "已取消".to_string(),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, ConnectError::Cancelled)
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason())
    }
}

/// 建连过程的订阅端。
pub struct ProgressFeed {
    receiver: watch::Receiver<Option<Progress>>,
}

impl ProgressFeed {
    /// 当前进度（还没收到任何进度时是 `None`）。
    pub fn snapshot(&self) -> Option<Progress> {
        self.receiver.borrow().clone()
    }

    /// 等一次进度变化；发送端退出时返回 `None`。
    pub async fn changed(&mut self) -> Option<Progress> {
        self.receiver.changed().await.ok()?;
        self.snapshot()
    }
}

/// 建连发起侧：进度、取消与结果都从这里出去。
pub struct Connector {
    options: ConnectOptions,
    cancel: Arc<AtomicBool>,
    progress: watch::Sender<Option<Progress>>,
    progress_rx: watch::Receiver<Option<Progress>>,
}

impl Connector {
    pub fn new(options: ConnectOptions) -> Self {
        let (progress, progress_rx) = watch::channel(None);
        Self {
            options,
            cancel: Arc::new(AtomicBool::new(false)),
            progress,
            progress_rx,
        }
    }

    pub fn options(&self) -> &ConnectOptions {
        &self.options
    }

    /// 读取进度的订阅端。
    pub fn subscribe(&self) -> ProgressFeed {
        ProgressFeed {
            receiver: self.progress_rx.clone(),
        }
    }

    /// 打断正在进行的建连。
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// 按 `options` 里的会话码决定角色。
    pub async fn connect(&self) -> Result<Connection, ConnectError> {
        match self.options.role() {
            Role::Offerer => self.connect_offer().await,
            Role::Answerer => self.connect_answer().await,
        }
    }

    fn report(&self, progress: Progress) {
        let _ = self.progress.send(Some(progress));
    }

    fn cancelled(&self) -> ConnectError {
        self.report(Progress::Cancelled);
        ConnectError::Cancelled
    }

    fn fail(&self, attempts: Vec<Attempt>, reason: String) -> ConnectError {
        self.report(Progress::Failed {
            reason: reason.clone(),
        });
        ConnectError::Failed(Failure::from_attempts(attempts, reason))
    }

    fn fail_early(&self, reason: String) -> ConnectError {
        self.fail(Vec::new(), reason)
    }

    /// 一轮失败后的去向：直连还能回退就返回 `None`，否则给出终态。
    fn after_round_failure(
        &self,
        discovery: &Discovery,
        attempts: &mut Vec<Attempt>,
        kind: AttemptKind,
        err: ConnectionError,
    ) -> Option<ConnectError> {
        let text = err.message();
        attempts.push(Attempt {
            kind,
            error: text.clone(),
        });
        let direct = attempts
            .iter()
            .find(|attempt| attempt.kind == AttemptKind::Direct)
            .map(|attempt| attempt.error.clone())
            .unwrap_or_else(|| text.clone());

        if kind == AttemptKind::RelayOnly {
            // 回退也失败了：两句都留着，否则用户只看到一半。
            return Some(self.fail(std::mem::take(attempts), format!("{direct}；{text}")));
        }
        match can_fallback(discovery) {
            Ok(()) => {
                self.report(Progress::FallbackToRelay { reason: text });
                None
            }
            Err(reason) => {
                let reason = format!("直连失败（{text}），{reason}");
                Some(self.fail(std::mem::take(attempts), reason))
            }
        }
    }

    /// 发起方：新建会话码，发 offer，等 answer。
    pub async fn connect_offer(&self) -> Result<Connection, ConnectError> {
        let mut attempts = Vec::new();
        let mut open = self
            .open_offer()
            .await
            .map_err(|err| self.fail_early(err.message()))?;
        let code = open.session_code.clone();

        if let Err(err) = wait_peer_join(&mut open, &self.options, &self.progress, &self.cancel).await
        {
            return Err(self.fail_early(err.message()));
        }

        for (index, kind) in [AttemptKind::Direct, AttemptKind::RelayOnly]
            .into_iter()
            .enumerate()
        {
            if self.is_cancelled() {
                return Err(self.cancelled());
            }
            if index > 0 {
                // 第二轮不等 peer_joined：对端还在会话里，收到新 offer 会重新协商。
                tokio::time::sleep(FALLBACK_GRACE).await;
            }

            let ice = match resolve_ice(&open.discovery, kind) {
                Ok(ice) => ice,
                Err(err) => {
                    attempts.push(Attempt {
                        kind,
                        error: err.message(),
                    });
                    return Err(self.fail(attempts, err.message()));
                }
            };
            self.report(Progress::Connecting { kind });

            let transport =
                match build_offer(&mut open.signal, &self.options, ice, &self.cancel).await {
                    Ok(transport) => transport,
                    Err(RoundError::Cancelled) => return Err(self.cancelled()),
                    Err(RoundError::Failed(err)) => {
                        tracing::warn!(attempt = kind.label(), "本轮建连失败：{}", err.message());
                        match self.after_round_failure(&open.discovery, &mut attempts, kind, err) {
                            Some(error) => return Err(error),
                            None => continue,
                        }
                    }
                };

            match handshake(&self.options, &transport, true).await {
                Ok((session, peer)) => {
                    return Ok(finish(
                        transport,
                        session,
                        code,
                        kind,
                        peer,
                        open.signal,
                        self.progress.clone(),
                    ))
                }
                Err(err) => {
                    drop(transport);
                    match self.after_round_failure(&open.discovery, &mut attempts, kind, err) {
                        Some(error) => return Err(error),
                        None => continue,
                    }
                }
            }
        }

        let reason = attempts
            .last()
            .map(|attempt| attempt.error.clone())
            .unwrap_or_else(|| "直连与中继都没连上".to_string());
        Err(self.fail(attempts, reason))
    }

    /// 应答方：加入会话码，逐轮接 offer 并回 answer。
    pub async fn connect_answer(&self) -> Result<Connection, ConnectError> {
        let mut attempts = Vec::new();
        let mut open = self
            .open_answer()
            .await
            .map_err(|err| self.fail_early(err.message()))?;
        let code = open.session_code.clone();

        for (index, kind) in [AttemptKind::Direct, AttemptKind::RelayOnly]
            .into_iter()
            .enumerate()
        {
            if self.is_cancelled() {
                return Err(self.cancelled());
            }

            let offer = match await_offer(
                &mut open,
                &self.options,
                index > 0,
                &self.progress,
                &self.cancel,
            )
            .await
            {
                Ok(Some(offer)) => offer,
                Ok(None) => return Err(self.cancelled()),
                Err(err) => return Err(self.fail_early(err.message())),
            };

            let ice = match resolve_ice(&open.discovery, kind) {
                Ok(ice) => ice,
                Err(err) => {
                    attempts.push(Attempt {
                        kind,
                        error: err.message(),
                    });
                    return Err(self.fail(attempts, err.message()));
                }
            };
            self.report(Progress::Connecting { kind });

            let transport =
                match build_answer(&mut open.signal, &self.options, offer, ice, &self.cancel).await {
                    Ok(transport) => transport,
                    Err(RoundError::Cancelled) => return Err(self.cancelled()),
                    Err(RoundError::Failed(err)) => {
                        tracing::warn!(attempt = kind.label(), "本轮建连失败：{}", err.message());
                        match self.after_round_failure(&open.discovery, &mut attempts, kind, err) {
                            Some(error) => return Err(error),
                            None => continue,
                        }
                    }
                };

            match handshake(&self.options, &transport, false).await {
                Ok((session, peer)) => {
                    return Ok(finish(
                        transport,
                        session,
                        code,
                        kind,
                        peer,
                        open.signal,
                        self.progress.clone(),
                    ))
                }
                Err(err) => {
                    drop(transport);
                    match self.after_round_failure(&open.discovery, &mut attempts, kind, err) {
                        Some(error) => return Err(error),
                        None => continue,
                    }
                }
            }
        }

        let reason = attempts
            .last()
            .map(|attempt| attempt.error.clone())
            .unwrap_or_else(|| "对端没有再发 offer".to_string());
        Err(self.fail(attempts, reason))
    }

    /// 发起方前段：发现配置、连信令、建会话。
    async fn open_offer(&self) -> Result<Open, ConnectionError> {
        let mut open = self.open_signal().await?;
        open.signal.send(&ClientMsg::Create).await?;
        open.session_code = take_created(&mut open.signal, self.options.peer_timeout).await?;
        self.report(Progress::SessionReady {
            session_code: open.session_code.clone(),
            role: Role::Offerer,
        });
        Ok(open)
    }

    /// 应答方前段：发现配置、连信令、加入会话。
    async fn open_answer(&self) -> Result<Open, ConnectionError> {
        let mut open = self.open_signal().await?;
        let code = normalize_code(&self.options.session_code)?;
        join_session(&mut open, self.options.peer_timeout, &code).await?;
        open.session_code = code.clone();
        self.report(Progress::SessionReady {
            session_code: code,
            role: Role::Answerer,
        });
        Ok(open)
    }

    /// 共用前段：解析基址、发现配置、核对短 ID、连信令。
    async fn open_signal(&self) -> Result<Open, ConnectionError> {
        self.report(Progress::Discovering);
        let endpoint = Endpoint::parse(&self.options.endpoint)
            .map_err(|err| ConnectionError::BadEndpoint(err.message()))?;
        let user_agent = secrelay_relay_client::user_agent();
        let discovery = secrelay_relay_client::discover(&endpoint, &user_agent)
            .await
            .map_err(|err| ConnectionError::Discovery(err.message()))?;

        self.report(Progress::Discovered {
            id_check: discovery.id_check,
            turn_ready: discovery.info.turn_configured,
        });
        if discovery.id_check != IdCheck::Match {
            return Err(ConnectionError::RelayIdMismatch {
                local: discovery.expected_id.clone(),
                advertised: discovery.info.id.clone(),
            });
        }
        if discovery.info.protocol_version > secrelay_relay_client::PROTOCOL_VERSION {
            return Err(ConnectionError::SignalingVersion {
                remote: discovery.info.protocol_version,
                local: secrelay_relay_client::PROTOCOL_VERSION,
            });
        }

        self.report(Progress::Signaling);
        let signal = SignalPump::start(&discovery.signaling_url(), &self.options.peer_id).await?;
        Ok(Open {
            discovery,
            signal,
            session_code: String::new(),
            peer_joined: false,
        })
    }
}

// ────────────────────────────────────────────────────── 连接

/// 一条已建立的连接。
pub struct Connection {
    session: Session,
    transport: Arc<WebRtcTransport>,
    connected: Connected,
    signal: Option<SignalPump>,
    stop: Arc<AtomicBool>,
    publisher: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("connected", &self.connected)
            .field("relayed", &self.transport.is_relayed())
            .finish_non_exhaustive()
    }
}

impl Connection {
    /// 建连成功的详情。
    pub fn connected(&self) -> &Connected {
        &self.connected
    }

    /// 会话码。
    pub fn session_code(&self) -> &str {
        &self.connected.session_code
    }

    /// 这条连接是否经过中继。候选对变化时会跟着变。
    pub fn is_relayed(&self) -> bool {
        self.transport.is_relayed()
    }

    /// 对端信息。
    ///
    /// 握手用的 `Session` 不留到连接上，所以这里用建连时记下的那份。
    pub fn peer(&self) -> Option<&PeerInfo> {
        Some(&self.connected.peer)
    }

    /// 只读的底层传输。
    pub fn transport(&self) -> &dyn Transport {
        self.transport.as_ref()
    }

    /// 会话的可变引用，用来发消息、取事件。
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// 关掉会话与建连期间用的信令连接。幂等。
    pub async fn close(&mut self, reason: &str) -> Result<(), SessionError> {
        eprintln!("[dbg] Connection::close({reason})");
        self.stop.store(true, Ordering::SeqCst);
        self.publisher.abort();
        let result = self.session.close(reason).await;
        if let Some(mut signal) = self.signal.take() {
            signal.close().await;
        }
        result
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        eprintln!("[dbg] Connection::drop");
        self.stop.store(true, Ordering::SeqCst);
        self.publisher.abort();
    }
}

/// 把一轮成功的传输包成 `Connection`，并起后台任务跟踪中继占比。
fn finish(
    transport: Arc<WebRtcTransport>,
    session: Session,
    session_code: String,
    kind: AttemptKind,
    peer: PeerInfo,
    signal: SignalPump,
    progress: watch::Sender<Option<Progress>>,
) -> Connection {
    let connected = Connected {
        session_code,
        kind,
        relayed: transport.is_relayed(),
        peer,
    };

    let stop = Arc::new(AtomicBool::new(false));
    let publisher = tokio::spawn(watch_relay_usage(
        transport.clone(),
        kind,
        connected.relayed,
        progress.clone(),
        stop.clone(),
    ));

    tracing::info!(
        session_code = %connected.session_code,
        kind = kind.label(),
        relayed = connected.relayed,
        peer = %connected.peer.device_id,
        "会话已就绪"
    );
    let _ = progress.send(Some(Progress::Connected {
        kind,
        relayed: connected.relayed,
    }));

    Connection {
        session,
        transport,
        connected,
        signal: Some(signal),
        stop,
        publisher,
    }
}

/// 建连之后继续盯着中继占比的变化。
async fn watch_relay_usage(
    transport: Arc<WebRtcTransport>,
    kind: AttemptKind,
    initial: bool,
    progress: watch::Sender<Option<Progress>>,
    stop: Arc<AtomicBool>,
) {
    let mut last = initial;
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(RELAY_WATCH_INTERVAL).await;
        transport.refresh_pair().await;
        let relayed = transport.is_relayed();
        if relayed != last {
            last = relayed;
            let _ = progress.send(Some(Progress::Connected { kind, relayed }));
        }
    }
}

// ────────────────────────────────────────────────────── 单轮建连

enum RoundError {
    Failed(ConnectionError),
    Cancelled,
}

/// 发起方的一轮：offer → answer → 数据通道打开。
async fn build_offer(
    signal: &mut SignalPump,
    options: &ConnectOptions,
    ice: IceConfig,
    cancel: &AtomicBool,
) -> Result<Arc<WebRtcTransport>, RoundError> {
    let transport = new_transport(&ice, options).await?;
    let offer = transport
        .create_offer()
        .await
        .map_err(|err| RoundError::Failed(ConnectionError::Transport(err.to_string())))?;

    signal
        .send(&ClientMsg::Offer {
            payload: sdp_payload(&offer, "offer"),
        })
        .await
        .map_err(RoundError::Failed)?;

    let answer = match await_answer(signal, options.connect_timeout, cancel).await {
        Ok(Some(answer)) => answer,
        Ok(None) => return Err(RoundError::Cancelled),
        Err(err) => return Err(err),
    };
    transport
        .accept_answer(answer)
        .await
        .map_err(|err| RoundError::Failed(ConnectionError::Transport(err.to_string())))?;

    wait_connected(&transport, options.connect_timeout, cancel).await?;
    Ok(transport)
}

/// 应答方的一轮：接 offer → 回 answer → 数据通道打开。
async fn build_answer(
    signal: &mut SignalPump,
    options: &ConnectOptions,
    offer: String,
    ice: IceConfig,
    cancel: &AtomicBool,
) -> Result<Arc<WebRtcTransport>, RoundError> {
    let transport = new_transport(&ice, options).await?;
    let answer = transport
        .accept_offer(offer)
        .await
        .map_err(|err| RoundError::Failed(ConnectionError::Transport(err.to_string())))?;

    signal
        .send(&ClientMsg::Answer {
            payload: sdp_payload(&answer, SDP_ANSWER),
        })
        .await
        .map_err(RoundError::Failed)?;

    wait_connected(&transport, options.connect_timeout, cancel).await?;
    Ok(transport)
}

async fn new_transport(
    ice: &IceConfig,
    options: &ConnectOptions,
) -> Result<Arc<WebRtcTransport>, RoundError> {
    WebRtcTransport::new_with_bind(ice, options.bind.clone())
        .await
        .map(Arc::new)
        .map_err(|err| RoundError::Failed(ConnectionError::Transport(err.to_string())))
}

/// 等数据通道打开，期间响应取消与总超时。
///
/// 不用切片轮询：`wait_connected` 每次调用都会重算自己的超时，反复调用等于把超时无限续上。
async fn wait_connected(
    transport: &WebRtcTransport,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<(), RoundError> {
    let connected = tokio::select! {
        biased;
        result = transport.wait_connected(timeout) => result,
        _ = wait_cancel(cancel) => return Err(RoundError::Cancelled),
    };
    connected.map_err(|err| RoundError::Failed(ConnectionError::Transport(err.to_string())))
}

/// 取消标志被置上时返回。
async fn wait_cancel(cancel: &AtomicBool) {
    while !cancel.load(Ordering::SeqCst) {
        tokio::time::sleep(CANCEL_POLL_INTERVAL).await;
    }
}

/// 会话握手。
///
/// 握手用的 `Session` 就是随后的那个，不能另建一个：它握着传输层的收件箱。
async fn handshake(
    options: &ConnectOptions,
    transport: &Arc<WebRtcTransport>,
    offerer: bool,
) -> Result<(Session, PeerInfo), ConnectionError> {
    let mut session = Session::new(transport.clone(), session_config(options));
    let peer = if offerer {
        session.connect().await
    } else {
        session.accept().await
    }
    .map_err(|err| ConnectionError::Session(err.to_string()))?;
    Ok((session, peer))
}

fn session_config(options: &ConnectOptions) -> SessionConfig {
    SessionConfig::new(options.device_id.clone())
        .with_capabilities(options.capabilities.clone())
        .with_handshake_timeout(options.handshake_timeout)
}

enum IcePlan {
    All,
    RelayOnly,
}

/// 按策略把中继上报的 ICE 列表转成传输层配置。
fn resolve_ice(discovery: &Discovery, kind: AttemptKind) -> Result<IceConfig, ConnectionError> {
    let servers: Vec<IceServerConfig> = discovery
        .info
        .ice
        .iter()
        .map(|server| IceServerConfig {
            urls: server.urls.clone(),
            username: server.username.clone().unwrap_or_default(),
            credential: server.credential.clone().unwrap_or_default(),
        })
        .collect();

    match kind.plan() {
        IcePlan::All => Ok(IceConfig::from_servers(servers)),
        IcePlan::RelayOnly => {
            let turn: Vec<IceServerConfig> = servers
                .into_iter()
                .filter(|server| {
                    !server.username.is_empty()
                        && server.urls.iter().any(|url| url.starts_with("turn"))
                })
                .collect();
            match can_fallback(discovery) {
                Ok(()) if !turn.is_empty() => {
                    let mut config = IceConfig::from_servers(turn);
                    config.stun_only = true;
                    Ok(config)
                }
                Ok(()) => Err(ConnectionError::NoRelayFallback {
                    direct: "已跳过".to_string(),
                    reason: "中继没有返回带凭据的 TURN 地址".to_string(),
                }),
                Err(reason) => Err(ConnectionError::NoRelayFallback {
                    direct: "已跳过".to_string(),
                    reason,
                }),
            }
        }
    }
}

/// 中继能不能承担回退。
fn can_fallback(discovery: &Discovery) -> Result<(), String> {
    if !discovery.info.turn_configured {
        return Err("中继上报 TURN 未配置".to_string());
    }
    if discovery.info.turn_urls().is_empty() {
        return Err("中继没有返回 TURN 地址".to_string());
    }
    Ok(())
}

/// 会话短码的形态校验：八位十六进制。
fn normalize_code(code: &str) -> Result<String, ConnectionError> {
    let code = code.trim();
    if code.len() == 8 && code.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(code.to_ascii_lowercase())
    } else {
        Err(ConnectionError::BadSessionCode(code.to_string()))
    }
}

// ────────────────────────────────────────────────────── 信令泵

/// 一条常驻的信令连接：一个后台任务负责收，其余代码只跟通道打交道。
///
/// 为什么不让调用方自己读：建连期间既要发 offer/answer 又要收对端的消息，中途还会长时间
/// 停在等数据通道上。服务端只把消息转给**当前在会话里**的连接，读写分离才不会中途掉线。
struct SignalPump {
    sink: Option<SignalSink>,
    events: tokio::sync::mpsc::UnboundedReceiver<ServerMsg>,
    task: tokio::task::JoinHandle<()>,
}

impl SignalPump {
    /// 连上信令并起泵。
    async fn start(url: &str, peer_id: &str) -> Result<Self, ConnectionError> {
        let socket = SignalSocket::connect(url, peer_id)
            .await
            .map_err(|err| ConnectionError::Signaling(err.message()))?;
        let (sink, stream) = socket.split();
        let (tx, events) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(pump_messages(stream, tx));
        Ok(Self {
            sink: Some(sink),
            events,
            task,
        })
    }

    async fn send(&mut self, msg: &ClientMsg) -> Result<(), ConnectionError> {
        let sink = self
            .sink
            .as_mut()
            .ok_or_else(|| ConnectionError::Signaling("信令连接已关闭".to_string()))?;
        sink.send(msg)
            .await
            .map_err(|err| ConnectionError::Signaling(err.message()))
    }

    /// 等一条消息；到点返回 `None`，连接没了返回错误。
    async fn next(&mut self, timeout: Duration) -> Result<Option<ServerMsg>, ConnectionError> {
        match tokio::time::timeout(timeout, self.events.recv()).await {
            Ok(Some(msg)) => Ok(Some(msg)),
            Ok(None) => Err(ConnectionError::Signaling("信令连接被关闭".to_string())),
            Err(_) => Ok(None),
        }
    }

    async fn close(&mut self) {
        if let Some(sink) = self.sink.take() {
            if let Err(err) = sink.close().await {
                tracing::debug!("关闭信令连接失败：{err}");
            }
        }
        self.task.abort();
    }
}

impl Drop for SignalPump {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 把信令消息倒进通道，直到连接结束。
async fn pump_messages(
    mut stream: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    sink: tokio::sync::mpsc::UnboundedSender<ServerMsg>,
) {
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<ServerMsg>(&text) {
                Ok(msg) => {
                    if sink.send(msg).is_err() {
                        break;
                    }
                }
                Err(err) => tracing::debug!("信令消息无法解析，已忽略：{err}"),
            },
            Some(Ok(Message::Close(_))) | None => break,
            Some(Ok(_)) => continue,
            Some(Err(err)) => {
                tracing::debug!("信令读取失败：{err}");
                break;
            }
        }
    }
}

/// 建连前段的结果：一条常驻信令 + 发现结果 + 会话码。
struct Open {
    discovery: Discovery,
    signal: SignalPump,
    session_code: String,
    /// 对端是否已经在会话里。
    peer_joined: bool,
}

/// 发 `create` 并读出服务端给的会话码。
async fn take_created(
    signal: &mut SignalPump,
    timeout: Duration,
) -> Result<String, ConnectionError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(ConnectionError::Signaling("等待会话码超时".to_string()));
        }
        match signal.next(left).await? {
            None => continue,
            Some(ServerMsg::Created { session_id }) => return Ok(session_id),
            Some(ServerMsg::Hello { .. }) => continue,
            Some(ServerMsg::Error { code, message }) => {
                return Err(ConnectionError::Signaling(format!("{code}：{message}")))
            }
            Some(other) => {
                tracing::debug!("建会话时收到无关消息：{other:?}");
                continue;
            }
        }
    }
}

/// 发 `join` 并等 `joined` 确认。
async fn join_session(
    open: &mut Open,
    timeout: Duration,
    code: &str,
) -> Result<(), ConnectionError> {
    open.signal
        .send(&ClientMsg::Join {
            session_id: code.to_string(),
        })
        .await?;

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(ConnectionError::PeerJoinTimeout(timeout));
        }
        match open.signal.next(left).await? {
            None => continue,
            Some(ServerMsg::Joined { .. }) => {
                open.peer_joined = true;
                return Ok(());
            }
            Some(ServerMsg::Hello { .. }) => continue,
            Some(ServerMsg::Error { code, message }) => {
                return Err(ConnectionError::Signaling(format!("{code}：{message}")))
            }
            Some(other) => {
                tracing::debug!("加入会话时收到无关消息：{other:?}");
                continue;
            }
        }
    }
}

/// 发起方等对端进入会话。
async fn wait_peer_join(
    open: &mut Open,
    options: &ConnectOptions,
    progress: &watch::Sender<Option<Progress>>,
    cancel: &AtomicBool,
) -> Result<(), ConnectionError> {
    let deadline = tokio::time::Instant::now() + options.peer_timeout;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(ConnectionError::PeerJoinTimeout(options.peer_timeout));
        }
        match open.signal.next(left.min(CANCEL_POLL_INTERVAL)).await? {
            None => continue,
            Some(ServerMsg::PeerJoined { peer_id }) => {
                let _ = progress.send(Some(Progress::PeerJoined { peer_id }));
                open.peer_joined = true;
                return Ok(());
            }
            Some(ServerMsg::Hello { .. }) => continue,
            Some(ServerMsg::Error { code, message }) => {
                return Err(ConnectionError::Signaling(format!("{code}：{message}")))
            }
            Some(other) => {
                tracing::debug!("等对端加入时收到无关消息：{other:?}");
                continue;
            }
        }
    }
}

/// 应答方等一次 offer；`rejoin` 为真时先重新加入会话。
async fn await_offer(
    open: &mut Open,
    options: &ConnectOptions,
    rejoin: bool,
    progress: &watch::Sender<Option<Progress>>,
    cancel: &AtomicBool,
) -> Result<Option<String>, ConnectionError> {
    if rejoin {
        // 第二轮：会话可能已经被服务端回收，重新加入一次。
        let code = normalize_code(&options.session_code)?;
        open.peer_joined = false;
        if let Err(err) = join_session(open, options.peer_timeout, &code).await {
            tracing::debug!("第二轮重新加入会话失败：{err}");
        }
    }

    let deadline = tokio::time::Instant::now() + options.connect_timeout;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(ConnectionError::OfferTimeout(options.connect_timeout));
        }
        match open.signal.next(left.min(CANCEL_POLL_INTERVAL)).await? {
            None => continue,
            Some(ServerMsg::Offer { payload, .. }) => return sdp_from_payload(&payload).map(Some),
            Some(ServerMsg::Hello { .. }) => continue,
            Some(ServerMsg::PeerJoined { peer_id }) => {
                let _ = progress.send(Some(Progress::PeerJoined { peer_id }));
                open.peer_joined = true;
                continue;
            }
            Some(ServerMsg::PeerLeft { .. }) => {
                return Err(ConnectionError::Signaling("对端已离开会话".to_string()))
            }
            Some(ServerMsg::Error { code, message }) => {
                return Err(ConnectionError::Signaling(format!("{code}：{message}")))
            }
            Some(other) => {
                tracing::debug!("等 offer 时收到无关消息：{other:?}");
                continue;
            }
        }
    }
}

/// 发起方等 answer。
async fn await_answer(
    signal: &mut SignalPump,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<Option<String>, RoundError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(RoundError::Failed(ConnectionError::AnswerTimeout(timeout)));
        }
        match signal.next(left.min(CANCEL_POLL_INTERVAL)).await {
            Err(err) => return Err(RoundError::Failed(err)),
            Ok(None) => continue,
            Ok(Some(ServerMsg::Answer { payload, .. })) => {
                return sdp_from_payload(&payload)
                    .map(Some)
                    .map_err(RoundError::Failed)
            }
            Ok(Some(ServerMsg::Error { code, message })) => {
                return Err(RoundError::Failed(ConnectionError::Signaling(format!(
                    "{code}：{message}"
                ))))
            }
            Ok(Some(_)) => continue,
        }
    }
}

fn sdp_payload(sdp: &str, kind: &str) -> Value {
    json!({ SDP_TYPE: kind, SDP_VALUE: sdp })
}

fn sdp_from_payload(payload: &Value) -> Result<String, ConnectionError> {
    payload
        .get(SDP_VALUE)
        .and_then(Value::as_str)
        .filter(|sdp| !sdp.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| ConnectionError::BadSdp(payload.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrelay_relay_client::RelayInfo;

    fn discovery(raw: &str) -> Discovery {
        let endpoint = Endpoint::parse("https://relay.example.com").unwrap();
        Discovery {
            expected_id: endpoint.local_id(),
            endpoint,
            info: serde_json::from_str::<RelayInfo>(raw).unwrap(),
            id_check: IdCheck::Match,
        }
    }

    const FULL: &str = r#"{
        "id":"AF4KR6IMPE","protocol_version":1,
        "signaling":"wss://relay.example.com/ws/signal",
        "ice":[{"urls":["turn:relay.example.com:3478?transport=udp"],"username":"u","credential":"c"},
               {"urls":["stun:stun.example.com:3478"]}],
        "turn_configured":true
    }"#;

    #[test]
    fn 会话码必须是八位十六进制() {
        assert_eq!(normalize_code("ab12cd34").unwrap(), "ab12cd34");
        assert_eq!(normalize_code("  AB12CD34 ").unwrap(), "ab12cd34");
        for bad in ["", "abc", "abcd1234z", "1234567890"] {
            assert!(normalize_code(bad).is_err(), "{bad} 应当被拒");
        }
    }

    #[test]
    fn 直连策略带上所有_ice_服务器() {
        let config = resolve_ice(&discovery(FULL), AttemptKind::Direct).unwrap();
        assert_eq!(config.ice.len(), 2);
        assert!(!config.stun_only, "直连不该强制中继");
    }

    #[test]
    fn 中继回退只留带凭据的_turn() {
        let config = resolve_ice(&discovery(FULL), AttemptKind::RelayOnly).unwrap();
        assert_eq!(config.ice.len(), 1);
        assert!(config.ice[0].urls[0].starts_with("turn:"));
        assert_eq!(config.ice[0].username, "u");
        assert!(config.stun_only, "回退时只能收集中继候选");
    }

    #[test]
    fn 没有_turn_时回退路径直接报错() {
        let stun_only = discovery(
            r#"{"ice":[{"urls":["stun:stun.example.com:3478"]}],"turn_configured":false}"#,
        );
        assert!(can_fallback(&stun_only).is_err());
        let err = resolve_ice(&stun_only, AttemptKind::RelayOnly).unwrap_err();
        assert!(matches!(err, ConnectionError::NoRelayFallback { .. }));

        // 声明 TURN 可用但列表里没有 TURN 地址：同样拒绝
        let lying = discovery(r#"{"ice":[{"urls":["stun:x:3478"]}],"turn_configured":true}"#);
        assert!(resolve_ice(&lying, AttemptKind::RelayOnly).is_err());
    }

    #[test]
    fn 角色由会话码决定() {
        assert_eq!(ConnectOptions::new("https://a", "p").role(), Role::Offerer);
        assert_eq!(
            ConnectOptions::new("https://a", "p").joining("ab12cd34").role(),
            Role::Answerer
        );
        assert_eq!(
            ConnectOptions::new("https://a", "p")
                .joining("ab12cd34")
                .as_offerer()
                .role(),
            Role::Offerer
        );
    }

    #[test]
    fn sdp_载荷往返() {
        let payload = sdp_payload("v=0\r\n", "offer");
        assert_eq!(payload[SDP_TYPE], "offer");
        assert_eq!(sdp_from_payload(&payload).unwrap(), "v=0\r\n");

        assert!(sdp_from_payload(&json!({})).is_err());
        assert!(sdp_from_payload(&json!({ "sdp": "  " })).is_err());
        assert!(sdp_from_payload(&json!({ "sdp": 7 })).is_err());
    }

    #[test]
    fn 失败汇总把每一轮都写进去() {
        let error = ConnectError::Failed(Failure::from_attempts(
            vec![
                Attempt {
                    kind: AttemptKind::Direct,
                    error: "超时".into(),
                },
                Attempt {
                    kind: AttemptKind::RelayOnly,
                    error: "没有 TURN".into(),
                },
            ],
            "汇总".into(),
        ));
        assert!(!error.reason().is_empty());
        assert!(ConnectError::Cancelled.is_cancelled());
        assert!(!error.is_cancelled());
    }

    #[test]
    fn 进度订阅端能读到最新值() {
        let connector = Connector::new(ConnectOptions::new("https://a", "p"));
        let feed = connector.subscribe();
        assert!(feed.snapshot().is_none());
        connector.report(Progress::Discovering);
        assert_eq!(feed.snapshot(), Some(Progress::Discovering));
    }
}
