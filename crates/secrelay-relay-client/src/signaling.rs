//! 信令：WebSocket 上的房间协调与 SDP / ICE 透传。
//!
//! 这里只做编解码与收发，不驱动会话 —— 候选怎么用、SDP 怎么生成属于传输层。

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{client_async_tls_with_config, Connector, MaybeTlsStream, WebSocketStream};

use crate::error::Error;

/// 客户端声称支持的协议版本。服务端版本更高时会回一条 `error`。
pub const PROTOCOL_VERSION: u32 = 1;

/// 客户端 → 服务端。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// 声明匿名身份，必须最先发送。
    Hello { peer_id: String, protocol_version: u32 },
    /// 创建会话，服务端回 `created`。
    Create,
    /// 用短码加入会话。
    Join { session_id: String },
    /// 透传 SDP offer。
    Offer { payload: Value },
    /// 透传 SDP answer。
    Answer { payload: Value },
    /// 透传单个 ICE 候选。
    Candidate { payload: Value },
    /// 主动离开。
    Bye,
}

/// 服务端 → 客户端。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Hello { protocol_version: u32 },
    Created { session_id: String },
    Joined { session_id: String },
    PeerJoined { peer_id: String },
    PeerLeft { peer_id: String },
    Offer { from: String, payload: Value },
    Answer { from: String, payload: Value },
    Candidate { from: String, payload: Value },
    Error { code: String, message: String },
}

/// 一次读取的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalEvent {
    /// 收到一条协议消息。
    Message(ServerMsg),
    /// 对端关掉了连接。
    Closed,
    /// 收到无法解析的内容或非文本帧。
    Ignored,
}

/// 一条已连接的信令通道。
pub struct SignalSocket {
    sink: futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    stream: futures_util::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
}

impl SignalSocket {
    /// 连接信令地址并发 `hello` 声明匿名身份。
    pub async fn connect(url: &str, peer_id: &str) -> Result<Self, Error> {
        let (tls, host, port) = split_signaling_url(url)?;
        let stream = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|err| Error::WebSocket(format!("连接 {host}:{port} 失败：{err}")))?;

        // 自己建 TCP，是为了能用与 HTTP 请求同一个 rustls 配置。
        let connector = match tls {
            true => Some(Connector::Rustls(crate::http::client_config()?)),
            false => None,
        };

        let (socket, _response) = client_async_tls_with_config(url, stream, None, connector)
            .await
            .map_err(|err| Error::WebSocket(err.to_string()))?;
        let (sink, stream) = socket.split();
        let mut socket = Self { sink, stream };
        socket
            .send(&ClientMsg::Hello {
                peer_id: peer_id.to_string(),
                protocol_version: PROTOCOL_VERSION,
            })
            .await?;
        Ok(socket)
    }

    /// 发一条消息。
    pub async fn send(&mut self, msg: &ClientMsg) -> Result<(), Error> {
        let text = serde_json::to_string(msg)
            .map_err(|err| Error::WebSocket(format!("序列化失败：{err}")))?;
        self.sink
            .send(Message::Text(text.into()))
            .await
            .map_err(|err| Error::WebSocket(err.to_string()))
    }

    /// 等一条消息。
    pub async fn next_event(&mut self) -> Result<SignalEvent, Error> {
        loop {
            let Some(frame) = self.stream.next().await else {
                return Ok(SignalEvent::Closed);
            };
            match frame {
                Ok(Message::Text(text)) => match serde_json::from_str::<ServerMsg>(&text) {
                    Ok(msg) => return Ok(SignalEvent::Message(msg)),
                    Err(err) => {
                        tracing::warn!(%err, "信令消息无法解析，已忽略");
                        return Ok(SignalEvent::Ignored);
                    }
                },
                // 二进制帧不在协议里；Ping/Pong 由底层自动处理。
                Ok(Message::Binary(_)) => continue,
                Ok(Message::Close(_)) => return Ok(SignalEvent::Closed),
                Ok(_) => continue,
                Err(err) => return Err(Error::WebSocket(err.to_string())),
            }
        }
    }

    /// 发 `bye` 并关闭连接。
    pub async fn close(mut self) -> Result<(), Error> {
        let _ = self.send(&ClientMsg::Bye).await;
        self.sink
            .close()
            .await
            .map_err(|err| Error::WebSocket(err.to_string()))
    }

    /// 拆成发送端与接收端。
    ///
    /// 从发送端可以直接发 `bye`，不再借用 `SignalSocket`。
    pub fn split(
        self,
    ) -> (
        SignalSink,
        futures_util::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    ) {
        (SignalSink { sink: self.sink }, self.stream)
    }
}

/// 信令的发送端。
pub struct SignalSink {
    sink: futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
}

impl SignalSink {
    /// 发一条消息。
    pub async fn send(&mut self, msg: &ClientMsg) -> Result<(), Error> {
        let text = serde_json::to_string(msg)
            .map_err(|err| Error::WebSocket(format!("序列化失败：{err}")))?;
        self.sink
            .send(Message::Text(text.into()))
            .await
            .map_err(|err| Error::WebSocket(err.to_string()))
    }

    /// 发 `bye` 后关闭。连接已经断了不算错。
    pub async fn close(mut self) -> Result<(), Error> {
        let _ = self.send(&ClientMsg::Bye).await;
        let _ = self.sink.close().await;
        Ok(())
    }
}

