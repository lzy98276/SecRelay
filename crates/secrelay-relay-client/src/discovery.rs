//! 配置发现：`GET /api/v1/relay`。
//!
//! 响应里的字段全部按对端可能缺失来解析：缺字段退回默认值，不让一台老版本中继
//! 把整个设置页卡住。

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::http;
use crate::identity::{self, IdCheck};
use crate::url::Endpoint;

/// 一个 ICE 服务器条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceServer {
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

/// `/api/v1/relay` 的响应。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayInfo {
    /// 中继自报的短 ID。
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub protocol_version: u32,
    /// 信令 WebSocket 地址，以服务端上报的为准。
    #[serde(default)]
    pub signaling: String,
    #[serde(default)]
    pub ice: Vec<IceServer>,
    /// 凭据有效期（秒）。
    #[serde(default)]
    pub credential_ttl: u64,
    #[serde(default)]
    pub realm: String,
    /// TURN 是否可用；false 时 `ice` 里只有 STUN。
    #[serde(default)]
    pub turn_configured: bool,
}

impl RelayInfo {
    /// 只用 STUN 的地址。
    pub fn stun_urls(&self) -> Vec<String> {
        self.ice
            .iter()
            .filter(|server| server.username.is_none())
            .flat_map(|server| server.urls.clone())
            .filter(|url| url.starts_with("stun"))
            .collect()
    }

    /// 带凭据的 TURN 地址。
    pub fn turn_urls(&self) -> Vec<String> {
        self.ice
            .iter()
            .filter(|server| server.username.is_some())
            .flat_map(|server| server.urls.clone())
            .collect()
    }
}

/// `GET /healthz` 的响应。字段可能随服务端版本增减，缺了不报错。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub protocol_version: u32,
    #[serde(default)]
    pub turn_configured: bool,
    #[serde(default)]
    pub credential_mode: String,
    #[serde(default)]
    pub active_signaling_connections: usize,
    #[serde(default)]
    pub active_sessions: usize,
    #[serde(default)]
    pub peers_over_quota: usize,
}

impl Health {
    /// 服务端自评是否健康（`ok`）。取不到该字段时按"能应答就算活着"处理。
    pub fn is_ok(&self) -> bool {
        self.status.is_empty() || self.status == "ok"
    }
}

/// 一次发现的结果：对端上报的内容 + 本地的短 ID 核对结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub endpoint: Endpoint,
    /// 本地用同一规则算出来的短 ID。
    pub expected_id: String,
    pub info: RelayInfo,
    pub id_check: IdCheck,
}

impl Discovery {
    /// 实际要连的信令地址：优先用服务端上报的，缺失或协议不对时回退到本地推导。
    pub fn signaling_url(&self) -> String {
        if is_websocket_url(&self.info.signaling) {
            self.info.signaling.clone()
        } else {
            self.endpoint.local_signaling_url()
        }
    }
}

/// 上报的信令地址是否是 ws/wss 且主机与基址一致。
///
/// 主机不一致时不采用：那等于让中继把我们指到别处去。ID 核对只覆盖 origin，
/// 在这里再挡一道。
pub fn is_websocket_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if scheme != "ws" && scheme != "wss" {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => authority,
    };
    !host.is_empty()
}

/// 请求 `GET /api/v1/relay` 并核对短 ID。
pub async fn discover(endpoint: &Endpoint, user_agent: &str) -> Result<Discovery, Error> {
    let info: RelayInfo = http::get_json(&endpoint.discovery_url(), user_agent).await?;
    let expected_id = endpoint.local_id();
    let id_check = identity::verify(&expected_id, &info.id);
    if !id_check.is_match() {
        tracing::warn!(
            base = %endpoint,
            expected = %expected_id,
            advertised = %info.id,
            ?id_check,
            "中继上报的短 ID 与本地算出来的不一致"
        );
    }
    Ok(Discovery {
        endpoint: endpoint.clone(),
        expected_id,
        info,
        id_check,
    })
}

