//! HTTP 传输：http 直接连，https 用 rustls 加上内置根证书。
//!
//! 只实现本项目需要的部分：一次 `GET`、读完整响应、`Connection: close`。
//! 没有重定向、没有压缩、没有连接池 —— 发现与探活各发一次请求而已。

use std::sync::{Arc, OnceLock};

use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tokio_rustls::TlsConnector;

use crate::error::Error;

/// 单次请求的总超时。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// 响应体上限，避免对端一直灌数据。
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// 进程内共享的 TLS 配置。
///
/// 只建一次：根证书表解析一遍就够，每次请求重建纯属浪费。
pub fn client_config() -> Result<Arc<ClientConfig>, Error> {
    static CONFIG: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();

    CONFIG
        .get_or_init(|| build_client_config().map_err(|err| err.to_string()))
        .clone()
        .map_err(Error::Http)
}

fn build_client_config() -> Result<Arc<ClientConfig>, Error> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if roots.is_empty() {
        return Err(Error::Http("内置根证书为空".to_string()));
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// 一次 HTTP 响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// 发一次 `GET`。
pub async fn get(url: &str, accept: &str, user_agent: &str) -> Result<HttpResponse, Error> {
    let target = Target::parse(url)?;
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nAccept: {accept}\r\nUser-Agent: {user_agent}\r\n\
         Connection: close\r\n\r\n",
        target.path, target.host_header
    );

    timeout(REQUEST_TIMEOUT, send(&target, request.as_bytes()))
        .await
        .map_err(|_| Error::Http(format!("请求超时（{} 秒）", REQUEST_TIMEOUT.as_secs())))?
}

/// 发一次 `GET` 并反序列化 JSON body。
pub async fn get_json<T: serde::de::DeserializeOwned>(
    url: &str,
    user_agent: &str,
) -> Result<T, Error> {
    let response = get(url, "application/json", user_agent).await?;
    if response.status != 200 {
        return Err(Error::Status {
            status: response.status,
            body: truncate(&response.body, 200),
        });
    }
    serde_json::from_str(&response.body)
        .map_err(|err| Error::Decode(format!("{err}：{}", truncate(&response.body, 200))))
}

async fn send(target: &Target, request: &[u8]) -> Result<HttpResponse, Error> {
    let stream = TcpStream::connect((target.host.as_str(), target.port))
        .await
        .map_err(|err| {
            Error::Http(format!(
                "连接 {}:{} 失败：{err}",
                target.host, target.port
            ))
        })?;

    if target.tls {
        let connector = TlsConnector::from(client_config()?);
        let name = ServerName::try_from(target.host.clone()).map_err(|_| Error::InvalidHost {
            host: target.host.clone(),
        })?;
        let tls = connector
            .connect(name, stream)
            .await
            .map_err(|err| Error::Http(format!("TLS 握手失败：{err}")))?;
        exchange(&mut Box::new(tls), request).await
    } else {
        exchange(&mut Box::new(stream), request).await
    }
}

/// 把请求写出去，再读到连接结束。
async fn exchange<S>(stream: &mut S, request: &[u8]) -> Result<HttpResponse, Error>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + ?Sized,
{
    stream
        .write_all(request)
        .await
        .map_err(|err| Error::Http(format!("发送请求失败：{err}")))?;
    stream
        .flush()
        .await
        .map_err(|err| Error::Http(format!("发送请求失败：{err}")))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .map_err(|err| Error::Http(format!("读取响应失败：{err}")))?;
    parse_response(&raw)
}

/// 请求目标的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    tls: bool,
    host: String,
    port: u16,
    /// `Host` 头，非默认端口时带上端口。
    host_header: String,
    /// 请求行里的路径。
    path: String,
}

impl Target {
    fn parse(url: &str) -> Result<Self, Error> {
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| Error::Http(format!("URL 缺少协议：`{url}`")))?;
        let tls = match scheme {
            "http" => false,
            "https" => true,
            other => {
                return Err(Error::UnsupportedScheme {
                    scheme: other.to_string(),
                })
            }
        };

        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, "/"),
        };

        let (host, port) = match authority.rsplit_once(':') {
            // 端口只在最后一段且全为数字时才算端口，避免把 IPv6 的冒号当分隔符。
            Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => (
                host.to_string(),
                port.parse::<u16>()
                    .map_err(|_| Error::InvalidPort { port: port.to_string() })?,
            ),
            _ => (
                authority.to_string(),
                if tls { 443 } else { 80 },
            ),
        };
        if host.is_empty() {
            return Err(Error::EmptyHost);
        }
        let host_header = authority.to_string();

        Ok(Self {
            tls,
            host,
            port,
            host_header,
            path: path.to_string(),
        })
    }
}

