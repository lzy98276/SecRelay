//! 合成画面源：**不采集任何真实屏幕**，只生成已知内容的测试图案。
//!
//! 两个用途：
//!
//! 1. 让采集层以上的代码（编码、差分、传输、UI）在无显示器 / CI 环境里可测试；
//! 2. 提供**变化区域已知且可控**的画面，用来验证脏矩形差分的实现是否正确 ——
//!    真实桌面的变化模式太随机，不适合当测试基准。

use std::time::{Duration, Instant};

use crate::capture::{CaptureError, ScreenSource};
use crate::frame::{PixelFormat, VideoFrame};

/// 移动方块边长。
const BLOCK: usize = 32;
/// 底部状态条的位宽（每 4 像素一位）。
const BAR_BIT_PIXELS: usize = 4;

/// 合成画面源。
pub struct SyntheticScreenSource {
    width: u32,
    height: u32,
    interval: Duration,
    sequence: u64,
    next_at: Instant,
}

impl SyntheticScreenSource {
    /// 按指定分辨率与目标帧率创建。
    pub fn new(width: u32, height: u32, fps: u32) -> Self {
        let fps = fps.max(1);
        Self {
            width,
            height,
            interval: Duration::from_micros(1_000_000 / u64::from(fps)),
            sequence: 0,
            next_at: Instant::now(),
        }
    }

    /// 生成第 `sequence` 帧的图案。
    ///
    /// 图案刻意包含**两类变化区域**，方便下游验证差分逻辑：
    /// - 一个随帧号移动的 32x32 方块（小范围变化）；
    /// - 底部一条编码帧号的细长状态条（便于肉眼/脚本确认有没有丢帧）。
    ///
    /// 其余区域保持恒定 —— 模拟"桌面大部分时间没动"的真实情况。
    pub fn render(&self, sequence: u64) -> VideoFrame {
        let width = self.width as usize;
        let height = self.height as usize;
        let stride = width * 4;
        let mut data = vec![0u8; stride * height];

        // 恒定背景：缓慢渐变。全黑会让编码器行为不真实。
        for y in 0..height {
            for x in 0..width {
                let offset = y * stride + x * 4;
                data[offset] = (x % 251) as u8; // B
                data[offset + 1] = (y % 251) as u8; // G
                data[offset + 2] = 0x80; // R
                data[offset + 3] = 0xFF; // A
            }
        }

        // 移动方块：沿对角线走
        let max_x = width.saturating_sub(BLOCK);
        let max_y = height.saturating_sub(BLOCK);
        let bx = if max_x == 0 { 0 } else { (sequence as usize * 7) % max_x };
        let by = if max_y == 0 { 0 } else { (sequence as usize * 5) % max_y };
        for y in by..(by + BLOCK).min(height) {
            for x in bx..(bx + BLOCK).min(width) {
                let offset = y * stride + x * 4;
                data[offset] = 0x00;
                data[offset + 1] = 0xFF;
                data[offset + 2] = 0xFF;
                data[offset + 3] = 0xFF;
            }
        }

        // 底部状态条：把帧号按位画出来
        if height > 0 {
            let bar_y = height - 1;
            for bit in 0..64usize {
                let x = bit * BAR_BIT_PIXELS;
                if x + BAR_BIT_PIXELS > width {
                    break;
                }
                let value = if (sequence >> bit) & 1 == 1 { 0xFF } else { 0x00 };
                for dx in 0..BAR_BIT_PIXELS {
                    let offset = bar_y * stride + (x + dx) * 4;
                    data[offset] = value;
                    data[offset + 1] = value;
                    data[offset + 2] = value;
                    data[offset + 3] = 0xFF;
                }
            }
        }

        VideoFrame::new(
            self.width,
            self.height,
            PixelFormat::Bgra8,
            data,
            Instant::now(),
            sequence,
        )
        .expect("合成帧的缓冲区大小由构造保证")
    }
}

impl ScreenSource for SyntheticScreenSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, CaptureError> {
        let now = Instant::now();
        if now < self.next_at {
            let wait = self.next_at - now;
            if wait > timeout {
                return Err(CaptureError::Timeout(timeout));
            }
            std::thread::sleep(wait);
        }

        let frame = self.render(self.sequence);
        self.sequence += 1;
        self.next_at += self.interval;

        // 落后太多时重新对齐，避免"追赶"式突发。
        if self.next_at < Instant::now() {
            self.next_at = Instant::now() + self.interval;
        }

        Ok(Some(frame))
    }

    fn description(&self) -> String {
        format!(
            "合成画面源 {}x{} @ {:.0}fps",
            self.width,
            self.height,
            1.0 / self.interval.as_secs_f64()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 相邻帧只有小部分变化() {
        // 这个性质是脏矩形差分能成立的前提：桌面大部分时间没动。
        let source = SyntheticScreenSource::new(320, 240, 30);
        let ratio = source.render(1).diff_ratio(&source.render(2));
        assert!(ratio > 0.0, "相邻帧应当有变化");
        assert!(
            ratio < 0.10,
            "变化区域应当很小（实际 {ratio:.4}），否则合成源不适合验证差分"
        );
    }

    #[test]
    fn 状态条随帧号变化() {
        let source = SyntheticScreenSource::new(64, 8, 30);
        // 第 1 帧与第 2 帧的状态条不同（低位从 1 变 10）
        assert!(!source.render(1).is_identical_to(&source.render(2)));
        // 相同帧号必须完全一致（保证测试可重复）
        assert!(source.render(5).is_identical_to(&source.render(5)));
    }

    #[test]
    fn 分辨率与描述一致() {
        let source = SyntheticScreenSource::new(1920, 1080, 60);
        let frame = source.render(0);
        assert_eq!((frame.width, frame.height), (1920, 1080));
        assert_eq!(frame.byte_len(), 1920 * 1080 * 4);

        let description = source.description();
        assert!(description.contains("1920x1080"), "实际：{description}");
        assert!(description.contains("60fps"), "实际：{description}");
    }

    #[test]
    fn 极小分辨率不崩溃() {
        let source = SyntheticScreenSource::new(1, 1, 30);
        let frame = source.render(3);
        assert_eq!(frame.byte_len(), 4);
    }

    #[test]
    fn 按序产帧且序号递增() {
        let mut source = SyntheticScreenSource::new(64, 64, 240);
        let first = source.next_frame(Duration::from_secs(1)).unwrap().unwrap();
        let second = source.next_frame(Duration::from_secs(1)).unwrap().unwrap();
        assert_eq!(first.sequence, 0);
        assert_eq!(second.sequence, 1);
        assert!(!first.is_identical_to(&second));
    }

    #[test]
    fn 超时语义正确() {
        // 目标帧率极低时，短超时应当返回 Timeout 而不是 None
        let mut source = SyntheticScreenSource::new(16, 16, 1);
        let _ = source.next_frame(Duration::from_secs(1)).unwrap();
        let err = source
            .next_frame(Duration::from_millis(1))
            .expect_err("应当超时");
        assert!(matches!(err, CaptureError::Timeout(_)));
    }
}