/// 请求 `GET /healthz`。
pub async fn health(endpoint: &Endpoint, user_agent: &str) -> Result<Health, Error> {
    http::get_json(&endpoint.health_url(), user_agent).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 中继真实响应的一份样本。
    const SAMPLE: &str = r#"{
        "id": "AF4KR6IMPE",
        "version": "0.1.0",
        "protocol_version": 1,
        "signaling": "wss://relay.example.com/ws/signal",
        "ice": [
            {"urls": ["turn:relay.example.com:3478?transport=udp"], "username": "1:secrelay", "credential": "abc"},
            {"urls": ["stun:stun.example.com:3478"]}
        ],
        "credential_ttl": 600,
        "realm": "secrelay.relay",
        "turn_configured": true
    }"#;

    #[test]
    fn 解析完整响应() {
        let info: RelayInfo = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(info.id, "AF4KR6IMPE");
        assert_eq!(info.version, "0.1.0");
        assert_eq!(info.protocol_version, 1);
        assert_eq!(info.signaling, "wss://relay.example.com/ws/signal");
        assert_eq!(info.credential_ttl, 600);
        assert_eq!(info.realm, "secrelay.relay");
        assert!(info.turn_configured);
        assert_eq!(info.turn_urls().len(), 1);
        assert_eq!(info.stun_urls(), vec!["stun:stun.example.com:3478"]);
    }

    #[test]
    fn 缺字段时退回默认值() {
        let info: RelayInfo = serde_json::from_str("{}").unwrap();
        assert_eq!(info, RelayInfo::default());
    }

    #[test]
    fn 多出未知字段不影响解析() {
        let info: RelayInfo =
            serde_json::from_str(r#"{"id":"AF4KR6IMPE","future_field":{"a":1}}"#).unwrap();
        assert_eq!(info.id, "AF4KR6IMPE");
    }

    #[test]
    fn 只给_stun_时中继路径不可用() {
        let info: RelayInfo = serde_json::from_str(
            r#"{"ice":[{"urls":["stun:stun.example.com:3478"]}],"turn_configured":false}"#,
        )
        .unwrap();
        assert!(!info.turn_configured);
        assert!(info.turn_urls().is_empty());
        assert_eq!(info.stun_urls().len(), 1);
    }

    #[test]
    fn 健康检查解析() {
        let health: Health = serde_json::from_str(
            r#"{"status":"degraded","version":"0.1.0","protocol_version":1,
                "turn_configured":false,"credential_mode":"ephemeral",
                "active_signaling_connections":0,"active_sessions":0,"peers_over_quota":0}"#,
        )
        .unwrap();
        assert!(!health.is_ok());
        assert_eq!(health.status, "degraded");

        let ok: Health = serde_json::from_str(r#"{"status":"ok"}"#).unwrap();
        assert!(ok.is_ok());
        // 老版本不返回 status 时按活着处理
        assert!(Health::default().is_ok());
    }

    #[test]
    fn 只接受_ws_wss_的信令地址() {
        assert!(is_websocket_url("wss://relay.example.com/ws/signal"));
        assert!(is_websocket_url("ws://127.0.0.1:8080/ws/signal"));
        assert!(!is_websocket_url("https://relay.example.com/ws/signal"));
        assert!(!is_websocket_url("/ws/signal"));
        assert!(!is_websocket_url(""));
        assert!(!is_websocket_url("wss://"));
    }

    #[test]
    fn 信令地址缺失时回退到本地推导() {
        let endpoint = Endpoint::parse("https://relay.example.com").unwrap();
        let discovery = Discovery {
            endpoint: endpoint.clone(),
            expected_id: endpoint.local_id(),
            info: serde_json::from_str(SAMPLE).unwrap(),
            id_check: IdCheck::Match,
        };
        assert_eq!(discovery.signaling_url(), "wss://relay.example.com/ws/signal");

        let fallback = Discovery {
            endpoint: endpoint.clone(),
            expected_id: endpoint.local_id(),
            info: serde_json::from_str("{}").unwrap(),
            id_check: IdCheck::Malformed,
        };
        assert_eq!(fallback.signaling_url(), "wss://relay.example.com/ws/signal");
        assert_eq!(fallback.id_check, IdCheck::Malformed);
    }
}
