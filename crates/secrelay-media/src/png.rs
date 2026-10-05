//! 极简 PNG 编码器。
//!
//! 为什么自己写而不是引依赖：我们只需要"把一帧 BGRA 存成 PNG"这一件事，
//! 而 PNG 的必要子集非常小 —— 签名、IHDR、IDAT、IEND，加上 **stored 模式的 deflate**
//! （不压缩，直接分块存放）。用 `flate2`/`png` 之类会引入 zlib 的 C 依赖或成规模的纯 Rust 实现，
//! 对"截图自证 / 文档配图 / 缺陷复现"这个用途不划算。
//!
//! 代价是**文件比压缩后大**（1080p 约 8MB）。所以提供了整数倍降采样：
//! `scale=2` 时面积变成 1/4，用于文档配图足够。
//!
//! 这不是通用图像库，也不打算变成通用图像库。

use crate::frame::VideoFrame;

/// PNG 文件签名。
const SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// stored deflate 单块的最大长度。
const MAX_STORED_BLOCK: usize = 65_535;

#[derive(Debug, thiserror::Error)]
pub enum PngError {
    #[error("缩放倍数必须大于 0")]
    BadScale,

    #[error("帧尺寸为 0，无法编码")]
    EmptyFrame,

    #[error("帧缓冲区过大，无法编码为单个 PNG：{0} 字节")]
    TooLarge(usize),
}

/// 把一帧编码成 PNG（8 位真彩色，无 alpha）。
///
/// `scale` 是整数降采样倍数：1 表示原尺寸，2 表示宽高各取一半（面积 1/4）。
pub fn encode(frame: &VideoFrame, scale: u32) -> Result<Vec<u8>, PngError> {
    if scale == 0 {
        return Err(PngError::BadScale);
    }
    if frame.width == 0 || frame.height == 0 {
        return Err(PngError::EmptyFrame);
    }

    let (out_width, out_height) = (
        frame.width.div_ceil(scale),
        frame.height.div_ceil(scale),
    );

    // 每行 1 字节滤波器类型 + RGB 像素
    let row_len = 1 + out_width as usize * 3;
    let raw_len = row_len * out_height as usize;
    if raw_len > u32::MAX as usize {
        return Err(PngError::TooLarge(raw_len));
    }

    let mut raw = Vec::with_capacity(raw_len);
    for out_y in 0..out_height {
        raw.push(0); // 滤波器类型：None
        for out_x in 0..out_width {
            let (r, g, b) = sample_rgb(frame, out_x * scale, out_y * scale, scale);
            raw.push(r);
            raw.push(g);
            raw.push(b);
        }
    }

    let mut png = Vec::with_capacity(raw_len + raw_len / 512 + 128);
    png.extend_from_slice(&SIGNATURE);

    // IHDR
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&out_width.to_be_bytes());
    ihdr.extend_from_slice(&out_height.to_be_bytes());
    ihdr.push(8); // 位深
    ihdr.push(2); // 颜色类型 2 = 真彩色 RGB
    ihdr.push(0); // 压缩方法
    ihdr.push(0); // 滤波方法
    ihdr.push(0); // 非隔行
    write_chunk(&mut png, b"IHDR", &ihdr);

    // IDAT：zlib 头 + stored deflate + adler32
    let mut zlib = Vec::with_capacity(raw_len + raw_len / 1024 + 64);
    zlib.push(0x78); // CMF: deflate, 32K 窗口
    zlib.push(0x01); // FLG: 使 (CMF<<8|FLG) % 31 == 0，无字典
    write_stored_deflate(&mut zlib, &raw);
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
    write_chunk(&mut png, b"IDAT", &zlib);

    write_chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