/// 信令地址的协议、主机与端口。
fn split_signaling_url(url: &str) -> Result<(bool, String, u16), Error> {
    let bad = || Error::BadSignalingUrl {
        url: url.to_string(),
    };

    let (scheme, rest) = url.split_once("://").ok_or_else(bad)?;
    let tls = match scheme {
        "ws" => false,
        "wss" => true,
        _ => return Err(bad()),
    };
    let default_port = if tls { 443 } else { 80 };

    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return Err(bad());
    }

    // 冒号只在最后一段且全为数字时才算端口，避免把 IPv6 的冒号当分隔符。
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => (
            host.to_string(),
            port.parse::<u16>()
                .map_err(|_| Error::InvalidPort { port: port.to_string() })?,
        ),
        _ => (authority.to_string(), default_port),
    };
    if host.is_empty() {
        return Err(bad());
    }
    Ok((tls, host, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 客户端消息的线上格式() {
        let hello = serde_json::to_value(ClientMsg::Hello {
            peer_id: "peer-a".into(),
            protocol_version: PROTOCOL_VERSION,
        })
        .unwrap();
        assert_eq!(
            hello,
            json!({"type": "hello", "peer_id": "peer-a", "protocol_version": 1})
        );

        assert_eq!(
            serde_json::to_value(ClientMsg::Create).unwrap(),
            json!({"type": "create"})
        );
        assert_eq!(
            serde_json::to_value(ClientMsg::Join {
                session_id: "deadbeef".into()
            })
            .unwrap(),
            json!({"type": "join", "session_id": "deadbeef"})
        );
        assert_eq!(
            serde_json::to_value(ClientMsg::Candidate {
                payload: json!({"candidate": "x"})
            })
            .unwrap(),
            json!({"type": "candidate", "payload": {"candidate": "x"}})
        );
        assert_eq!(
            serde_json::to_value(ClientMsg::Bye).unwrap(),
            json!({"type": "bye"})
        );
    }

    #[test]
    fn 服务端消息的线上格式() {
        let hello: ServerMsg =
            serde_json::from_str(r#"{"type":"hello","protocol_version":1}"#).unwrap();
        assert_eq!(hello, ServerMsg::Hello { protocol_version: 1 });

        let created: ServerMsg =
            serde_json::from_str(r#"{"type":"created","session_id":"ab12cd34"}"#).unwrap();
        assert_eq!(
            created,
            ServerMsg::Created {
                session_id: "ab12cd34".into()
            }
        );

        let offer: ServerMsg = serde_json::from_str(
            r#"{"type":"offer","from":"peer-b","payload":{"sdp":"v=0"}}"#,
        )
        .unwrap();
        assert_eq!(
            offer,
            ServerMsg::Offer {
                from: "peer-b".into(),
                payload: json!({"sdp": "v=0"}),
            }
        );

        let error: ServerMsg =
            serde_json::from_str(r#"{"type":"error","code":"join_failed","message":"会话不存在"}"#)
                .unwrap();
        assert_eq!(
            error,
            ServerMsg::Error {
                code: "join_failed".into(),
                message: "会话不存在".into(),
            }
        );

        for text in [
            r#"{"type":"peer_joined","peer_id":"peer-b"}"#,
            r#"{"type":"peer_left","peer_id":"peer-b"}"#,
            r#"{"type":"joined","session_id":"ab12cd34"}"#,
            r#"{"type":"answer","from":"b","payload":{}}"#,
            r#"{"type":"candidate","from":"b","payload":{}}"#,
        ] {
            serde_json::from_str::<ServerMsg>(text).unwrap_or_else(|err| panic!("{text}: {err}"));
        }
    }

    #[test]
    fn 未知消息类型解析失败而不是误读() {
        assert!(serde_json::from_str::<ServerMsg>(r#"{"type":"未来的消息"}"#).is_err());
        assert!(serde_json::from_str::<ServerMsg>("not json").is_err());
    }

    #[test]
    fn 往返一致() {
        for msg in [
            ClientMsg::Hello {
                peer_id: "peer-a".into(),
                protocol_version: 1,
            },
            ClientMsg::Create,
            ClientMsg::Join {
                session_id: "x".into(),
            },
            ClientMsg::Offer {
                payload: json!({"sdp": "v=0"}),
            },
            ClientMsg::Answer {
                payload: json!({"sdp": "v=0"}),
            },
            ClientMsg::Candidate {
                payload: json!({"candidate": "c"}),
            },
            ClientMsg::Bye,
        ] {
            let text = serde_json::to_string(&msg).unwrap();
            assert_eq!(serde_json::from_str::<ClientMsg>(&text).unwrap(), msg);
        }
    }

    #[test]
    fn 只有_ws_wss_的地址能用() {
        assert_eq!(
            split_signaling_url("ws://127.0.0.1:8080/ws/signal").unwrap(),
            (false, "127.0.0.1".to_string(), 8080)
        );
        assert_eq!(
            split_signaling_url("wss://relay.example.com/ws/signal").unwrap(),
            (true, "relay.example.com".to_string(), 443)
        );
        assert_eq!(
            split_signaling_url("ws://relay.example.com/ws/signal").unwrap(),
            (false, "relay.example.com".to_string(), 80)
        );
        assert!(split_signaling_url("https://relay.example.com/ws/signal").is_err());
        assert!(split_signaling_url("/ws/signal").is_err());
        assert!(split_signaling_url("wss://").is_err());
        assert!(split_signaling_url("ws://host:/x").is_err());
    }

    #[test]
    fn 能建出_tls_配置() {
        // 内置根证书可用时这一步才会成功
        assert!(crate::http::client_config().is_ok());
    }
}
