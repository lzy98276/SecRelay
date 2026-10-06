//! 建连失败的原因。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("中继地址不合法：{0}")]
    BadEndpoint(String),

    #[error("没能从中继拿到配置：{0}")]
    Discovery(String),

    #[error("中继上报的短 ID 与本地算出来的不一致：本地 {local}，中继自称 {advertised}")]
    RelayIdMismatch { local: String, advertised: String },

    #[error("中继上报的信令协议版本 {remote} 高于本端认识的 {local}")]
    SignalingVersion { remote: u32, local: u32 },

    #[error("连接信令失败：{0}")]
    Signaling(String),

    #[error("会话码必须是非空的十六进制短码，实际是 `{0}`")]
    BadSessionCode(String),

    #[error("等待对端加入会话超时（{0:?}）")]
    PeerJoinTimeout(std::time::Duration),

    #[error("等待对端 offer 超时（{0:?}）")]
    OfferTimeout(std::time::Duration),

    #[error("等待对端 answer 超时（{0:?}）")]
    AnswerTimeout(std::time::Duration),

    #[error("SDP 载荷不合法：{0}")]
    BadSdp(String),

    #[error("建立传输失败：{0}")]
    Transport(String),

    #[error("会话握手失败：{0}")]
    Session(String),

    /// 直连失败后要回退中继，但中继不可用。
    #[error("直连失败（{direct}），且没有可用的中继：{reason}")]
    NoRelayFallback { direct: String, reason: String },
}

impl ConnectionError {
    /// 给用户看的一句话。
    pub fn message(&self) -> String {
        self.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn 每种错误的文案都非空且带上细节() {
        let samples = [
            ConnectionError::BadEndpoint("x".into()),
            ConnectionError::Discovery("x".into()),
            ConnectionError::RelayIdMismatch {
                local: "AAAA".into(),
                advertised: "BBBB".into(),
            },
            ConnectionError::SignalingVersion {
                remote: 9,
                local: 1,
            },
            ConnectionError::Signaling("x".into()),
            ConnectionError::BadSessionCode("x".into()),
            ConnectionError::PeerJoinTimeout(Duration::from_secs(1)),
            ConnectionError::OfferTimeout(Duration::from_secs(1)),
            ConnectionError::AnswerTimeout(Duration::from_secs(1)),
            ConnectionError::BadSdp("x".into()),
            ConnectionError::Transport("x".into()),
            ConnectionError::Session("x".into()),
            ConnectionError::NoRelayFallback {
                direct: "x".into(),
                reason: "y".into(),
            },
        ];
        for error in samples {
            let text = error.message();
            assert!(!text.trim().is_empty(), "{error:?}");
            assert!(!text.contains("TODO") && !text.contains("FIXME"), "{text}");
        }
    }

    #[test]
    fn 短_id_不符时两个值都写进文案() {
        let text = ConnectionError::RelayIdMismatch {
            local: "LOCALID".into(),
            advertised: "ADVID".into(),
        }
        .message();
        assert!(text.contains("LOCALID") && text.contains("ADVID"), "{text}");
    }
}
