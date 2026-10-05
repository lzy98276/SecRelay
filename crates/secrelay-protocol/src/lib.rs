//! SecRelay 线上协议。
//!
//! 设计依据是 `docs/需求分析.md` §3 的核心抽象：**一条加密通道 + N 种频道**。
//! 屏幕、摄像头、投屏、文件、文字、语音全部是这条通道上的不同频道，
//! 而不是各自一套连接逻辑。
//!
//! 本 crate 只定义**接口与线格式**：不做 I/O、不依赖 UI、不认识 WebRTC。
//! 这样它既能被原生端用，也能编译到 WASM 给 Web 端复用（见需求分析 §6.1 的复用边界）。
//!
//! # 线格式
//!
//! ```text
//! ┌─────────┬──────┬────────────┬─────────────────┐
//! │ channel │ kind │  len (u32) │      body       │
//! │  1 byte │ 1 B  │  4 bytes   │   len bytes     │
//! └─────────┴──────┴────────────┴─────────────────┘
//! ```
//!
//! - `channel`：[`Channel`]，决定这条帧属于哪一路逻辑流。
//! - `kind`：`0` = 控制消息（body 是 UTF-8 JSON），`1` = 原始字节（body 原样透传）。
//! - `len`：大端序，body 字节数。
//!
//! 控制消息用 JSON：量小、可读、便于跨语言；媒体与文件用原始字节：不承担 JSON 的编码开销。

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 协议版本。不兼容时由握手阶段拒绝，见 [`ControlMessage::Hello`]。
pub const PROTOCOL_VERSION: u16 = 1;

/// 单个帧 body 的最大长度（16 MiB）。
///
/// 文件传输不靠单个大帧，而是切成小块走 `File` 频道（见需求分析 §6.3），
/// 所以这个上限只用来防御异常输入。
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// 帧头长度：`channel(1) + kind(1) + len(4)`。
pub const HEADER_LEN: usize = 6;

/// 设备标识：**长期身份公钥的短标识**，不是用户账号。
///
/// 需求分析 §3.1：账号是可选的渐进增强，身份与信任关系由设备密钥与本地信任列表决定。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(String);

impl DeviceId {
    /// 设备 ID 的最大长度。
    pub const MAX_LEN: usize = 64;

    /// 构造并校验设备 ID。
    ///
    /// 允许的字符：ASCII 字母、数字、`-`、`_`。长度 1..=[`DeviceId::MAX_LEN`]。
    pub fn new(raw: impl Into<String>) -> Result<Self, ProtocolError> {
        let raw = raw.into();
        let valid = !raw.is_empty()
            && raw.len() <= Self::MAX_LEN
            && raw
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            return Err(ProtocolError::InvalidDeviceId(raw));
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 会话上的一路逻辑流。对应需求分析 §3 的"三种流"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Channel {
    /// ① 实时媒体：屏幕画面、摄像头、麦克风。低延迟优先，**允许丢帧**。
    Media = 0,
    /// ② 可靠字节流：文件、剪贴板大对象。必达、有序、可分块续传。
    File = 1,
    /// ③ 消息与控制：文字消息、桌面提示、能力协商。必达、有序、量小。
    Control = 2,
}

impl Channel {
    /// 全部频道，便于遍历与协商。
    pub const ALL: [Channel; 3] = [Channel::Media, Channel::File, Channel::Control];

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(value: u8) -> Result<Self, ProtocolError> {
        match value {
            0 => Ok(Channel::Media),
            1 => Ok(Channel::File),
            2 => Ok(Channel::Control),
            other => Err(ProtocolError::UnknownChannel(other)),
        }
    }

    /// 该频道是否容忍丢帧。
    ///
    /// 媒体可以丢（宁可掉帧也不要卡顿），文件与控制不可以。
    pub fn is_lossy(self) -> bool {
        matches!(self, Channel::Media)
    }

    /// 该频道是否只接受结构化控制消息。
    pub fn requires_control_message(self) -> bool {
        matches!(self, Channel::Control)
    }
}

/// 帧载荷类型（线格式里的 `kind` 字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum FrameKind {
    Control = 0,
    Raw = 1,
}

impl FrameKind {
    fn from_u8(value: u8) -> Result<Self, ProtocolError> {
        match value {
            0 => Ok(FrameKind::Control),
            1 => Ok(FrameKind::Raw),
            other => Err(ProtocolError::UnknownKind(other)),
        }
    }
}

/// 控制频道上的结构化消息。
///
/// 握手、频道开关、心跳、错误都在这里。用 serde 的 internally-tagged 表示，
/// 便于跨语言实现（Web 端也要能解析）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// 发起方问候，携带协议版本、自身设备 ID 与能力清单。
    Hello {
        protocol_version: u16,
        device_id: DeviceId,
        capabilities: Vec<String>,
    },
    /// 应答方问候。
    HelloAck {
        protocol_version: u16,
        device_id: DeviceId,
        capabilities: Vec<String>,
    },
    /// 请求打开某一路频道。
    OpenChannel { channel: Channel },
    /// 确认频道已打开。
    ChannelOpened { channel: Channel },
    /// 关闭某一路频道。
    CloseChannel { channel: Channel },
    /// 心跳。
    Ping { nonce: u64 },
    /// 心跳应答。
    Pong { nonce: u64 },
    /// 文字消息（FR-8「传递话语」）。
    ///
    /// 走 `Control` 频道的理由：它是必达、有序、量小的一路流，正好匹配文字语义。
    /// 语音消息与实时对讲不走这里 —— 它们需要独立的音频管线与抖动缓冲。
    Text { body: String },
    /// 有序退出。
    Bye { reason: String },
    /// 协议级错误。
    Error { code: String, message: String },
}

