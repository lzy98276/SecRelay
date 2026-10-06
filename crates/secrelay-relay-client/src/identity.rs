//! 中继短 ID：由对外 origin 推导，用来核对中继的自称。
//!
//! 中继是开放且没有注册表的，自行声明 ID 就能冒用别人的；把 ID 绑到 origin 之后，
//! 冒用某个 ID 就必须真正拥有那个 origin。
//!
//! ```text
//! canonical = scheme://host[:port]      // 全小写，去路径，省略默认端口
//! id        = base32(sha1(canonical))[0..10]   // RFC 4648 大写、无填充
//! ```
//!
//! 短 ID 是**标识**不是安全原语：它防的是冒用别人的 ID，而不是碰撞。

use sha1::{Digest, Sha1};

/// ID 长度（字符）。
pub const ID_LEN: usize = 10;

/// 由规范化的 origin 算出短 ID。
pub fn relay_id(canonical_origin: &str) -> String {
    let digest = Sha1::digest(canonical_origin.as_bytes());
    let encoded = base32_upper(&digest);
    encoded[..ID_LEN].to_string()
}

/// 核对中继在 `/api/v1/relay` 上报的 `id`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdCheck {
    /// 上报值与本地算出来的一致。
    Match,
    /// 上报值与本地算出来的不一致：不信任它的自称，但不拒绝连接。
    Mismatch,
    /// 上报的 `id` 不是合法的短 ID（长度或字符集不对）。
    Malformed,
}

impl IdCheck {
    pub fn is_match(self) -> bool {
        matches!(self, IdCheck::Match)
    }
}

/// 比对上报的 ID 与本地算出来的 ID。
pub fn verify(expected: &str, advertised: &str) -> IdCheck {
    let advertised = advertised.trim();
    if advertised.len() != ID_LEN
        || !advertised
            .chars()
            .all(|c| c.is_ascii_uppercase() || ('2'..='7').contains(&c))
    {
        return IdCheck::Malformed;
    }
    if advertised == expected {
        IdCheck::Match
    } else {
        IdCheck::Mismatch
    }
}

/// RFC 4648 base32：大写字母表，不带填充。
fn base32_upper(data: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;

    for byte in data {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 0x1F) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1F) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::url::Endpoint;

    #[test]
    fn 固定向量逐字符正确() {
        assert_eq!(relay_id("https://relay.example.com"), "AF4KR6IMPE");
        assert_eq!(relay_id("http://127.0.0.1:8080"), "6AYTZEVUQP");
        assert_eq!(relay_id("https://relay.secrelay.dev"), "VQPG6YZOS3");
    }

    #[test]
    fn 从基址算出的_id_与固定向量一致() {
        for (base, expected) in [
            ("https://relay.example.com", "AF4KR6IMPE"),
            ("http://127.0.0.1:8080", "6AYTZEVUQP"),
            ("https://relay.secrelay.dev", "VQPG6YZOS3"),
        ] {
            let endpoint = Endpoint::parse(base).unwrap();
            assert_eq!(endpoint.local_id(), expected, "{base}");
        }
    }

    #[test]
    fn 同一中继的不同写法算出同一个_id() {
        let expected = Endpoint::parse("https://relay.example.com").unwrap().local_id();
        for variant in [
            "HTTPS://Relay.Example.com",
            "https://relay.example.com/",
            "https://relay.example.com/api/v1/relay",
            "https://relay.example.com:443",
            "  https://Relay.Example.COM//  ",
        ] {
            assert_eq!(
                Endpoint::parse(variant).unwrap().local_id(),
                expected,
                "{variant} 应当等价"
            );
        }
        assert_eq!(
            Endpoint::parse("http://relay.example.com:80").unwrap().local_id(),
            Endpoint::parse("http://relay.example.com").unwrap().local_id()
        );
    }

    #[test]
    fn 端口参与计算() {
        assert_ne!(
            Endpoint::parse("https://relay.example.com").unwrap().local_id(),
            Endpoint::parse("https://relay.example.com:8443").unwrap().local_id()
        );
        assert_ne!(
            Endpoint::parse("http://127.0.0.1:8080").unwrap().local_id(),
            Endpoint::parse("http://127.0.0.1:9090").unwrap().local_id()
        );
    }

    #[test]
    fn 长度固定为十个字符() {
        for base in ["https://a.example", "http://192.168.1.1:9000", "http://[::1]:1234"] {
            assert_eq!(
                relay_id(&Endpoint::parse(base).unwrap().canonical_origin()).len(),
                ID_LEN,
                "{base}"
            );
        }
    }

    #[test]
    fn 核对结果分三种() {
        assert_eq!(verify("AF4KR6IMPE", "AF4KR6IMPE"), IdCheck::Match);
        assert_eq!(verify("AF4KR6IMPE", "AF4KR6IMPZ"), IdCheck::Mismatch);
        // 小写、长度不对、含 0/1/8/9（base32 字母表里没有）都算格式不对
        assert_eq!(verify("AF4KR6IMPE", "af4kr6impe"), IdCheck::Malformed);
        assert_eq!(verify("AF4KR6IMPE", "AF4KR6IMP"), IdCheck::Malformed);
        assert_eq!(verify("AF4KR6IMPE", "AF4KR6IMP0"), IdCheck::Malformed);
        assert_eq!(verify("AF4KR6IMPE", ""), IdCheck::Malformed);
        // 两端空白不算错
        assert_eq!(verify("AF4KR6IMPE", "  AF4KR6IMPE\n"), IdCheck::Match);
        assert!(IdCheck::Match.is_match());
        assert!(!IdCheck::Mismatch.is_match());
    }

    #[test]
    fn base32_符合_rfc4648_大写无填充() {
        assert_eq!(base32_upper(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_upper(b"f"), "MY");
        assert_eq!(base32_upper(b"fo"), "MZXQ");
    }
}
