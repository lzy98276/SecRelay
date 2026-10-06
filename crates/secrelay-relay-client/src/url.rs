//! 中继基址的解析、校验与规范化。
//!
//! 用户只输入一个基址（`https://relay.example.com`），其余地址都从这里推导：
//! `/api/v1/relay`、`/healthz`、`/ws/signal`。
//!
//! 规范化与 [`crate::identity`] 的短 ID 必须用同一套规则，否则同一台中继在两边会算出不同的 ID。

use std::fmt;
use std::sync::Arc;

use crate::error::Error;

/// 规范化后的中继基址。
///
/// 保存的形态固定为 `scheme://host[:port]` + 可选路径前缀：scheme 与 host 全小写、
/// 省略默认端口、结尾不留斜杠。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Endpoint {
    inner: Arc<Inner>,
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct Inner {
    scheme: Scheme,
    host: String,
    port: Option<u16>,
    path: String,
    text: String,
}

/// 基址协议。只有 http 与 https，其余一律拒绝。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    Http,
    Https,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Scheme::Http => 80,
            Scheme::Https => 443,
        }
    }

    /// 信令用的 WebSocket 协议：https 对应 wss，http 对应 ws。
    pub fn websocket(self) -> &'static str {
        match self {
            Scheme::Http => "ws",
            Scheme::Https => "wss",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "http" => Some(Scheme::Http),
            "https" => Some(Scheme::Https),
            _ => None,
        }
    }
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Endpoint {
    /// 解析并规范化基址。
    pub fn parse(input: &str) -> Result<Self, Error> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(Error::EmptyBaseUrl);
        }

        let (scheme_text, rest) = match trimmed.split_once("://") {
            Some((scheme, rest)) => (scheme.trim(), rest),
            None => {
                let scheme = trimmed.split(":").next().unwrap_or(trimmed);
                return Err(Error::MissingScheme {
                    found: scheme.to_string(),
                });
            }
        };

        let scheme = Scheme::parse(scheme_text).ok_or_else(|| Error::UnsupportedScheme {
            scheme: scheme_text.to_string(),
        })?;

        // 去掉路径/查询/片段之后再拆主机与端口；基址里带路径前缀时保留它。
        let authority_end = rest
            .find(['/', '?', '#'])
            .unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        let path = normalize_path(&rest[authority_end..]);

        if authority.is_empty() {
            return Err(Error::EmptyHost);
        }
        if authority.contains('@') {
            return Err(Error::InvalidHost {
                host: authority.to_string(),
            });
        }

        let (host, port) = split_authority(authority, scheme)?;
        let text = match port {
            Some(port) if port != scheme.default_port() => format!("{scheme}://{host}:{port}{path}"),
            _ => format!("{scheme}://{host}{path}"),
        };

        Ok(Self {
            inner: Arc::new(Inner {
                scheme,
                host,
                port,
                path,
                text,
            }),
        })
    }

    /// 内置的默认中继基址。开箱可用。
    pub const DEFAULT_BASE: &'static str = "https://relay.secrelay.dev";

    pub fn scheme(&self) -> Scheme {
        self.inner.scheme
    }

    pub fn host(&self) -> &str {
        &self.inner.host
    }

    /// 用户显式写的非默认端口。
    pub fn port(&self) -> Option<u16> {
        self.inner.port
    }

    /// 规范化后的基址文本。
    pub fn as_str(&self) -> &str {
        &self.inner.text
    }

    /// 连接用的 `host:port`。
    pub fn authority(&self) -> String {
        let port = self
            .inner
            .port
            .unwrap_or_else(|| self.inner.scheme.default_port());
        format!("{}:{port}", self.inner.host)
    }

    /// 短 ID 的输入：`scheme://host[:port]`，不含路径。
    pub fn canonical_origin(&self) -> String {
        match self.inner.port {
            Some(port) if port != self.inner.scheme.default_port() => {
                format!("{}://{}:{port}", self.inner.scheme, self.inner.host)
            }
            _ => format!("{}://{}", self.inner.scheme, self.inner.host),
        }
    }

    /// 拼接一个以 `/` 开头的绝对路径。
    pub fn join(&self, absolute_path: &str) -> String {
        format!("{}{}", self.inner.text, absolute_path)
    }

    /// 发现的地址：`{base}/api/v1/relay`。
    pub fn discovery_url(&self) -> String {
        self.join("/api/v1/relay")
    }

    /// 探活地址：`{base}/healthz`。
    pub fn health_url(&self) -> String {
        self.join("/healthz")
    }

    /// 本地的信令地址推导：`{base}/ws/signal`，https→wss / http→ws。
    ///
    /// 实际连接以服务端 `/api/v1/relay` 上报的 `signaling` 为准，这里用于给用户看。
    pub fn local_signaling_url(&self) -> String {
        let port = match self.inner.port {
            Some(port) if port != self.inner.scheme.default_port() => format!(":{port}"),
            _ => String::new(),
        };
        format!(
            "{}://{}{port}{}/ws/signal",
            self.inner.scheme.websocket(),
            self.inner.host,
            self.inner.path
        )
    }

    /// 本地的短 ID：与规范化的 origin 绑定。
    pub fn local_id(&self) -> String {
        crate::identity::relay_id(&self.canonical_origin())
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 路径前缀规范化：空路径与 `/` 都变成空串，其余去掉结尾斜杠。
fn normalize_path(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        String::new()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

/// 拆分 `host[:port]`。
fn split_authority(authority: &str, scheme: Scheme) -> Result<(String, Option<u16>), Error> {
    if let Some(rest) = authority.strip_prefix('[') {
        // IPv6 字面量：括号内允许冒号，端口写在括号之后。
        let (inside, after) = rest.split_once(']').ok_or_else(|| Error::InvalidHost {
            host: authority.to_string(),
        })?;
        inside.parse::<std::net::Ipv6Addr>().map_err(|_| Error::InvalidHost {
            host: authority.to_string(),
        })?;
        let port = parse_port(after, scheme, authority)?;
        return Ok((format!("[{inside}]"), port));
    }

    let (host, port_text) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };

    let host = host.to_ascii_lowercase();
    if !is_valid_host(&host) {
        return Err(Error::InvalidHost {
            host: authority.to_string(),
        });
    }

    let port = match port_text {
        Some(port) => parse_port(&format!(":{port}"), scheme, authority)?,
        None => None,
    };
    Ok((host, port))
}

/// 解析可选的 `:port` 后缀。
fn parse_port(after: &str, scheme: Scheme, authority: &str) -> Result<Option<u16>, Error> {
    if after.is_empty() {
        return Ok(None);
    }
    let digits = after.strip_prefix(':').ok_or_else(|| Error::InvalidHost {
        host: authority.to_string(),
    })?;
    if digits.is_empty() {
        return Err(Error::EmptyPort {
            host: authority.to_string(),
        });
    }
    let port: u16 = digits.parse().map_err(|_| Error::InvalidPort {
        port: digits.to_string(),
    })?;
    if port == 0 {
        return Err(Error::InvalidPort {
            port: digits.to_string(),
        });
    }
    if port == scheme.default_port() {
        return Ok(None);
    }
    Ok(Some(port))
}

/// 主机名的粗校验：IPv4 字面量，或 `字母/数字/-/.` 组成的域名。
fn is_valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    host.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && !host.starts_with('.')
        && !host.ends_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 拒绝非_http_https_的协议() {
        for input in [
            "ftp://relay.example.com",
            "ws://relay.example.com",
            "wss://relay.example.com",
            "file:///tmp/x",
            "localhost:8080",
            "relay.example.com",
            "",
            "   ",
        ] {
            assert!(Endpoint::parse(input).is_err(), "{input} 应当被拒绝");
        }
    }

    #[test]
    fn 缺协议与协议不受支持给出不同的错误() {
        assert!(matches!(
            Endpoint::parse("relay.example.com"),
            Err(Error::MissingScheme { .. })
        ));
        assert!(matches!(
            Endpoint::parse("ftp://relay.example.com"),
            Err(Error::UnsupportedScheme { .. })
        ));
    }

    #[test]
    fn 大小写与协议都要规范化() {
        let endpoint = Endpoint::parse("HTTPS://Relay.Example.COM").unwrap();
        assert_eq!(endpoint.as_str(), "https://relay.example.com");
        assert_eq!(endpoint.scheme(), Scheme::Https);
        assert_eq!(endpoint.host(), "relay.example.com");
        assert_eq!(endpoint.port(), None);
    }

    #[test]
    fn 去掉结尾斜杠() {
        for input in [
            "https://relay.example.com/",
            "https://relay.example.com///",
            "  https://relay.example.com  ",
        ] {
            let endpoint = Endpoint::parse(input).unwrap();
            assert_eq!(endpoint.as_str(), "https://relay.example.com", "{input}");
            assert_eq!(
                endpoint.canonical_origin(),
                "https://relay.example.com",
                "{input}"
            );
        }
    }

    #[test]
    fn 查询与片段被丢掉但路径保留() {
        let query = Endpoint::parse("https://relay.example.com/api/v1/relay?x=1#frag").unwrap();
        assert_eq!(query.as_str(), "https://relay.example.com/api/v1/relay");
        // 短 ID 只认 origin，路径怎么写都一样
        assert_eq!(query.canonical_origin(), "https://relay.example.com");

        let bare = Endpoint::parse("https://relay.example.com?x=1#frag").unwrap();
        assert_eq!(bare.as_str(), "https://relay.example.com");
    }

    #[test]
    fn 省略默认端口但保留非默认端口() {
        assert_eq!(
            Endpoint::parse("https://relay.example.com:443").unwrap().as_str(),
            "https://relay.example.com"
        );
        assert_eq!(
            Endpoint::parse("http://relay.example.com:80").unwrap().as_str(),
            "http://relay.example.com"
        );
        assert_eq!(
            Endpoint::parse("https://relay.example.com:8443").unwrap().as_str(),
            "https://relay.example.com:8443"
        );
        assert_eq!(
            Endpoint::parse("http://127.0.0.1:8080").unwrap().as_str(),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn 避免同一中继出现两种写法() {
        let a = Endpoint::parse("https://relay.example.com:443/").unwrap();
        let b = Endpoint::parse("HTTPS://Relay.example.com").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn ipv6_字面量带括号() {
        let endpoint = Endpoint::parse("http://[::1]:8080").unwrap();
        assert_eq!(endpoint.host(), "[::1]");
        assert_eq!(endpoint.as_str(), "http://[::1]:8080");
        assert_eq!(endpoint.authority(), "[::1]:8080");
        assert!(Endpoint::parse("http://[::1").is_err());
        assert!(Endpoint::parse("http://[zz]:80").is_err());
    }

    #[test]
    fn 非法主机与端口被拒绝() {
        for input in [
            "https://",
            "https://:443",
            "https://relay_example.com",
            "https://.example.com",
            "https://relay.example.com:",
            "https://relay.example.com:abc",
            "https://relay.example.com:0",
            "https://relay.example.com:70000",
            "https://user@relay.example.com",
        ] {
            assert!(Endpoint::parse(input).is_err(), "{input} 应当被拒绝");
        }
    }

    #[test]
    fn 推导出的地址() {
        let endpoint = Endpoint::parse("https://relay.example.com").unwrap();
        assert_eq!(
            endpoint.discovery_url(),
            "https://relay.example.com/api/v1/relay"
        );
        assert_eq!(endpoint.health_url(), "https://relay.example.com/healthz");
        assert_eq!(
            endpoint.local_signaling_url(),
            "wss://relay.example.com/ws/signal"
        );

        let local = Endpoint::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!(local.health_url(), "http://127.0.0.1:8080/healthz");
        assert_eq!(local.local_signaling_url(), "ws://127.0.0.1:8080/ws/signal");
    }

    #[test]
    fn 路径前缀被保留() {
        let endpoint = Endpoint::parse("https://example.com/relay/").unwrap();
        assert_eq!(endpoint.as_str(), "https://example.com/relay");
        assert_eq!(endpoint.discovery_url(), "https://example.com/relay/api/v1/relay");
        assert_eq!(
            endpoint.local_signaling_url(),
            "wss://example.com/relay/ws/signal"
        );
        // 短 ID 只看 origin，与路径前缀无关
        assert_eq!(
            endpoint.canonical_origin(),
            Endpoint::parse("https://example.com")
                .unwrap()
                .canonical_origin()
        );
    }

    #[test]
    fn 默认基址可用() {
        let endpoint = Endpoint::parse(Endpoint::DEFAULT_BASE).unwrap();
        assert_eq!(endpoint.host(), "relay.secrelay.dev");
        assert_eq!(endpoint.local_id(), "VQPG6YZOS3");
        assert_eq!(
            endpoint.discovery_url(),
            "https://relay.secrelay.dev/api/v1/relay"
        );
    }
}
