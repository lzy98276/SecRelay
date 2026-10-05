//! 屏幕与摄像头采集抽象。
//!
//! 这一层只做一件事：**把平台采集 API 归一成"一串 BGRA 帧"**。
//! 编解码、脏矩形差分、渲染都不在这里。
//!
//! # 为什么先做这一层
//!
//! 需求分析 §7 把"桌面文字清晰度"列为**唯一一个"选对库也解决不了、必须自研"**的风险：
//! 通用编码器的率失真优化会主动牺牲文本高频细节，而远程桌面用户是逐字阅读。
//! 要解决它必须在**采集→编码之间**插入变更检测与区域分类，所以采集层的数据结构
//! 从一开始就要能支撑"比较两帧、找出变化区域"，而不是只能拿出一张图。
//!
//! # 平台现状
//!
//! | 平台 | 后端 | 状态 |
//! |---|---|---|
//! | Windows | DXGI Desktop Duplication | ✅ |
//! | Linux X11 | XShm | ⬜ |
//! | Linux Wayland | PipeWire + xdg-desktop-portal | ⬜ |
//! | macOS | ScreenCaptureKit | ⬜ |
//! | 测试/无显示器 | [`SyntheticScreenSource`] | ✅ |

use std::time::{Duration, Instant};

use thiserror::Error;

#[cfg(target_os = "windows")]
pub mod windows_dxgi;

/// 像素格式。
///
/// 只支持一种：**BGRA8**。这是 Windows DXGI 的原生输出格式，也是 macOS/Android
/// 采集链路最容易统一的格式。不做通用图像库——那是另一个项目的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 每像素 4 字节，顺序为 B、G、R、A。
    Bgra8,
}

impl PixelFormat {
    /// 每像素字节数。
    pub fn bytes_per_pixel(self) -> usize {
        4
    }
}

/// 一帧原始画面。
///
/// `data` 是**紧凑排列**的（没有行填充），这样下游可以不关心 pitch 的差异；
/// DXGI 那种带 pitch 的表面在采集层内部就被压平了。
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// 长度必须等于 `width * height * bytes_per_pixel`。
    pub data: Vec<u8>,
    /// 采集时刻（用于统计真实帧率）。
    pub captured_at: Instant,
    /// 自采集源启动以来的帧序号。
    pub sequence: u64,
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFrame")
            .field("size", &format!("{}x{}", self.width, self.height))
            .field("format", &self.format)
            .field("bytes", &self.data.len())
            .field("sequence", &self.sequence)
            .finish()
    }
}

impl VideoFrame {
    /// 构造一帧，并校验缓冲区大小。
    pub fn new(
        width: u32,
        height: u32,
        format: PixelFormat,
        data: Vec<u8>,
        captured_at: Instant,
        sequence: u64,
    ) -> Result<Self, CaptureError> {
        let expected = expected_len(width, height, format);
        if data.len() != expected {
            return Err(CaptureError::BadFrameSize {
                expected,
                actual: data.len(),
            });
        }
        Ok(Self {
            width,
            height,
            format,
            data,
            captured_at,
            sequence,
        })
    }

    /// 每行字节数（紧凑，无填充）。
    pub fn stride(&self) -> usize {
        self.width as usize * self.format.bytes_per_pixel()
    }

