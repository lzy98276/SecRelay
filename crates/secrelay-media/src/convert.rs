//! 像素格式转换与整数倍降采样。
//!
//! 采集后端统一产出 **BGRA** 帧（DXGI 的原生输出），但两个下游需要 **RGBA**：
//! PNG 编码器与 UI 的视频画面。所以转换只写一次，两边共用 —— 包括降采样的盒式滤波。
//!
//! 降采样是**整数倍**的，刻意不支持任意比例：整数倍的实现与验证都简单得多，
//! 而我们的用途（文档配图、预览画面）对比例没有苛刻要求。
//!
//! ⚠️ 这条路径是**纯 CPU 的**：每次预览都要在 CPU 上读满整帧、做盒式平均、
//! 再上传给 UI。`docs/measurements.md` 记录了它的开销量级；真正的产品路径是
//! GPU 纹理零拷贝，这个模块是"先把画面显示出来"的过渡方案。

use thiserror::Error;

use crate::frame::{PixelFormat, VideoFrame};

#[derive(Debug, Error)]
pub enum ConvertError {
    #[error("缩放倍数必须大于 0")]
    BadScale,

    #[error("帧尺寸为 0，无法转换")]
    EmptyFrame,

    #[error("不支持的像素格式：{0:?}")]
    UnsupportedFormat(PixelFormat),
}

/// 紧凑排列的 RGBA8 图像。
#[derive(Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// 长度 = `width * height * 4`。
    pub data: Vec<u8>,
}

impl std::fmt::Debug for RgbaImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RgbaImage")
            .field("size", &format!("{}x{}", self.width, self.height))
            .field("bytes", &self.data.len())
            .finish()
    }
}

impl RgbaImage {
    pub fn byte_len(&self) -> usize {
        self.data.len()
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = (y as usize * self.width as usize + x as usize) * 4;
        self.data
            .get(offset..offset + 4)
            .map(|p| [p[0], p[1], p[2], p[3]])
    }
}

/// 把 BGRA 帧转成 RGBA，并按整数倍 `scale` 盒式降采样。
///
/// `scale = 1` 时同时完成通道重排（BGRA → RGBA）。
///
/// **alpha 一律写成 255。** 桌面采集给出来的 alpha 通道没有意义，
/// 直接把 DXGI 的原始值透传下去会让画面整体透明（黑色背景上看着像"没渲染"）。
pub fn to_rgba_scaled(frame: &VideoFrame, scale: u32) -> Result<RgbaImage, ConvertError> {
    if scale == 0 {
        return Err(ConvertError::BadScale);
    }
    if frame.width == 0 || frame.height == 0 {
        return Err(ConvertError::EmptyFrame);
    }
    if frame.format != PixelFormat::Bgra8 {
        return Err(ConvertError::UnsupportedFormat(frame.format));
    }

    let out_width = frame.width.div_ceil(scale);
    let out_height = frame.height.div_ceil(scale);
    let mut data = vec![0u8; out_width as usize * out_height as usize * 4];
    let stride = frame.stride();

    for out_y in 0..out_height {
        for out_x in 0..out_width {
            let out_offset = (out_y as usize * out_width as usize + out_x as usize) * 4;

            if scale == 1 {
                let src = out_y as usize * stride + out_x as usize * 4;
                let px = &frame.data[src..src + 4];
                data[out_offset] = px[2]; // R
                data[out_offset + 1] = px[1]; // G
                data[out_offset + 2] = px[0]; // B
                data[out_offset + 3] = 0xFF; // A
                continue;
            }

            let mut sum = [0u32; 3];
            let mut count = 0u32;
            for dy in 0..scale {
                let y = out_y * scale + dy;
                if y >= frame.height {
                    break;
                }
                for dx in 0..scale {
                    let x = out_x * scale + dx;
                    if x >= frame.width {
                        break;
                    }
                    let src = y as usize * stride + x as usize * 4;
                    let px = &frame.data[src..src + 4];
                    sum[0] += u32::from(px[2]); // R
                    sum[1] += u32::from(px[1]); // G
                    sum[2] += u32::from(px[0]); // B
                    count += 1;
                }
            }

            if count == 0 {
                continue; // 全透明黑，data 已是 0
            }
            data[out_offset] = (sum[0] / count) as u8;
            data[out_offset + 1] = (sum[1] / count) as u8;
            data[out_offset + 2] = (sum[2] / count) as u8;
            data[out_offset + 3] = 0xFF;
        }
    }

    Ok(RgbaImage {
        width: out_width,
        height: out_height,
        data,
    })
}

