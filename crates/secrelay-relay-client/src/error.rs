//! 中继客户端的错误类型。
//!
//! 错误分两类：一类是用户输入的问题（基址写错），需要原样告诉用户；
//! 一类是网络或对端的问题，界面只展示"连不上/响应不对"。

use thiserror::Error;

/// 中继客户端的错误。
#[derive(Debug, Error)]
pub enum Error {
    #[error("中继地址为空")]
    EmptyBaseUrl,

    #[error("中继地址缺少协议前缀（形如 https://relay.example.com），实际是 `{found}`")]
    MissingScheme { found: String },

    #[error("只支持 http 与 https，不支持 `{scheme}`")]
    UnsupportedScheme { scheme: String },

    #[error("中继地址缺少主机名")]
    EmptyHost,

    #[error("主机名不合法：`{host}`")]
    InvalidHost { host: String },

    #[error("端口不合法：`{port}`")]
    InvalidPort { port: String },

    #[error("端口为空：`{host}`")]
    EmptyPort { host: String },

    #[error("HTTP 请求失败：{0}")]
    Http(String),

    #[error("服务返回状态 {status}：{body}")]
    Status { status: u16, body: String },

    #[error("响应格式不对：{0}")]
    Decode(String),

    #[error("信令 URL 不合法：`{url}`")]
    BadSignalingUrl { url: String },

    #[error("WebSocket 出错：{0}")]
    WebSocket(String),
}

impl Error {
    /// 转成给用户看的一句话。
    pub fn message(&self) -> String {
        self.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 每种错误的文案都非空且不含未完成标记() {
        let samples = [
            Error::EmptyBaseUrl,
            Error::MissingScheme {
                found: "x".into(),
            },
            Error::UnsupportedScheme {
                scheme: "ftp".into(),
            },
            Error::EmptyHost,
            Error::InvalidHost { host: "x".into() },
            Error::InvalidPort { port: "x".into() },
            Error::EmptyPort { host: "x".into() },
            Error::Http("x".into()),
            Error::Status {
                status: 500,
                body: "x".into(),
            },
            Error::Decode("x".into()),
            Error::BadSignalingUrl { url: "x".into() },
            Error::WebSocket("x".into()),
        ];
        for error in samples {
            let text = error.message();
            assert!(!text.trim().is_empty(), "{error:?}");
            assert!(!text.contains("TODO") && !text.contains("FIXME"), "{text}");
        }
    }

    #[test]
    fn 协议错误里带上用户输入() {
        let text = Error::UnsupportedScheme {
            scheme: "ftp".into(),
        }
        .message();
        assert!(text.contains("ftp"), "{text}");
        assert!(text.contains("http"), "{text}");
    }
}
