//! 端到端：本机起一个假中继，用真实 HTTP 与 WebSocket 跑一遍发现、探活、短 ID 核对与信令。
//!
//! 假中继只实现本项目用到的那部分协议：`/healthz`、`/api/v1/relay`、`/ws/signal`，
//! 信令回 `hello` / `created` / `joined`，`offer` 打上 `from` 后回给客户端。
//!
//! 服务端这半边是手写的（握手 + 帧），为的是不在测试里再引一个 WebSocket 服务端依赖。

use std::time::Duration;

use secrelay_relay_client::{
    discover, health, ClientMsg, Endpoint, IdCheck, RelayInfo, ServerMsg, SignalEvent, SignalSocket,
};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// 假中继的配置。
#[derive(Clone)]
struct FakeRelay {
    /// `/api/v1/relay` 里上报的短 ID，改掉它就能模拟"ID 不符"。
    advertised_id: String,
    turn_configured: bool,
    /// 所有请求都回 404，用来模拟探活/发现失败。
    offline: bool,
}

impl FakeRelay {
    fn ice(&self) -> Value {
        let mut ice = vec![json!({"urls": ["stun:stun.example.com:3478"]})];
        if self.turn_configured {
            ice.insert(
                0,
                json!({
                    "urls": ["turn:relay.example.com:3478?transport=udp"],
                    "username": "1:secrelay",
                    "credential": "abc",
                }),
            );
        }
        Value::Array(ice)
    }

    fn relay_payload(&self, addr: std::net::SocketAddr) -> Value {
        json!({
            "id": self.advertised_id,
            "version": "0.1.0",
            "protocol_version": 1,
            "signaling": format!("ws://{addr}/ws/signal"),
            "ice": self.ice(),
            "credential_ttl": 600,
            "realm": "secrelay.relay",
            "turn_configured": self.turn_configured,
        })
    }

    fn health_payload(&self) -> Value {
        json!({
            "status": if self.turn_configured { "ok" } else { "degraded" },
            "version": "0.1.0",
            "protocol_version": 1,
            "turn_configured": self.turn_configured,
            "credential_mode": "ephemeral",
            "active_signaling_connections": 0,
            "active_sessions": 0,
            "peers_over_quota": 0,
        })
    }
}

/// 起一个假中继。`advertised` 为 `None` 时按真实基址算出正确的短 ID。
async fn start_relay(
    advertised: Option<&str>,
    turn_configured: bool,
    offline: bool,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let advertised_id = advertised
        .map(str::to_string)
        .unwrap_or_else(|| Endpoint::parse(&base).unwrap().local_id());

    let relay = FakeRelay {
        advertised_id,
        turn_configured,
        offline,
    };

    let handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let relay = relay.clone();
            tokio::spawn(serve(stream, relay, addr));
        }
    });

    (base, handle)
}

async fn serve(mut stream: TcpStream, relay: FakeRelay, addr: std::net::SocketAddr) {
    let mut buffer = vec![0u8; 4096];
    let Ok(read) = stream.read(&mut buffer).await else {
        return;
    };
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();

    if !relay.offline && path == "/ws/signal" {
        serve_signaling(stream, &request).await;
        return;
    }

    let body = if relay.offline {
        None
    } else {
        match path.as_str() {
            "/healthz" => Some(relay.health_payload().to_string()),
            "/api/v1/relay" => Some(relay.relay_payload(addr).to_string()),
            _ => None,
        }
    };

    let response = match body {
        Some(body) => format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
    };
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

// ────────────────────────────────────────── 手写的服务端 WebSocket

/// 完成握手，然后按消息回。
async fn serve_signaling(mut stream: TcpStream, request: &str) {
    let key = request
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("sec-websocket-key")
                .then(|| value.trim().to_string())
        })
        .unwrap_or_default();

    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(&key)
    );
    if stream.write_all(response.as_bytes()).await.is_err() {
        return;
    }

    send_text(
        &mut stream,
        &json!({"type": "hello", "protocol_version": 1}).to_string(),
    )
    .await;

    while let Some(payload) = read_text(&mut stream).await {
        let Ok(msg) = serde_json::from_str::<Value>(&payload) else {
            continue;
        };
        let reply = match msg["type"].as_str() {
            Some("hello") => None,
            Some("create") => Some(json!({"type": "created", "session_id": "ab12cd34"})),
            Some("join") => Some(json!({"type": "joined", "session_id": msg["session_id"]})),
            Some("offer") => Some(json!({
                "type": "offer",
                "from": "peer-b",
                "payload": msg["payload"],
            })),
            Some("bye") => break,
            _ => Some(json!({"type": "error", "code": "bad_message", "message": "不支持"})),
        };
        if let Some(reply) = reply {
            send_text(&mut stream, &reply.to_string()).await;
        }
    }
}