/// 取一个输出像素对应的 RGB。`scale=1` 时就是直接取值；
/// 大于 1 时对 `scale x scale` 块做平均（盒式滤波），避免最近邻的锯齿。
fn sample_rgb(frame: &VideoFrame, base_x: u32, base_y: u32, scale: u32) -> (u8, u8, u8) {
    if scale == 1 {
        let (b, g, r) = pixel_bgr(frame, base_x, base_y);
        return (r, g, b);
    }

    let mut sum = [0u32; 3];
    let mut count = 0u32;
    for dy in 0..scale {
        for dx in 0..scale {
            let x = base_x + dx;
            let y = base_y + dy;
            if x >= frame.width || y >= frame.height {
                continue;
            }
            let (b, g, r) = pixel_bgr(frame, x, y);
            sum[0] += u32::from(r);
            sum[1] += u32::from(g);
            sum[2] += u32::from(b);
            count += 1;
        }
    }
    if count == 0 {
        return (0, 0, 0);
    }
    (
        (sum[0] / count) as u8,
        (sum[1] / count) as u8,
        (sum[2] / count) as u8,
    )
}

/// 读取 BGRA 像素，返回 `(b, g, r)`。越界返回 (0,0,0)。
fn pixel_bgr(frame: &VideoFrame, x: u32, y: u32) -> (u8, u8, u8) {
    let stride = frame.stride();
    let offset = y as usize * stride + x as usize * 4;
    match frame.data.get(offset..offset + 4) {
        Some(px) => (px[0], px[1], px[2]),
        None => (0, 0, 0),
    }
}

/// 写入一个 PNG chunk：长度 + 类型 + 数据 + CRC32（CRC 覆盖类型与数据）。
fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);

    let mut crc = Crc32::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// 以 **stored（不压缩）** 模式写出 deflate 数据流。
fn write_stored_deflate(out: &mut Vec<u8>, data: &[u8]) {
    if data.is_empty() {
        // 空输入也要写一个空的 final block，否则不是合法 deflate 流。
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
        return;
    }

    let mut offset = 0;
    while offset < data.len() {
        let take = (data.len() - offset).min(MAX_STORED_BLOCK);
        let is_last = offset + take == data.len();

        out.push(if is_last { 0x01 } else { 0x00 }); // BFINAL + BTYPE=00
        let len = take as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes()); // NLEN 是 LEN 的反码
        out.extend_from_slice(&data[offset..offset + take]);

        offset += take;
    }
}