    /// 像素总数。
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// 读取某个像素（越界返回 `None`）。
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = y as usize * self.stride() + x as usize * 4;
        self.data
            .get(offset..offset + 4)
            .map(|slice| [slice[0], slice[1], slice[2], slice[3]])
    }

    /// 与另一帧的**不同像素比例**（0.0 ~ 1.0）。
    ///
    /// 这是脏矩形差分的起点：桌面场景下静态画面占比极高，只传变化区域能把码率
    /// 降低 5~50 倍——比"换编解码器"（1.5~2 倍）大一个数量级（需求分析 §6.3）。
    ///
    /// 尺寸或格式不同的两帧视为完全不同（返回 1.0）。
    pub fn diff_ratio(&self, other: &VideoFrame) -> f32 {
        if self.width != other.width
            || self.height != other.height
            || self.format != other.format
        {
            return 1.0;
        }
        let total = self.pixel_count();
        if total == 0 {
            return 0.0;
        }
        let mut changed = 0usize;
        for (a, b) in self.data.chunks_exact(4).zip(other.data.chunks_exact(4)) {
            if a != b {
                changed += 1;
            }
        }
        changed as f32 / total as f32
    }

    /// 是否与另一帧完全相同。
    pub fn is_identical_to(&self, other: &VideoFrame) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.format == other.format
            && self.data == other.data
    }

    /// 内容指纹（FNV-1a 64）。用于快速判重与统计，不是密码学哈希。
    pub fn fingerprint(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in &self.data {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
}

fn expected_len(width: u32, height: u32, format: PixelFormat) -> usize {
    width as usize * height as usize * format.bytes_per_pixel()
}

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
/// `next_frame` 返回 `Ok(None)` 表示"这次没有新画面"（桌面静止时是正常现象，
/// 不是错误）——这个语义很重要，因为桌面场景下大量时间确实没有变化。
pub trait ScreenSource: Send {
    /// 等待并取下一帧。
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, CaptureError>;

    /// 人类可读的描述（用于日志与 UI 显示"正在共享什么"）。
    fn description(&self) -> String;
}

/// 合成画面源：**不采集任何真实屏幕**，只生成已知内容的测试图案。
///
/// 两个用途：
/// 1. 让采集层以上的代码（编码、差分、传输）在无显示器/CI 环境里可测试；
/// 2. 提供**可控的变化区域**，用来验证脏矩形差分实现是否正确。
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
    /// 图案刻意包含**两类区域**，方便下游验证差分逻辑：
    /// - 左上角一个随帧号移动的方块（小范围变化）；
    /// - 底部一条随帧号变化的像素条（细长变化）。
    /// 其余区域保持恒定——模拟"桌面大部分没动"的真实情况。
    pub fn render(&self, sequence: u64) -> VideoFrame {
        let stride = self.width as usize * 4;
        let mut data = vec![0u8; stride * self.height as usize];

        // 恒定背景：缓慢渐变，避免全黑导致编码器行为不真实
        for y in 0..self.height as usize {
            for x in 0..self.width as usize {
                let offset = y * stride + x * 4;
                data[offset] = (x % 251) as u8; // B
                data[offset + 1] = (y % 251) as u8; // G
                data[offset + 2] = 0x80; // R
                data[offset + 3] = 0xFF; // A
            }
        }

        // 移动方块：32x32，沿对角线移动
        let block = 32usize;
        let max_x = (self.width as usize).saturating_sub(block);
        let max_y = (self.height as usize).saturating_sub(block);
        let bx = if max_x == 0 { 0 } else { (sequence as usize * 7) % max_x };
        let by = if max_y == 0 { 0 } else { (sequence as usize * 5) % max_y };
        for y in by..(by + block).min(self.height as usize) {
            for x in bx..(bx + block).min(self.width as usize) {
                let offset = y * stride + x * 4;
                data[offset] = 0x00;
                data[offset + 1] = 0xFF;
                data[offset + 2] = 0xFF;
                data[offset + 3] = 0xFF;
            }
        }

        // 底部状态条：编码帧号，便于肉眼/脚本确认丢帧
        let bar_y = (self.height as usize).saturating_sub(1);
        for bit in 0..64usize {
            let x = bit * 4;
            if x + 3 >= self.width as usize {
                break;
            }
            let on = (sequence >> bit) & 1 == 1;
            let value = if on { 0xFF } else { 0x00 };
            for dx in 0..4 {
                let offset = bar_y * stride + (x + dx) * 4;
                data[offset] = value;
                data[offset + 1] = value;
                data[offset + 2] = value;
                data[offset + 3] = 0xFF;
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
        // 落后太多时重新对齐，避免"追赶"式突发
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
    fn 帧缓冲区大小被校验() {
        let err = VideoFrame::new(2, 2, PixelFormat::Bgra8, vec![0; 15], Instant::now(), 0)
            .expect_err("大小不符应当被拒");
        assert!(matches!(err, CaptureError::BadFrameSize { .. }));

        assert!(VideoFrame::new(2, 2, PixelFormat::Bgra8, vec![0; 16], Instant::now(), 0).is_ok());
    }

    #[test]
    fn 像素读写与步长正确() {
        let frame = SyntheticScreenSource::new(64, 32, 30).render(0);
        assert_eq!(frame.stride(), 64 * 4);
        assert_eq!(frame.pixel_count(), 64 * 32);
        assert_eq!(frame.data.len(), 64 * 32 * 4);

        let pixel = frame.pixel(10, 5).expect("应当取到像素");
        assert_eq!(pixel[3], 0xFF, "alpha 通道应当是不透明");
        assert!(frame.pixel(64, 0).is_none(), "越界应当返回 None");
        assert!(frame.pixel(0, 32).is_none());
    }

    #[test]
    fn 同一帧与自身完全相同() {
        let source = SyntheticScreenSource::new(64, 64, 30);
        let frame = source.render(7);
        assert!(frame.is_identical_to(&frame));
        assert_eq!(frame.diff_ratio(&frame), 0.0);
    }

    #[test]
    fn 相邻帧只有小部分变化() {
        // 这个性质是脏矩形差分能成立的前提：桌面大部分时间没动。
        let source = SyntheticScreenSource::new(320, 240, 30);
        let a = source.render(1);
        let b = source.render(2);

        let ratio = a.diff_ratio(&b);
        assert!(ratio > 0.0, "相邻帧应当有变化");
        assert!(
            ratio < 0.10,
            "变化区域应当很小（实际 {ratio:.4}），否则说明差分方案没有收益"
        );
    }

    #[test]
    fn 相隔较远的帧变化更大() {
        let source = SyntheticScreenSource::new(320, 240, 30);
        let near = source.render(1).diff_ratio(&source.render(2));
        let far = source.render(1).diff_ratio(&source.render(30));
        assert!(far >= near, "远处的帧变化不应更小：near={near}, far={far}");
    }

    #[test]
    fn 尺寸不同的帧视为完全不同() {
        let a = SyntheticScreenSource::new(64, 64, 30).render(0);
        let b = SyntheticScreenSource::new(32, 32, 30).render(0);
        assert_eq!(a.diff_ratio(&b), 1.0);
        assert!(!a.is_identical_to(&b));
    }

    #[test]
    fn 指纹稳定且随内容变化() {
        let source = SyntheticScreenSource::new(64, 64, 30);
        let a = source.render(1);
        let b = source.render(1);
        let c = source.render(2);

        assert_eq!(a.fingerprint(), b.fingerprint(), "相同内容指纹必须相同");
        assert_ne!(a.fingerprint(), c.fingerprint(), "不同内容指纹应当不同");
    }

    #[test]
    fn 合成源按序产帧且序号递增() {
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

    #[test]
    fn 描述包含分辨率与帧率() {
        let source = SyntheticScreenSource::new(1920, 1080, 60);
        let description = source.description();
        assert!(description.contains("1920x1080"), "实际：{description}");
        assert!(description.contains("60fps"), "实际：{description}");
    }
}
