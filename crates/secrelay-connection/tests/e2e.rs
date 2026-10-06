//! 端到端：本机起一个假信令中继，用真实 HTTP / WebSocket / ICE / DTLS / SCTP
//! 跑一遍完整建连，并在建好的连接上收发一条消息。
//!
//! 假中继只实现本项目用到的那部分：`/healthz`、`/api/v1/relay`、`/ws/signal`。
//! 服务端那半边的 WebSocket 是手写的（握手 + 帧），省掉一个服务端依赖。
//!
//! **范围**：两端都在本机，候选是 host 直连；跨机打洞不在这里覆盖。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use secrelay_connection::{ConnectError, ConnectOptions, Connector, Progress};
use secrelay_protocol::{ControlMessage, DeviceId};
use secrelay_relay_client::Endpoint;
use secrelay_session::SessionEvent;
use secrelay_transport::ice::parse_bind_addr;
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc as tokio_mpsc;

/// 假信令中继。
struct FakeRelay {
    listener: TcpListener,
    /// 不转发 offer/answer，用来验证失败路径。
    drop_sdp: bool,
    /// 只上报 STUN，用来验证没有凭据时不能回退。
    stun_only: bool,
    /// 上报的 ICE 列表为空，两端只剩 host 候选。
    host_only: bool,
}

#[derive(Clone)]
struct PeerHandle {
    tx: tokio_mpsc::UnboundedSender<Value>,
}

#[derive(Default)]
struct Hub {
    next: u64,
    peers: HashMap<u64, PeerHandle>,
    peer_ids: HashMap<u64, String>,
    sessions: HashMap<String, Vec<u64>>,
}

type SharedHub = Arc<Mutex<Hub>>;

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

impl FakeRelay {
    /// 中继上报的 ICE 列表。
    ///
    /// 本机两端的用例只留 host 候选：这条链路要证的是"发现 → 信令 → SDP 交换 → ICE
    /// → 握手"，不是 STUN/TURN 本身，候选收集越快越好。
    fn ice(&self) -> Value {
        if self.host_only {
            return json!([]);
        }
        if self.stun_only {
            return json!([{"urls": ["stun:127.0.0.1:1"]}]);
        }
        json!([
            {"urls": ["turn:127.0.0.1:1?transport=udp"], "username": "u", "credential": "c"},
            {"urls": ["stun:127.0.0.1:1"]}
        ])
    }

    /// 带凭据的 TURN 存在时才算"可回退"。
    fn turn_configured(&self) -> bool {
        !self.host_only && !self.stun_only
    }

    /// 起服务并返回基址。
    async fn serve(self, hub: SharedHub) -> String {
        let addr = self.listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let signaling = format!("ws://{addr}/ws/signal");
        let ice = self.ice();
        let turn_configured = self.turn_configured();
        let drop_sdp = self.drop_sdp;

        tokio::spawn(async move {
            while let Ok((stream, _)) = self.listener.accept().await {
                let hub = hub.clone();
                let signaling = signaling.clone();
                let ice = ice.clone();
                tokio::spawn(async move {
                    serve_http(stream, hub, signaling, ice, turn_configured, drop_sdp).await;
                });
            }
        });

        base
    }
}

/// 逐字节读一次 HTTP 请求头，**读到 `\r\n\r\n` 就停**。
///
/// 不能多读：升级请求之后紧跟着的就是 WebSocket 帧，多读一个字节都会把帧流读歪。
async fn read_head(stream: &mut TcpStream) -> Option<String> {
    use tokio::io::AsyncReadExt;

    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        if stream.read_exact(&mut byte).await.is_err() {
            return None;
        }
        raw.push(byte[0]);
        if raw.len() > 64 * 1024 {
            return None;
        }
    }
    Some(String::from_utf8_lossy(&raw).to_string())
}