/// RFC 6455 的 `Sec-WebSocket-Accept`。
fn accept_key(key: &str) -> String {
    const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let digest = Sha1::digest(format!("{key}{GUID}").as_bytes());
    base64(&digest)
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
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

/// 发一个不掩码的文本帧。
async fn send_text(stream: &mut TcpStream, text: &str) {
    let payload = text.as_bytes();
    let mut frame = vec![0x81];
    if payload.len() < 126 {
        frame.push(payload.len() as u8);
    } else {
        frame.push(126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    let _ = stream.write_all(&frame).await;
}

/// 读一个帧；客户端发来的帧一定带掩码。返回文本内容，连接结束返回 `None`。
async fn read_text(stream: &mut TcpStream) -> Option<String> {
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
        // 文本
        0x1 => String::from_utf8(payload).ok(),
        // 关闭
        0x8 => None,
        // ping / pong / 二进制一律忽略，继续读
        _ => Some(String::new()),
    }
}

/// 超时保护，免得测试挂死。
async fn within<F: std::future::Future>(future: F) -> F::Output {
    timeout(Duration::from_secs(10), future)
        .await
        .expect("测试超时")
}

// ────────────────────────────────────────── 用例

#[tokio::test]
async fn 发现配置并核对短_id() {
    let (base, _handle) = start_relay(None, true, false).await;
    let endpoint = Endpoint::parse(&base).unwrap();

    let discovery = within(discover(&endpoint, "SecRelay/test")).await.unwrap();
    assert_eq!(discovery.id_check, IdCheck::Match);
    assert_eq!(discovery.expected_id, discovery.info.id);
    assert_eq!(discovery.info.version, "0.1.0");
    assert_eq!(discovery.info.protocol_version, 1);
    assert_eq!(discovery.info.credential_ttl, 600);
    assert_eq!(discovery.info.realm, "secrelay.relay");
    assert!(discovery.info.turn_configured);
    assert_eq!(discovery.info.turn_urls().len(), 1);
    assert_eq!(discovery.info.stun_urls(), vec!["stun:stun.example.com:3478"]);
    assert_eq!(
        discovery.signaling_url(),
        format!("ws://{}/ws/signal", endpoint.authority())
    );

    let health = within(health(&endpoint, "SecRelay/test")).await.unwrap();
    assert!(health.is_ok());
    assert_eq!(health.version, "0.1.0");
    assert!(health.turn_configured);
}

#[tokio::test]
async fn 上报的_id_不符时只标记不拒绝() {
    let (base, _handle) = start_relay(Some("ZZZZZZZZZZ"), true, false).await;
    let endpoint = Endpoint::parse(&base).unwrap();

    let discovery = within(discover(&endpoint, "SecRelay/test")).await.unwrap();
    assert_eq!(discovery.id_check, IdCheck::Mismatch);
    assert_ne!(discovery.expected_id, discovery.info.id);
    // 仍然给出了可用信息，没有直接失败
    assert!(!discovery.info.signaling.is_empty());
}

#[tokio::test]
async fn 上报的_id_格式不对也算不符() {
    let (base, _handle) = start_relay(Some("nope"), true, false).await;
    let endpoint = Endpoint::parse(&base).unwrap();

    let discovery = within(discover(&endpoint, "SecRelay/test")).await.unwrap();
    assert_eq!(discovery.id_check, IdCheck::Malformed);
}

#[tokio::test]
async fn 探活失败时给出错误而不是假成功() {
    let (base, _handle) = start_relay(None, true, true).await;
    let endpoint = Endpoint::parse(&base).unwrap();

    assert!(within(discover(&endpoint, "SecRelay/test")).await.is_err());
    assert!(within(health(&endpoint, "SecRelay/test")).await.is_err());
}

#[tokio::test]
async fn 连不上的端口报错() {
    // 绑定后立刻释放，端口上没有人监听
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let endpoint = Endpoint::parse(&format!("http://{addr}")).unwrap();
    assert!(within(health(&endpoint, "SecRelay/test")).await.is_err());
}

#[tokio::test]
async fn 信令收发() {
    let (base, _handle) = start_relay(None, true, false).await;
    let endpoint = Endpoint::parse(&base).unwrap();
    let url = format!("ws://{}/ws/signal", endpoint.authority());

    let mut socket = within(SignalSocket::connect(&url, "peer-a")).await.unwrap();

    // 服务端先发 hello
    match within(socket.next_event()).await.unwrap() {
        SignalEvent::Message(ServerMsg::Hello { protocol_version }) => {
            assert_eq!(protocol_version, 1)
        }
        other => panic!("期望 hello，收到 {other:?}"),
    }

    within(socket.send(&ClientMsg::Create)).await.unwrap();
    match within(socket.next_event()).await.unwrap() {
        SignalEvent::Message(ServerMsg::Created { session_id }) => {
            assert_eq!(session_id, "ab12cd34")
        }
        other => panic!("期望 created，收到 {other:?}"),
    }

    within(socket.send(&ClientMsg::Join {
        session_id: "ab12cd34".into(),
    }))
    .await
    .unwrap();
    match within(socket.next_event()).await.unwrap() {
        SignalEvent::Message(ServerMsg::Joined { session_id }) => {
            assert_eq!(session_id, "ab12cd34")
        }
        other => panic!("期望 joined，收到 {other:?}"),
    }

    let sdp = json!({"sdp": "v=0", "type": "offer"});
    within(socket.send(&ClientMsg::Offer {
        payload: sdp.clone(),
    }))
    .await
    .unwrap();
    match within(socket.next_event()).await.unwrap() {
        SignalEvent::Message(ServerMsg::Offer { from, payload }) => {
            assert_eq!(from, "peer-b");
            assert_eq!(payload, sdp);
        }
        other => panic!("期望 offer，收到 {other:?}"),
    }

    within(socket.close()).await.unwrap();
}

#[tokio::test]
async fn 信令连不上时报错() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    assert!(within(SignalSocket::connect(
        &format!("ws://{addr}/ws/signal"),
        "peer-a"
    ))
    .await
    .is_err());
}

#[tokio::test]
async fn 只给_stun_时中继路径标记为不可用() {
    let (base, _handle) = start_relay(None, false, false).await;
    let endpoint = Endpoint::parse(&base).unwrap();

    let discovery = within(discover(&endpoint, "SecRelay/test")).await.unwrap();
    assert!(!discovery.info.turn_configured);
    assert!(discovery.info.turn_urls().is_empty());
    assert_eq!(discovery.info.stun_urls().len(), 1);
    // 信令地址不受 TURN 影响
    assert!(discovery.signaling_url().ends_with("/ws/signal"));

    let health = within(health(&endpoint, "SecRelay/test")).await.unwrap();
    assert!(!health.is_ok());
}

#[tokio::test]
async fn 响应缺字段时退回默认值() {
    let payload: RelayInfo = serde_json::from_str("{}").unwrap();
    assert_eq!(payload, RelayInfo::default());
}

#[tokio::test]
async fn 短_id_的核对规则与固定向量一致() {
    // 对拍：本地算出来的值必须与中继仓库给出的三组值一致
    for (base, expected) in [
        ("https://relay.example.com", "AF4KR6IMPE"),
        ("http://127.0.0.1:8080", "6AYTZEVUQP"),
        ("https://relay.secrelay.dev", "VQPG6YZOS3"),
    ] {
        let endpoint = Endpoint::parse(base).unwrap();
        assert_eq!(endpoint.local_id(), expected, "{base}");
    }
}
