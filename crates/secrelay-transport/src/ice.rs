//! ICE 服务器配置。
//!
//! 与中继服务的发现接口返回的 JSON 形状对齐，直接反序列化即可：
//!
//! ```json
//! {"ice":[{"urls":["turn:host:3478?transport=udp"],"username":"u","credential":"c"},
//!         {"urls":["stun:host:3478"]}]}
//! ```
//!
//! 中继发现返回的是整个对象；这里只取 `ice` 字段，因此额外字段一律忽略。

use std::net::SocketAddr;

use serde::{Deserialize, Deserializer};
use webrtc::peer_connection::{RTCIceServer, RTCIceTransportPolicy};

/// 一台 ICE 服务器：一组 URL 加可选的长期凭据。
///
/// `urls` 只允许字符串或字符串数组两种写法（单条 URL 省略方括号是常见写法）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IceServerConfig {
    #[serde(deserialize_with = "urls_from_str_or_seq")]
    pub urls: Vec<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub credential: String,
}

/// 传输层要用的 ICE 配置。
///
/// `stun_only` 为 `true` 时只收集直连候选（用于验证打洞失败时中继是否真的接上）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct IceConfig {
    pub ice: Vec<IceServerConfig>,
    pub stun_only: bool,
}

impl IceConfig {
    /// 不做任何 ICE 服务器查询，只用本机候选。
    pub fn host_only() -> Self {
        Self::default()
    }

    /// 只接受中继候选。
    pub fn transport_policy(&self) -> RTCIceTransportPolicy {
        if self.stun_only {
            RTCIceTransportPolicy::Relay
        } else {
            RTCIceTransportPolicy::All
        }
    }

    /// 转换成 webrtc 的服务器列表。
    pub fn to_ice_servers(&self) -> Vec<RTCIceServer> {
        self.ice
            .iter()
            .map(|server| RTCIceServer {
                urls: server.urls.clone(),
                username: server.username.clone(),
                credential: server.credential.clone(),
            })
            .collect()
    }

    /// 把发现接口返回的 `ice` 数组直接转成本配置。
    pub fn from_servers(ice: Vec<IceServerConfig>) -> Self {
        Self {
            ice,
            stun_only: false,
        }
    }
}

/// `urls` 接受 `"stun:h"` 与 `["stun:h","turn:h"]` 两种写法。
fn urls_from_str_or_seq<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(url) => vec![url],
        OneOrMany::Many(urls) => urls,
    })
}

/// 本机 UDP 绑定地址。
///
/// 默认绑 `0.0.0.0:0`，即所有网卡、随机端口 —— 打洞要能从任意网卡发出候选。
/// 测试里绑 `127.0.0.1:0` 可以确保流量不出本机。
pub fn default_udp_addrs() -> Vec<String> {
    vec!["0.0.0.0:0".to_string()]
}

/// 绑定地址的纯函数校验：`ip:port`，端口允许为 0。
pub fn parse_bind_addr(raw: &str) -> Result<SocketAddr, String> {
    raw.parse::<SocketAddr>()
        .map_err(|e| format!("非法绑定地址 {raw}：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 中继发现接口返回的形状。
    const DISCOVERY: &str = r#"{
        "id":"AF4KR6IMPE","version":"0.1.0","protocol_version":1,
        "signaling":"wss://relay.example.com/ws/signal",
        "ice":[{"urls":["turn:host:3478?transport=udp"],"username":"u1","credential":"c1"},
               {"urls":["stun:host:3478"]}],
        "credential_ttl":600,"realm":"secrelay.relay","turn_configured":true
    }"#;

    fn parse(raw: &str) -> Result<IceConfig, serde_json::Error> {
        serde_json::from_str(raw)
    }

    #[test]
    fn 解析发现接口的完整响应() {
        let config = parse(DISCOVERY).expect("应当能解析");
        assert_eq!(config.ice.len(), 2);
        assert_eq!(config.ice[0].urls, vec!["turn:host:3478?transport=udp"]);
        assert_eq!(config.ice[0].username, "u1");
        assert_eq!(config.ice[0].credential, "c1");
        assert_eq!(config.ice[1].urls, vec!["stun:host:3478"]);
        assert_eq!(config.ice[1].username, "");
        assert!(!config.stun_only);
        assert_eq!(config.transport_policy(), RTCIceTransportPolicy::All);
    }

    #[test]
    fn urls_接受字符串写法() {
        let config = parse(r#"{"ice":[{"urls":"stun:host:3478"}]}"#).unwrap();
        assert_eq!(config.ice[0].urls, vec!["stun:host:3478"]);
    }

    #[test]
    fn 缺失_ice_字段退回默认() {
        let config = parse(r#"{"signaling":"wss://x"}"#).unwrap();
        assert!(config.ice.is_empty());
        assert!(config.to_ice_servers().is_empty());
    }

    #[test]
    fn 未知字段被忽略() {
        let config = parse(r#"{"ice":[],"future":"x"}"#).unwrap();
        assert!(config.ice.is_empty());
    }

    #[test]
    fn 破损的_json_被拒绝() {
        assert!(parse("{ 不是 json").is_err());
    }

    #[test]
    fn 转换成_webrtc_服务器列表() {
        let config = parse(DISCOVERY).unwrap();
        let servers = config.to_ice_servers();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].urls[0], "turn:host:3478?transport=udp");
        assert_eq!(servers[0].username, "u1");
        assert_eq!(servers[0].credential, "c1");
        assert_eq!(servers[1].urls[0], "stun:host:3478");
    }

    #[test]
    fn stun_only_选择中继策略() {
        let mut config = parse(DISCOVERY).unwrap();
        config.stun_only = true;
        assert_eq!(config.transport_policy(), RTCIceTransportPolicy::Relay);
    }

    #[test]
    fn 绑定地址校验() {
        assert!(parse_bind_addr("0.0.0.0:0").is_ok());
        assert!(parse_bind_addr("127.0.0.1:5000").is_ok());
        assert!(parse_bind_addr("127.0.0.1").is_err());
        assert!(parse_bind_addr("").is_err());
    }
}