async fn serve_http(
    mut stream: TcpStream,
    hub: SharedHub,
    signaling: String,
    ice: Value,
    turn_configured: bool,
    drop_sdp: bool,
) {
    let Some(request) = read_head(&mut stream).await else {
        return;
    };
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();

    if path == "/ws/signal" {
        serve_signal(stream, &request, hub, drop_sdp).await;
        return;
    }

    let body = match path.as_str() {
        "/api/v1/relay" => {
            let origin = format!("http://{}", stream.local_addr().unwrap());
            json!({
                "id": Endpoint::parse(&origin).unwrap().local_id(),
                "version": "0.1.0",
                "protocol_version": 1,
                "signaling": signaling,
                "ice": ice,
                "credential_ttl": 600,
                "realm": "secrelay.relay",
                "turn_configured": turn_configured,
            })
            .to_string()
        }
        "/healthz" => json!({
            "status": "ok",
            "protocol_version": 1,
            "turn_configured": turn_configured,
        })
        .to_string(),
        _ => {
            use tokio::io::AsyncWriteExt;
            let _ = stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            return;
        }
    };

    use tokio::io::AsyncWriteExt;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// 按 RFC 6455 回一个 101，之后按手写的帧编解码收发。
async fn serve_signal(mut stream: TcpStream, request: &str, hub: SharedHub, drop_sdp: bool) {
    use tokio::io::AsyncWriteExt;

    let Some(key) = request.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("sec-websocket-key")
            .then(|| value.trim().to_string())
    }) else {
        return;
    };

    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(&key)
    );
    if stream.write_all(response.as_bytes()).await.is_err() {
        return;
    }

    handle_socket(stream, hub, drop_sdp).await;
}

fn accept_key(key: &str) -> String {
    use sha1::{Digest, Sha1};
    let digest = Sha1::digest(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes());
    base64(&digest)
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

async fn handle_socket(socket: TcpStream, hub: SharedHub, drop_sdp: bool) {
    let (mut reader, mut writer) = socket.into_split();
    let (tx, mut rx) = tokio_mpsc::unbounded_channel::<Value>();

    let conn = {
        let mut hub = hub.lock().unwrap();
        hub.next += 1;
        let conn = hub.next;
        hub.peers.insert(conn, PeerHandle { tx });
        conn
    };

    send_to(&hub, conn, json!({"type": "hello", "protocol_version": 1}));

    // 收：客户端发来的帧一定是掩码的
    let reading = {
        let hub = hub.clone();
        tokio::spawn(async move {
            while let Some(text) = read_text_frame(&mut reader).await {
                let Ok(msg) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if !handle_msg(&hub, conn, &msg, drop_sdp) {
                    break;
                }
            }
            disconnect(&hub, conn);
        })
    };

    // 发：把队列里的 JSON 编成不掩码的文本帧
    while let Some(value) = rx.recv().await {
        if write_text_frame(&mut writer, &value.to_string())
            .await
            .is_err()
        {
            break;
        }
    }

    reading.abort();
    disconnect(&hub, conn);
}

fn disconnect(hub: &SharedHub, conn: u64) {
    let (peer_id, members) = {
        let mut hub = hub.lock().unwrap();
        if hub.peers.remove(&conn).is_none() {
            return;
        }
        let peer_id = hub.peer_ids.remove(&conn).unwrap_or_default();
        let mut members = Vec::new();
        for list in hub.sessions.values_mut() {
            if list.contains(&conn) {
                list.retain(|id| *id != conn);
                members.extend(list.iter().copied());
            }
        }
        (peer_id, members)
    };
    for id in members {
        send_to(hub, id, json!({"type": "peer_left", "peer_id": peer_id}));
    }
}

/// 读一个帧，返回文本内容；连接结束或非文本帧返回 `None`。
async fn read_text_frame<R>(stream: &mut R) -> Option<String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await.ok()?;
    let opcode = head[0] & 0x0F;
    let masked = head[1] & 0x80 != 0;
    let mut length = u64::from(head[1] & 0x7F);
    if length == 126 {
        let mut extended = [0u8; 2];
        stream.read_exact(&mut extended).await.ok()?;
        length = u64::from(u16::from_be_bytes(extended));
    } else if length == 127 {
        let mut extended = [0u8; 8];
        stream.read_exact(&mut extended).await.ok()?;
        length = u64::from_be_bytes(extended);
    }
    let mut mask = [0u8; 4];
    if masked {
        stream.read_exact(&mut mask).await.ok()?;
    }
    let mut payload = vec![0u8; length as usize];
    stream.read_exact(&mut payload).await.ok()?;
    if masked {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
    }

    match opcode {
        0x1 => String::from_utf8(payload).ok(),
        // ping / pong / 二进制一律忽略，继续读
        0x8 => None,
        _ => Some(String::new()),
    }
}