/// 帧载荷。
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    /// 结构化控制消息，只能走 [`Channel::Control`]。
    Control(Box<ControlMessage>),
    /// 原始字节，只能走 [`Channel::Media`] 或 [`Channel::File`]。
    Raw(Vec<u8>),
}

/// 一个协议帧。
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub channel: Channel,
    pub payload: Payload,
}

impl Frame {
    /// 构造控制帧。
    pub fn control(message: ControlMessage) -> Self {
        Self {
            channel: Channel::Control,
            payload: Payload::Control(Box::new(message)),
        }
    }

    /// 构造原始字节帧。
    ///
    /// `Control` 频道必须是控制消息，因此传 `Channel::Control` 会返回错误。
    pub fn raw(channel: Channel, bytes: impl Into<Vec<u8>>) -> Result<Self, ProtocolError> {
        if channel.requires_control_message() {
            return Err(ProtocolError::PayloadMismatch {
                channel,
                kind: "raw",
            });
        }
        let bytes = bytes.into();
        if bytes.len() > MAX_FRAME_LEN {
            return Err(ProtocolError::FrameTooLarge {
                len: bytes.len(),
                max: MAX_FRAME_LEN,
            });
        }
        Ok(Self {
            channel,
            payload: Payload::Raw(bytes),
        })
    }

    /// 编码为线格式字节。
    ///
    /// 原始载荷是**借用**而不是克隆 —— 媒体帧是热路径，每帧 16 MiB 的额外拷贝不可接受。
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;

        // 控制消息需要一份 JSON 暂存；原始载荷直接借用调用方的缓冲区。
        let json_storage;
        let (kind, body): (FrameKind, &[u8]) = match &self.payload {
            Payload::Control(message) => {
                json_storage = serde_json::to_vec(message)
                    .map_err(|e| ProtocolError::ControlCodec(e.to_string()))?;
                (FrameKind::Control, json_storage.as_slice())
            }
            Payload::Raw(bytes) => (FrameKind::Raw, bytes.as_slice()),
        };

        if body.len() > MAX_FRAME_LEN {
            return Err(ProtocolError::FrameTooLarge {
                len: body.len(),
                max: MAX_FRAME_LEN,
            });
        }