/// 解析 HTTP/1.x 响应报文。
fn parse_response(raw: &[u8]) -> Result<HttpResponse, Error> {
    let head_end = find_head_end(raw).ok_or_else(|| Error::Http("响应头不完整".to_string()))?;
    let head = String::from_utf8_lossy(&raw[..head_end]);
    let mut lines = head.split("\r\n");

    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| Error::Http(format!("状态行无法解析：`{}`", truncate(status_line, 80))))?;

    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        }
    }

    let body_bytes = &raw[head_end..];
    let body = if chunked {
        decode_chunked(body_bytes)?
    } else {
        String::from_utf8_lossy(body_bytes).into_owned()
    };

    Ok(HttpResponse { status, body })
}

/// 找到响应头与响应体的分界点，返回响应体的起始下标。
fn find_head_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

/// 解开 chunked 传输编码。
fn decode_chunked(raw: &[u8]) -> Result<String, Error> {
    let mut out = Vec::new();
    let mut pos = 0;

    loop {
        let line_end = raw[pos..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| Error::Decode("chunked 长度行不完整".to_string()))?
            + pos;
        let line = String::from_utf8_lossy(&raw[pos..line_end]);
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| Error::Decode(format!("chunked 长度无法解析：`{size_text}`")))?;
        pos = line_end + 2;

        if size == 0 {
            break;
        }
        if pos + size > raw.len() {
            return Err(Error::Decode("chunked 数据不完整".to_string()));
        }
        out.extend_from_slice(&raw[pos..pos + size]);
        pos += size + 2;
        if out.len() > MAX_BODY_BYTES {
            return Err(Error::Decode("响应体过大".to_string()));
        }
    }

    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        text.chars().take(limit).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析请求目标() {
        let target = Target::parse("https://relay.example.com/api/v1/relay").unwrap();
        assert!(target.tls);
        assert_eq!(target.host, "relay.example.com");
        assert_eq!(target.port, 443);
        assert_eq!(target.host_header, "relay.example.com");
        assert_eq!(target.path, "/api/v1/relay");

        let local = Target::parse("http://127.0.0.1:8080/healthz").unwrap();
        assert!(!local.tls);
        assert_eq!(local.port, 8080);
        assert_eq!(local.host_header, "127.0.0.1:8080");

        let bare = Target::parse("http://relay.example.com").unwrap();
        assert_eq!(bare.path, "/");
        assert_eq!(bare.port, 80);
    }

    #[test]
    fn 请求目标拒绝非法输入() {
        assert!(Target::parse("ftp://relay.example.com").is_err());
        assert!(Target::parse("relay.example.com").is_err());
        assert!(Target::parse("https://:443/x").is_err());
    }

    #[test]
    fn 解析带内容长度的响应() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"a\":1}";
        let response = parse_response(raw).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"a\":1}");
    }

    #[test]
    fn 解析_chunked_响应() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n7\r\n{\"a\":1}\r\n0\r\n\r\n";
        let response = parse_response(raw).unwrap();
        assert_eq!(response.body, "{\"a\":1}");
    }

    #[test]
    fn 头部字段名大小写不敏感() {
        let raw = b"HTTP/1.1 200 OK\r\nTRANSFER-ENCODING: Chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n";
        assert_eq!(parse_response(raw).unwrap().body, "abc");
    }

    #[test]
    fn 非_200_状态照样解析出来() {
        let raw = b"HTTP/1.1 503 Service Unavailable\r\n\r\ndown";
        let response = parse_response(raw).unwrap();
        assert_eq!(response.status, 503);
        assert_eq!(response.body, "down");
    }

    #[test]
    fn 报文残缺时报错而不是panic() {
        assert!(parse_response(b"HTTP/1.1 200 OK").is_err());
        assert!(parse_response(b"garbage\r\n\r\n").is_err());
        assert!(decode_chunked(b"zz\r\n").is_err());
        assert!(decode_chunked(b"5\r\nab\r\n").is_err());
    }
}