/// zlib 的 Adler-32 校验和。
fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for byte in data {
        a = (a + u32::from(*byte)) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

/// PNG 使用的 CRC-32（反射多项式 0xEDB88320）。
struct Crc32 {
    value: u32,
}

impl Crc32 {
    fn new() -> Self {
        Self { value: 0xFFFF_FFFF }
    }

    fn update(&mut self, data: &[u8]) {
        for byte in data {
            self.value ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = (self.value & 1).wrapping_neg();
                self.value = (self.value >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
    }

    fn finish(self) -> u32 {
        self.value ^ 0xFFFF_FFFF
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::PixelFormat;
    use std::time::Instant;

    fn frame_from(width: u32, height: u32, pick: impl Fn(u32, u32) -> (u8, u8, u8)) -> VideoFrame {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let (r, g, b) = pick(x, y);
                data.extend_from_slice(&[b, g, r, 0xFF]); // BGRA
            }
        }
        VideoFrame::new(width, height, PixelFormat::Bgra8, data, Instant::now(), 0).unwrap()
    }

    /// 按 PNG 规则重新算一遍文件里每个 chunk 的 CRC，验证文件结构自洽。
    fn verify_structure(png: &[u8]) -> Vec<(String, usize)> {
        assert_eq!(&png[..8], &SIGNATURE, "签名不对");
        let mut chunks = Vec::new();
        let mut offset = 8;
        while offset < png.len() {
            let len = u32::from_be_bytes(png[offset..offset + 4].try_into().unwrap()) as usize;
            let kind = std::str::from_utf8(&png[offset + 4..offset + 8])
                .unwrap()
                .to_string();
            let data = &png[offset + 8..offset + 8 + len];
            let stored_crc =
                u32::from_be_bytes(png[offset + 8 + len..offset + 12 + len].try_into().unwrap());

            let mut crc = Crc32::new();
            crc.update(&png[offset + 4..offset + 8]);
            crc.update(data);
            assert_eq!(crc.finish(), stored_crc, "chunk {kind} 的 CRC 不正确");

            chunks.push((kind, len));
            offset += 12 + len;
        }
        chunks
    }

    #[test]
    fn 编码出结构合法的_png() {
        let frame = frame_from(4, 3, |x, y| ((x * 60) as u8, (y * 80) as u8, 128));
        let png = encode(&frame, 1).unwrap();
        let chunks = verify_structure(&png);

        let kinds: Vec<&str> = chunks.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, vec!["IHDR", "IDAT", "IEND"]);
        assert_eq!(chunks[0].1, 13, "IHDR 长度必须是 13");
        assert_eq!(chunks[2].1, 0, "IEND 必须为空");
    }

    #[test]
    fn ihdr_里的宽高与帧一致() {
        let frame = frame_from(7, 5, |_, _| (1, 2, 3));
        let png = encode(&frame, 1).unwrap();
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        assert_eq!((width, height), (7, 5));
        assert_eq!(png[24], 8, "位深应为 8");
        assert_eq!(png[25], 2, "颜色类型应为真彩色");
    }

    #[test]
    fn 降采样后尺寸正确() {
        let frame = frame_from(8, 6, |_, _| (10, 20, 30));
        let png = encode(&frame, 2).unwrap();
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        assert_eq!((width, height), (4, 3));
        verify_structure(&png);
    }

    #[test]
    fn 奇数尺寸降采样向上取整() {
        let frame = frame_from(7, 5, |_, _| (0, 0, 0));
        let png = encode(&frame, 2).unwrap();
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        assert_eq!((width, height), (4, 3), "7/2 向上取整为 4，5/2 为 3");
    }

    #[test]
    fn 颜色按_rgb_顺序写入而不是_bgra_顺序() {
        // 一个纯红像素：BGRA 是 [0,0,255,255]，PNG 里应当是 FF 00 00
        let frame = frame_from(1, 1, |_, _| (255, 0, 0));
        let png = encode(&frame, 1).unwrap();

        // 找到 IDAT 数据区：zlib(2) + block header(5) 之后就是第一个像素
        let idat_start = png
            .windows(4)
            .position(|w| w == b"IDAT")
            .expect("应当有 IDAT");
        let data_start = idat_start + 4 + 2 + 5 + 1; // +1 是行首滤波器字节
        assert_eq!(
            &png[data_start..data_start + 3],
            &[0xFF, 0x00, 0x00],
            "纯红像素应当是 FF 00 00"
        );
    }

    #[test]
    fn 空帧与非法缩放被拒() {
        let frame = frame_from(2, 2, |_, _| (0, 0, 0));
        assert!(matches!(encode(&frame, 0), Err(PngError::BadScale)));
    }

    #[test]
    fn adler32_符合已知值() {
        // zlib 规范里的示例值
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"a"), 0x0062_0062);
        assert_eq!(adler32(b"abc"), 0x024D_0127);
    }

    #[test]
    fn crc32_符合已知值() {
        let mut crc = Crc32::new();
        crc.update(b"123456789");
        assert_eq!(crc.finish(), 0xCBF4_3926, "CRC-32 标准测试向量");
    }

    #[test]
    fn 大数据会分成多个_stored_块() {
        let len = MAX_STORED_BLOCK * 2 + 100;
        let data = vec![7u8; len];
        let mut out = Vec::new();
        write_stored_deflate(&mut out, &data);
        // 每块 5 字节头 + 数据
        assert_eq!(out.len(), len + 5 * 3);
        assert_eq!(out[0] & 0x01, 0, "非最后一块 BFINAL 应为 0");
    }
}