/// 写一个不掩码的文本帧。
async fn write_text_frame<W>(stream: &mut W, text: &str) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;

    let payload = text.as_bytes();
    let mut frame = vec![0x81];
    if payload.len() < 126 {
        frame.push(payload.len() as u8);
    } else if payload.len() <= u16::MAX as usize {
        frame.push(126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame).await?;
    stream.flush().await
}

/// 返回是否继续处理这条连接。
fn handle_msg(hub: &SharedHub, conn: u64, msg: &Value, drop_sdp: bool) -> bool {
    match msg["type"].as_str() {
        Some("hello") => {
            let peer_id = msg["peer_id"].as_str().unwrap_or_default().to_string();
            hub.lock().unwrap().peer_ids.insert(conn, peer_id);
            true
        }
        Some("create") => {
            let index = NEXT_SESSION.fetch_add(1, Ordering::SeqCst);
            let session_id = format!("{index:08x}");
            hub.lock()
                .unwrap()
                .sessions
                .insert(session_id.clone(), vec![conn]);
            send_to(
                hub,
                conn,
                json!({"type": "created", "session_id": session_id}),
            );
            true
        }
        Some("join") => {
            let session_id = msg["session_id"].as_str().unwrap_or_default().to_string();
            let outcome = {
                let mut hub = hub.lock().unwrap();
                match hub.sessions.get_mut(&session_id) {
                    Some(members) if !members.contains(&conn) && members.len() < 2 => {
                        members.push(conn);
                        Ok(members.clone())
                    }
                    Some(_) => Err("session_full"),
                    None => Err("join_failed"),
                }
            };
            let members = match outcome {
                Ok(members) => members,
                Err(code) => {
                    let message = if code == "session_full" {
                        "会话已满"
                    } else {
                        "会话不存在"
                    };
                    send_to(
                        hub,
                        conn,
                        json!({"type": "error", "code": code, "message": message}),
                    );
                    return true;
                }
            };
            send_to(
                hub,
                conn,
                json!({"type": "joined", "session_id": session_id}),
            );
            let peer_id = hub
                .lock()
                .unwrap()
                .peer_ids
                .get(&conn)
                .cloned()
                .unwrap_or_default();
            for id in members {
                if id != conn {
                    send_to(hub, id, json!({"type": "peer_joined", "peer_id": peer_id}));
                }
            }
            true
        }
        Some(kind @ ("offer" | "answer" | "candidate")) if !drop_sdp => {
            let targets = {
                let hub = hub.lock().unwrap();
                let session_id = hub
                    .sessions
                    .iter()
                    .find(|(_, members)| members.contains(&conn))
                    .map(|(id, _)| id.clone());
                let Some(session_id) = session_id else {
                    return true;
                };
                let peer_id = hub.peer_ids.get(&conn).cloned().unwrap_or_default();
                let ids: Vec<u64> = hub
                    .sessions
                    .get(&session_id)
                    .map(|members| {
                        members
                            .iter()
                            .copied()
                            .filter(|id| *id != conn)
                            .collect()
                    })
                    .unwrap_or_default();
                ids.into_iter()
                    .filter_map(|id| hub.peers.get(&id).map(|peer| (peer.tx.clone(), peer_id.clone())))
                    .collect::<Vec<_>>()
            };
            for (tx, peer_id) in targets {
                let _ = tx.send(json!({
                    "type": kind,
                    "from": peer_id,
                    "payload": msg["payload"],
                }));
            }
            true
        }
        // 故意不转发 SDP：两端都等不到 answer，用来验证失败路径
        Some("offer" | "answer" | "candidate") => true,
        Some("bye") => false,
        _ => {
            send_to(
                hub,
                conn,
                json!({"type": "error", "code": "bad_message", "message": "不支持"}),
            );
            true
        }
    }
}

fn send_to(hub: &SharedHub, conn: u64, value: Value) {
    let tx = hub.lock().unwrap().peers.get(&conn).map(|p| p.tx.clone());
    if let Some(tx) = tx {
        let _ = tx.send(value);
    }
}

// ────────────────────────────────────────── 测试外壳

async fn start_relay(drop_sdp: bool, stun_only: bool, host_only: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    FakeRelay {
        listener,
        drop_sdp,
        stun_only,
        host_only,
    }
    .serve(Arc::new(Mutex::new(Hub::default())))
    .await
}