        let mut out = Vec::with_capacity(HEADER_LEN + body.len());
        out.push(self.channel.as_u8());
        out.push(kind as u8);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(body);
        Ok(out)
    }

    /// 校验频道与载荷类型的组合是否合法。
    fn validate(&self) -> Result<(), ProtocolError> {
        match (&self.payload, self.channel) {
            (Payload::Control(_), Channel::Control) => Ok(()),
            (Payload::Raw(_), channel) if !channel.requires_control_message() => Ok(()),
            (_, channel) => Err(ProtocolError::PayloadMismatch {
                channel,
                kind: match &self.payload {
                    Payload::Control(_) => "control",
                    Payload::Raw(_) => "raw",
                },
            }),
        }
    }

    /// 从线格式字节解码。`bytes` 必须恰好是一个完整帧。
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < HEADER_LEN {
            return Err(ProtocolError::Truncated {
                expected: HEADER_LEN,
                actual: bytes.len(),
            });
        }

        let channel = Channel::from_u8(bytes[0])?;
        let kind = FrameKind::from_u8(bytes[1])?;
        let len = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]) as usize;

        if len > MAX_FRAME_LEN {
            return Err(ProtocolError::FrameTooLarge {
                len,
                max: MAX_FRAME_LEN,
            });
        }

        let body = &bytes[HEADER_LEN..];
        if body.len() != len {
            return Err(ProtocolError::Truncated {
                expected: HEADER_LEN + len,
                actual: bytes.len(),
            });
        }

        let payload = match kind {
            FrameKind::Control => {
                if !channel.requires_control_message() {
                    return Err(ProtocolError::PayloadMismatch {
                        channel,
                        kind: "control",
                    });
                }
                let message: ControlMessage = serde_json::from_slice(body)
                    .map_err(|e| ProtocolError::ControlCodec(e.to_string()))?;
                Payload::Control(Box::new(message))
            }
            FrameKind::Raw => {
                if channel.requires_control_message() {
                    return Err(ProtocolError::PayloadMismatch {
                        channel,
                        kind: "raw",
                    });
                }
                Payload::Raw(body.to_vec())
            }
        };

        Ok(Self { channel, payload })
    }

    /// 这是否是一个控制消息。
    pub fn as_control(&self) -> Option<&ControlMessage> {
        match &self.payload {
            Payload::Control(message) => Some(message),
            Payload::Raw(_) => None,
        }
    }

    /// 这是否是原始字节。
    pub fn as_raw(&self) -> Option<&[u8]> {
        match &self.payload {
            Payload::Raw(bytes) => Some(bytes),
            Payload::Control(_) => None,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("帧过大：{len} 字节，上限 {max} 字节")]
    FrameTooLarge { len: usize, max: usize },

    #[error("帧不完整：期望 {expected} 字节，实际 {actual} 字节")]
    Truncated { expected: usize, actual: usize },

    #[error("未知的频道编号 {0}")]
    UnknownChannel(u8),

    #[error("未知的载荷类型编号 {0}")]
    UnknownKind(u8),

    #[error("控制消息编解码失败：{0}")]
    ControlCodec(String),

    #[error("载荷类型与频道不匹配：{channel:?} 不接受 {kind} 载荷")]
    PayloadMismatch {
        channel: Channel,
        kind: &'static str,
    },

    #[error("设备 ID 非法（必须是 1~64 位 ASCII 字母/数字/`-`/`_`）：{0}")]
    InvalidDeviceId(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> ControlMessage {
        ControlMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId::new("dev-abc123").unwrap(),
            capabilities: vec!["screen".into(), "camera".into()],
        }
    }

    #[test]
    fn 控制帧往返一致() {
        let frame = Frame::control(hello());
        let encoded = frame.encode().expect("编码应当成功");
        assert_eq!(encoded[0], Channel::Control.as_u8());
        let decoded = Frame::decode(&encoded).expect("解码应当成功");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn 原始帧往返一致() {
        let frame = Frame::raw(Channel::Media, vec![0u8, 1, 2, 255]).unwrap();
        let encoded = frame.encode().unwrap();
        assert_eq!(encoded[0], Channel::Media.as_u8());
        assert_eq!(encoded[1], FrameKind::Raw as u8);
        let decoded = Frame::decode(&encoded).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(decoded.as_raw(), Some(&[0u8, 1, 2, 255][..]));
    }

    #[test]
    fn 文件频道可以承载原始字节() {
        let frame = Frame::raw(Channel::File, vec![9u8; 1024]).unwrap();
        let decoded = Frame::decode(&frame.encode().unwrap()).unwrap();
        assert_eq!(decoded.channel, Channel::File);
        assert_eq!(decoded.as_raw().unwrap().len(), 1024);
    }

    #[test]
    fn 控制频道不接受原始字节() {
        let err = Frame::raw(Channel::Control, vec![1, 2, 3]).unwrap_err();
        assert_eq!(
            err,
            ProtocolError::PayloadMismatch {
                channel: Channel::Control,
                kind: "raw",
            }
        );
    }

    #[test]
    fn 媒体频道不接受控制消息() {
        // 手工构造一个非法组合，确认 encode 会拒绝。
        let frame = Frame {
            channel: Channel::Media,
            payload: Payload::Control(Box::new(hello())),
        };
        assert!(matches!(
            frame.encode(),
            Err(ProtocolError::PayloadMismatch { .. })
        ));
    }

    #[test]
    fn 空载荷原始帧合法() {
        let frame = Frame::raw(Channel::Media, Vec::new()).unwrap();
        let decoded = Frame::decode(&frame.encode().unwrap()).unwrap();
        assert_eq!(decoded.as_raw(), Some(&[][..]));
    }

    #[test]
    fn 截断的帧被拒绝() {
        let frame = Frame::control(hello());
        let encoded = frame.encode().unwrap();

        assert!(matches!(
            Frame::decode(&encoded[..3]),
            Err(ProtocolError::Truncated { .. })
        ));
        // 头部声称的长度与实际不符
        assert!(matches!(
            Frame::decode(&encoded[..encoded.len() - 1]),
            Err(ProtocolError::Truncated { .. })
        ));
    }

    #[test]
    fn 未知频道与未知载荷被拒绝() {
        let mut bytes = Frame::control(hello()).encode().unwrap();
        bytes[0] = 99;
        assert_eq!(Frame::decode(&bytes), Err(ProtocolError::UnknownChannel(99)));

        let mut bytes = Frame::control(hello()).encode().unwrap();
        bytes[1] = 99;
        assert_eq!(Frame::decode(&bytes), Err(ProtocolError::UnknownKind(99)));
    }

    #[test]
    fn 声明超长的帧被拒绝() {
        // 头部声称 16 MiB + 1
        let mut bytes = vec![Channel::Media.as_u8(), FrameKind::Raw as u8];
        bytes.extend_from_slice(&((MAX_FRAME_LEN as u32) + 1).to_be_bytes());
        assert!(matches!(
            Frame::decode(&bytes),
            Err(ProtocolError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn 频道属性符合设计() {
        assert!(Channel::Media.is_lossy(), "媒体可以丢帧");
        assert!(!Channel::File.is_lossy(), "文件不能丢");
        assert!(!Channel::Control.is_lossy(), "控制不能丢");
        assert!(Channel::Control.requires_control_message());
        assert_eq!(Channel::ALL.len(), 3);
    }

    #[test]
    fn 设备_id_校验() {
        assert!(DeviceId::new("dev_01").is_ok());
        assert!(DeviceId::new("").is_err());
        assert!(DeviceId::new("有中文").is_err());
        assert!(DeviceId::new("a".repeat(DeviceId::MAX_LEN)).is_ok());
        assert!(DeviceId::new("a".repeat(DeviceId::MAX_LEN + 1)).is_err());
    }

    #[test]
    fn 全部控制消息都能往返() {
        let messages = vec![
            hello(),
            ControlMessage::HelloAck {
                protocol_version: PROTOCOL_VERSION,
                device_id: DeviceId::new("dev-xyz").unwrap(),
                capabilities: vec![],
            },
            ControlMessage::OpenChannel {
                channel: Channel::Media,
            },
            ControlMessage::ChannelOpened {
                channel: Channel::File,
            },
            ControlMessage::CloseChannel {
                channel: Channel::Control,
            },
            ControlMessage::Ping { nonce: 42 },
            ControlMessage::Pong { nonce: 42 },
            ControlMessage::Text {
                body: "跨设备连接，让看、传、说归于一处".into(),
            },
            ControlMessage::Bye {
                reason: "用户主动断开".into(),
            },
            ControlMessage::Error {
                code: "e_test".into(),
                message: "测试".into(),
            },
        ];

        for message in messages {
            let encoded = Frame::control(message.clone()).encode().unwrap();
            let decoded = Frame::decode(&encoded).unwrap();
            assert_eq!(decoded.as_control(), Some(&message));
        }
    }
}
