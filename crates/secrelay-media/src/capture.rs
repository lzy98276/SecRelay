//! 采集源接口与错误。

use std::time::Duration;

use thiserror::Error;

use crate::frame::VideoFrame;

/// 采集错误。
#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("帧缓冲区大小不符：期望 {expected} 字节，实际 {actual} 字节")]
    BadFrameSize { expected: usize, actual: usize },

    #[error("在 {0:?} 内没有等到新帧")]
    Timeout(Duration),

    #[error("采集源不支持：{0}")]
    Unsupported(String),

    #[error("平台采集失败：{0}")]
    Platform(String),
}

/// 屏幕采集源。
///
/// `next_frame` 返回 `Ok(None)` 表示"这段时间没有新画面" —— 桌面静止时这是**正常现象**，
/// 不是错误。这个语义很重要：它让上层能把"没变化"和"出错了"分开处理。
pub trait ScreenSource: Send {
    /// 等待并取下一帧。
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, CaptureError>;

    /// 人类可读的描述，用于日志与 UI 显示"正在共享什么"。
    fn description(&self) -> String;
}