/// 两端都关在本机回环上，超时压到几秒，好让失败路径也能被测到。
fn options(base: &str, peer_id: &str) -> ConnectOptions {
    ConnectOptions::new(base, peer_id)
        .with_device_id(DeviceId::new(peer_id).unwrap())
        .with_bind(vec!["127.0.0.1:0".to_string()])
        .with_timeouts(
            Duration::from_secs(8),
            Duration::from_secs(8),
            Duration::from_secs(8),
        )
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
}

/// 一条连接的结局，线程之间搬得动。
type Outcome = Result<secrelay_connection::Connection, ConnectError>;

/// 建连期间用的运行时必须是**长命**的。
///
/// 传输层在创建时把 webrtc 的驱动挂到当时的运行时上（进程内只有一份），
/// 运行时就地 drop 会让数据通道的收发一起停掉：`send` 仍然返回成功，
/// 对端却什么都收不到。所以两端共用这一份，由调用方一直拿着。
struct Harness {
    runtime: tokio::runtime::Runtime,
}

impl Harness {
    fn new() -> Self {
        Self {
            runtime: runtime(),
        }
    }

    fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.runtime
    }
}

/// 应答方在自己的线程里跑，等会话码送过来再动手。
///
/// 它用的是全局那一份运行时：进程里只有一份 webrtc 驱动，两端必须共用。
fn answerer_thread<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    runtime: &'scope tokio::runtime::Runtime,
    base: &str,
    code_rx: std::sync::mpsc::Receiver<String>,
) -> std::thread::ScopedJoinHandle<'scope, Outcome> {
    let base = base.to_string();
    scope.spawn(move || {
        let code = code_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("应当收到发起方给出的会话码");
        runtime.block_on(async move {
            Connector::new(options(&base, "peer-answer").joining(code))
                .connect_answer()
                .await
        })
    })
}

/// 从进度流里取出发起方建好的会话码，转手给应答方。
async fn offer_and_hand_over_code(base: &str, code_tx: std::sync::mpsc::Sender<String>) -> Outcome {
    let connector = Connector::new(options(base, "peer-offer").as_offerer());
    let mut feed = connector.subscribe();
    let connect = connector.connect_offer();
    let watch = async {
        while let Some(progress) = feed.changed().await {
            match progress {
                Progress::SessionReady { session_code, .. } => {
                    let _ = code_tx.send(session_code);
                }
                Progress::Connected { .. } | Progress::Failed { .. } | Progress::Cancelled => break,
                _ => {}
            }
        }
    };
    let (result, ()) = tokio::join!(connect, watch);
    result
}

/// 两端一起建连，返回各自的结果。
///
/// 发起方在 `harness` 的运行时上跑，应答方在自己线程里用**同一份**运行时；
/// 用一个作用域把两边都圈住，保证运行时比两端都活得久。
fn connect_pair(harness: &Harness, base: &str) -> (Outcome, Outcome) {
    let (code_tx, code_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let answerer = answerer_thread(scope, harness.runtime(), base, code_rx);
        let offerer = harness.block_on(offer_and_hand_over_code(base, code_tx));
        (offerer, answerer.join().expect("应答方线程不应 panic"))
    })
}

// ────────────────────────────────────────── 用例

#[test]
fn 本机两端走完整链路建连并能收发() {
    let harness = Harness::new();
    let base = harness.block_on(start_relay(false, false, false));
    let (offerer, answerer) = connect_pair(&harness, &base);

    let mut offerer = offerer.expect("发起方应当连上");
    let mut answerer = answerer.expect("应答方应当连上");
    assert_eq!(offerer.session_code().len(), 8, "会话码应当是 8 位短码");
    assert_eq!(offerer.session_code(), answerer.session_code());
    assert_eq!(offerer.peer().unwrap().device_id.as_str(), "peer-answer");
    assert_eq!(answerer.peer().unwrap().device_id.as_str(), "peer-offer");
    assert!(
        !offerer.is_relayed(),
        "本机回环应当是 host 候选，不该被记成中继"
    );
    assert!(!answerer.is_relayed());
    assert_eq!(
        offerer.connected().kind.label(),
        "直连尝试",
        "本机回环应当第一轮就成"
    );

    harness.block_on(async {
        offerer
            .session_mut()
            .send_control(ControlMessage::Text {
                body: "本机两进程".to_string(),
            })
            .await
            .unwrap();
        let event =
            tokio::time::timeout(Duration::from_secs(10), answerer.session_mut().next_event())
                .await
                .expect("应当在超时前收到消息")
                .unwrap();
        match event {
            SessionEvent::Control(ControlMessage::Text { body }) => assert_eq!(body, "本机两进程"),
            other => panic!("期望文字消息，收到 {other:?}"),
        }
        // 1 条是握手时的 Hello，1 条是刚才那条文字
        assert_eq!(offerer.transport().stats().frames_sent, 2);
        // 1 条是 Hello，1 条是文字
        assert_eq!(answerer.transport().stats().frames_received, 2);
        // 应答方只发过 HelloAck
        assert_eq!(answerer.transport().stats().frames_sent, 1);

        offerer.close("测试结束").await.unwrap();
        answerer.close("测试结束").await.unwrap();
    });
}