/// 选择一个合适的整数降采样倍数，让宽度不超过 `max_width`（至少为 1）。
///
/// UI 预览用它把 2560 宽的桌面压到能接受的 CPU 上传量 —— 这一层的开销
/// 是零拷贝方案落地前的主要瓶颈，见 `docs/measurements.md`。
pub fn scale_for_width(width: u32, max_width: u32) -> u32 {
    if width <= max_width || max_width == 0 {
        return 1;
    }
    width.div_ceil(max_width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn frame(width: u32, height: u32, pick: impl Fn(u32, u32) -> (u8, u8, u8)) -> VideoFrame {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let (r, g, b) = pick(x, y);
                data.extend_from_slice(&[b, g, r, 0x00]); // alpha 故意给 0，验证会被覆盖
            }
        }
        VideoFrame::new(width, height, PixelFormat::Bgra8, data, Instant::now(), 0).unwrap()
    }

    #[test]
    fn 通道顺序从_bgra_变为_rgba() {
        let f = frame(1, 1, |_, _| (255, 0, 0)); // 纯红
        let img = to_rgba_scaled(&f, 1).unwrap();
        assert_eq!(img.pixel(0, 0), Some([255, 0, 0, 255]));
    }

    #[test]
    fn alpha_被强制为不透明() {
        // 源 alpha 是 0，输出必须是 255，否则预览会整屏透明
        let f = frame(2, 2, |_, _| (10, 20, 30));
        let img = to_rgba_scaled(&f, 1).unwrap();
        assert_eq!(img.pixel(0, 0).unwrap()[3], 255);
        assert_eq!(img.pixel(1, 1).unwrap()[3], 255);
    }

    #[test]
    fn 缩放一倍时尺寸与源一致() {
        let f = frame(8, 4, |_, _| (1, 2, 3));
        let img = to_rgba_scaled(&f, 1).unwrap();
        assert_eq!((img.width, img.height), (8, 4));
        assert_eq!(img.byte_len(), 8 * 4 * 4);
    }

    #[test]
    fn 整数倍降采样尺寸正确且向上取整() {
        let f = frame(8, 4, |_, _| (0, 0, 0));
        assert_eq!(to_rgba_scaled(&f, 2).unwrap().width, 4);

        let odd = frame(7, 5, |_, _| (0, 0, 0));
        let img = to_rgba_scaled(&odd, 2).unwrap();
        assert_eq!((img.width, img.height), (4, 3), "7/2→4，5/2→3");
    }

    #[test]
    fn 盒式降采样取平均() {
        // 2x2 四个像素：红、绿、蓝、白 → 平均应为 (170,170,170) 附近
        let mut data = Vec::new();
        for (r, g, b) in [(255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 255)] {
            data.extend_from_slice(&[b, g, r, 0xFF]);
        }
        let f = VideoFrame::new(2, 2, PixelFormat::Bgra8, data, Instant::now(), 0).unwrap();
        let img = to_rgba_scaled(&f, 2).unwrap();
        assert_eq!((img.width, img.height), (1, 1));
        // (255+0+0+255)/4 = 127.5 → 127
        assert_eq!(img.pixel(0, 0).unwrap(), [127, 127, 127, 255]);
    }

    #[test]
    fn 非法输入被拒() {
        let f = frame(4, 4, |_, _| (0, 0, 0));
        assert!(matches!(
            to_rgba_scaled(&f, 0),
            Err(ConvertError::BadScale)
        ));

        let empty = VideoFrame::new(0, 0, PixelFormat::Bgra8, vec![], Instant::now(), 0).unwrap();
        assert!(matches!(
            to_rgba_scaled(&empty, 1),
            Err(ConvertError::EmptyFrame)
        ));
    }

    #[test]
    fn 缩放倍数选择() {
        assert_eq!(scale_for_width(2560, 960), 3, "2560/960 向上取整为 3");
        assert_eq!(scale_for_width(1920, 960), 2);
        assert_eq!(scale_for_width(800, 960), 1, "本来就够小则不缩放");
        assert_eq!(scale_for_width(2560, 0), 1, "上限为 0 时退化为不缩放");
    }
}
