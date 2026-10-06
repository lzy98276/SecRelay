//! SecRelay 中继客户端。
//!
//! 覆盖"能配置、能发现、能探活、能核对 ID、能连信令"这一段：给一个中继基址，
//! 推导出发现/探活/信令地址，把 `/api/v1/relay` 的响应解析成类型，
//! 并用本地算出的短 ID 核对中继的自称。
//!
//! **不驱动会话**：SDP 与 ICE 候选只是透传，打洞、媒体、配额都不在这里。
//!
//! ```
//! use secrelay_relay_client::Endpoint;
//!
//! let endpoint = Endpoint::parse("https://relay.example.com").unwrap();
//! assert_eq!(endpoint.discovery_url(), "https://relay.example.com/api/v1/relay");
//! assert_eq!(endpoint.local_signaling_url(), "wss://relay.example.com/ws/signal");
//! assert_eq!(endpoint.local_id(), "AF4KR6IMPE");
//! ```

pub mod discovery;
pub mod error;
pub mod http;
pub mod identity;
pub mod signaling;
pub mod url;

pub use discovery::{discover, health, Discovery, Health, IceServer, RelayInfo};
pub use error::Error;
pub use identity::{relay_id, verify, IdCheck};
pub use signaling::{
    ClientMsg, ServerMsg, SignalEvent, SignalSink, SignalSocket, PROTOCOL_VERSION,
};
pub use url::{Endpoint, Scheme};

/// 内置的默认中继基址。
pub const DEFAULT_RELAY_BASE: &str = Endpoint::DEFAULT_BASE;

/// 各中继共用的 `User-Agent`。
pub fn user_agent() -> String {
    format!("SecRelay/{}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 默认中继可用且短_id_与固定向量一致() {
        let endpoint = Endpoint::parse(DEFAULT_RELAY_BASE).unwrap();
        assert_eq!(DEFAULT_RELAY_BASE, "https://secrelay-relay.sectl.cn");
        assert_eq!(endpoint.local_id(), "VQPG6YZOS3");
    }

    #[test]
    fn user_agent_带版本号() {
        assert!(user_agent().starts_with("SecRelay/"));
        assert!(user_agent().contains(env!("CARGO_PKG_VERSION")));
    }
}
