//! 帧表示。
//!
//! 只支持 **BGRA8** 一种格式：它是 Windows DXGI 的原生输出，也是 macOS / Android
//! 采集链路最容易统一的格式。我们不做通用图像库 —— 那是另一个项目的事。

use std::time::Instant;

use crate::capture::CaptureError;

/// 像素格式。
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
/// `data` 是**紧凑排列**的（没有行填充），这样下游可以不关心不同采集 API 的 pitch 差异；
/// DXGI 那种带 pitch 的表面在采集层内部就被压平了。
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// 长度必须等于 `width * height * bytes_per_pixel`。
    pub data: Vec<u8>,
    /// 采集时刻，用于统计真实帧率。
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

    /// 未压缩字节数。
    pub fn byte_len(&self) -> usize {
        self.data.len()
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
    /// 这是脏矩形差分的起点：桌面场景下静态画面占比很高，只传变化区域能显著降低码率。
    ///
    /// ⚠️ 实测表明这个比例**高度依赖画面内容**：近乎静止的桌面约 0.02%，
    /// 而终端持续打印时可到 0.5% 平均 / 31% 峰值（见 `docs/measurements.md`）。
    /// 所以调用方必须准备好在比例过高时退回整帧编码。
    ///
    /// 尺寸或格式不同的两帧视为完全不同（返回 1.0）。
    pub fn diff_ratio(&self, other: &VideoFrame) -> f32 {
        if self.width != other.width || self.height != other.height || self.format != other.format
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

    /// 内容指纹（FNV-1a 64）。用于快速判重与统计，**不是密码学哈希**。
    pub fn fingerprint(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in &self.data {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
}

/// 给定尺寸与格式所需的字节数。
pub fn expected_len(width: u32, height: u32, format: PixelFormat) -> usize {
    width as usize * height as usize * format.bytes_per_pixel()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, fill: u8, sequence: u64) -> VideoFrame {
        let len = expected_len(width, height, PixelFormat::Bgra8);
        VideoFrame::new(
            width,
            height,
            PixelFormat::Bgra8,
            vec![fill; len],
            Instant::now(),
            sequence,
        )
        .unwrap()
    }

    #[test]
    fn 帧缓冲区大小被校验() {
        let err = VideoFrame::new(2, 2, PixelFormat::Bgra8, vec![0; 15], Instant::now(), 0)
            .expect_err("大小不符应当被拒");
        assert!(matches!(err, CaptureError::BadFrameSize { .. }));

        assert!(VideoFrame::new(2, 2, PixelFormat::Bgra8, vec![0; 16], Instant::now(), 0).is_ok());
    }

    #[test]
    fn 步长与像素读取正确() {
        let f = frame(64, 32, 0x11, 0);
        assert_eq!(f.stride(), 64 * 4);
        assert_eq!(f.pixel_count(), 64 * 32);
        assert_eq!(f.byte_len(), 64 * 32 * 4);

        let pixel = f.pixel(10, 5).expect("应当取到像素");
        assert_eq!(pixel, [0x11, 0x11, 0x11, 0x11]);
        assert!(f.pixel(64, 0).is_none(), "越界应当返回 None");
        assert!(f.pixel(0, 32).is_none());
    }

    #[test]
    fn 同一帧与自身完全相同() {
        let f = frame(8, 8, 3, 0);
        assert!(f.is_identical_to(&f));
        assert_eq!(f.diff_ratio(&f), 0.0);
    }

    #[test]
    fn 尺寸不同视为完全不同() {
        let a = frame(64, 64, 0, 0);
        let b = frame(32, 32, 0, 0);
        assert_eq!(a.diff_ratio(&b), 1.0);
        assert!(!a.is_identical_to(&b));
    }

    #[test]
    fn 差分比例随变化像素数线性() {
        let mut a = frame(10, 10, 0, 0); // 100 像素
        let b = frame(10, 10, 0, 1);
        // 改 10 个像素 -> 10%
        for i in 0..40 {
            a.data[i] = 0xFF;
        }
        let ratio = a.diff_ratio(&b);
        assert!((ratio - 0.10).abs() < 1e-6, "实际 {ratio}");
    }

    #[test]
    fn 指纹稳定且随内容变化() {
        let a = frame(8, 8, 1, 0);
        let b = frame(8, 8, 1, 1);
        let c = frame(8, 8, 2, 2);
        assert_eq!(a.fingerprint(), b.fingerprint(), "相同内容指纹必须相同");
        assert_ne!(a.fingerprint(), c.fingerprint(), "不同内容指纹应当不同");
    }

    #[test]
    fn 零尺寸帧不崩溃() {
        let f = frame(0, 0, 0, 0);
        assert_eq!(f.pixel_count(), 0);
        assert_eq!(f.diff_ratio(&f), 0.0);
    }
}