#[test]
fn 对端不回_answer_时两轮都留下记录且原因都写进汇总() {
    let harness = Harness::new();
    // 不转发 SDP：两端都建不起数据通道，直连与中继两轮都要留下记录
    let base = harness.block_on(start_relay(true, false, false));
    let (offerer, answerer) = connect_pair(&harness, &base);
    let _ = answerer;

    let error = offerer.expect_err("对端不回 answer，应当失败");
    let ConnectError::Failed(failure) = error else {
        panic!("不该是被取消");
    };
    assert_eq!(failure.attempts.len(), 2, "直连与中继各留一条：{failure:?}");
    assert_eq!(failure.attempts[0].kind.label(), "直连尝试");
    assert_eq!(failure.attempts[1].kind.label(), "中继回退");
    for attempt in &failure.attempts {
        assert!(
            failure.reason.contains(&attempt.error),
            "每一轮的原因都要进汇总：{} 里没有 {}",
            failure.reason,
            attempt.error
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 会话码不合法时立刻失败() {
    let base = start_relay(false, false, false).await;
    let connector = Connector::new(options(&base, "peer").joining("不是短码"));
    let error = connector
        .connect_answer()
        .await
        .expect_err("会话码不合法应当失败");
    assert!(
        error.reason().contains("十六进制"),
        "原因应当点明会话码：{}",
        error.reason()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 中继连不上时报错而不是假成功() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);

    let connector = Connector::new(
        ConnectOptions::new(&base, "peer")
            .as_offerer()
            .with_timeouts(
                Duration::from_millis(500),
                Duration::from_millis(500),
                Duration::from_millis(500),
            ),
    );
    let error = connector
        .connect_offer()
        .await
        .expect_err("应当失败");
    assert!(
        error.reason().contains("配置") || error.reason().contains("HTTP"),
        "原因应当是中继不可达：{}",
        error.reason()
    );
}

#[test]
fn 取消后立刻返回取消而不是等超时() {
    let harness = Harness::new();
    let base = harness.block_on(start_relay(false, false, false));

    harness.block_on(async move {
        let connector = Arc::new(Connector::new(
            ConnectOptions::new(&base, "peer-offer")
                .as_offerer()
                .with_bind(vec!["127.0.0.1:0".to_string()])
                .with_timeouts(
                    Duration::from_secs(60),
                    Duration::from_secs(60),
                    Duration::from_secs(60),
                ),
        ));
        let mut feed = connector.subscribe();
        let canceller = connector.clone();
        let handle = tokio::spawn(async move { canceller.connect_offer().await });

        // 等到会话建好、进入"等对端"的阶段再取消
        let started = tokio::time::Instant::now();
        while let Some(progress) = feed.changed().await {
            if matches!(progress, Progress::SessionReady { .. }) {
                break;
            }
        }
        connector.cancel();

        let result = tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("取消后应当立刻返回")
            .unwrap();
        assert!(matches!(result, Err(error) if error.is_cancelled()));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "取消不该等到超时"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 只给_stun_时中继回退没有凭据可用() {
    let base = start_relay(false, true, false).await;

    let endpoint = Endpoint::parse(&base).unwrap();
    let discovery = secrelay_relay_client::discover(&endpoint, "SecRelay/test")
        .await
        .unwrap();
    assert!(!discovery.info.turn_configured);
    assert!(discovery.info.turn_urls().is_empty());
    assert_eq!(discovery.info.stun_urls().len(), 1);
}

#[test]
fn 绑定地址常量仍然合法() {
    for addr in secrelay_transport::ice::default_udp_addrs() {
        assert!(parse_bind_addr(&addr).is_ok(), "{addr}");
    }
}






