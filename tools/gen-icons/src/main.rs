//! 从 FluentSystemIcons 字体生成 Slint 用的图标码位表。
//!
//! # 为什么要有这个工具
//!
//! Fluent System Icons 的 `GSUB` 表只有几十字节，**没有连字特性** ——
//! 也就是说不能靠"写字形名"来显示图标，只能按 **Private Use Area 码位**访问。
//! 而码位是一串没有任何可读性的数字，手抄进源码既容易错、又没法在字体升级后维护。
//!
//! 所以这里从字体本身把「字形名 → 码位」抽出来，生成 `icons.slint`。
//! 换字体版本时重新跑一次即可，不需要人工对照任何表格。
//!
//! ```bash
//! cargo run -p gen-icons
//! ```
//!
//! # 为什么不用现成的字体库
//!
//! `ttf-parser` 读这张字体的 `post` 表时一律返回 `None`（字形名取不到），
//! 而这恰恰是我们要的唯一信息。`post` 2.0 与 `cmap` 4/12 的格式都很简单，
//! 与其绕开库的限制，不如直接解析 —— 顺便把这个工具变成零依赖。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

/// 需要的图标：`(Slint 侧属性名, 字体里的字形名)`。
///
/// 字形名是 Fluent 的官方命名规则：`ic_fluent_<名称>_<尺寸>_<风格>`。
///
/// ⚠️ **风格用 filled（实心）**：产品上希望图标更醒目、在深色界面上更实。
/// 字体同时提供 `regular`（描边）与 `filled`，切换只改这一列。
///
/// ⚠️ **尺寸是 20 不是 24**：`FluentSystemIcons-Resizable` 是可变字重字体，
/// 它只保留一套基准尺寸（20）用于缩放，没有 24/28/32 这些独立尺寸。
/// 命名写错时生成器会直接列出候选，不用靠猜。
const ICONS: &[(&str, &str)] = &[
    // 导航
    ("devices", "ic_fluent_desktop_20_filled"),
    ("screen", "ic_fluent_share_screen_start_20_filled"),
    ("camera", "ic_fluent_camera_20_filled"),
    ("files", "ic_fluent_folder_20_filled"),
    ("messages", "ic_fluent_chat_20_filled"),
    ("settings", "ic_fluent_settings_20_filled"),
    // 动作
    ("connect", "ic_fluent_plug_connected_20_filled"),
    ("send", "ic_fluent_send_20_filled"),
    ("play", "ic_fluent_play_20_filled"),
    ("stop", "ic_fluent_stop_20_filled"),
    ("folder_open", "ic_fluent_folder_open_20_filled"),
    // 状态与设置项
    ("language", "ic_fluent_local_language_20_filled"),
    ("info", "ic_fluent_info_20_filled"),
    ("warning", "ic_fluent_warning_20_filled"),
    ("checkmark", "ic_fluent_checkmark_circle_20_filled"),
    ("dismiss", "ic_fluent_dismiss_circle_20_filled"),
    // 账号
    ("account", "ic_fluent_person_20_filled"),
    ("sign_in", "ic_fluent_arrow_enter_20_filled"),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let font_path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("assets/fonts/FluentSystemIcons-Resizable.ttf"));
    let out_path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("apps/secrelay-desktop/ui/icons.slint"));

    let data = std::fs::read(&font_path)
        .map_err(|e| format!("读取字体失败 {}：{e}", font_path.display()))?;

    let font = Font::parse(&data)?;
    println!("字体：{}", font.family);

    let glyph_names = font.glyph_names();
    println!("字形名：{} 个", glyph_names.len());

    // 码位 → 字形名
    let mut by_name: BTreeMap<String, u32> = BTreeMap::new();
    font.for_each_codepoint(|codepoint, glyph| {
        if let Some(Some(name)) = glyph_names.get(glyph as usize) {
            by_name
                .entry(name.clone())
                .and_modify(|existing| {
                    if codepoint < *existing {
                        *existing = codepoint;
                    }
                })
                .or_insert(codepoint);
        }
    });

    let mut resolved: Vec<(&str, &str, u32)> = Vec::new();
    let mut missing: Vec<(&str, &str)> = Vec::new();
    for (key, glyph_name) in ICONS {
        match by_name.get(*glyph_name) {
            Some(codepoint) => resolved.push((key, glyph_name, *codepoint)),
            None => missing.push((key, glyph_name)),
        }
    }

    if !missing.is_empty() {
        eprintln!("\n以下字形在字体里找不到（名称可能随字体版本变化）：");
        for (key, name) in &missing {
            eprintln!("  {key} -> {name}");
        }
        eprintln!("\n按关键词查找可用字形名：");
        for keyword in [
            "desktop", "screen_share", "camera", "folder_open", "folder_", "chat", "settings",
            "plug_connected", "send", "play", "stop", "local_language", "info", "warning",
            "checkmark_circle", "dismiss_circle",
        ] {
            let mut matches: Vec<&String> = by_name
                .keys()
                .filter(|n| n.contains(keyword) && n.ends_with("_regular"))
                .collect();
            matches.sort();
            if matches.is_empty() {
                eprintln!("  {keyword}: （无）");
            } else {
                eprintln!(
                    "  {keyword}: {}",
                    matches
                        .iter()
                        .take(4)
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        return Err(format!("有 {} 个图标名没解析出来", missing.len()).into());
    }

    let mut out = String::new();
    writeln!(
        out,
        "// 由 `cargo run -p gen-icons` 从字体自动生成，**不要手工编辑**。\n\
         //\n\
         // 来源字体：{}（family：{}）\n\
         //\n\
         // FluentSystemIcons 没有连字特性，图标只能按 Private Use Area 码位访问，\n\
         // 所以这里把码位固化成常量。用法见 ui/app.slint 里的 `Icon` 组件。",
        font_path.display(),
        font.family
    )?;
    writeln!(out)?;
    writeln!(out, "export global Icons {{")?;
    for (key, glyph_name, codepoint) in &resolved {
        writeln!(
            out,
            "    // {glyph_name}\n    out property <string> {key}: \"\\u{{{codepoint:X}}}\";"
        )?;
    }
    writeln!(out, "}}")?;

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&out_path, out)?;
    println!("已写入 {}（{} 个图标）", out_path.display(), resolved.len());
    Ok(())
}

// ───────────────────────────────────────────────────── 最小 TTF 解析

/// 只解析我们需要的三张表：`name`（字体名）、`cmap`（码位→字形）、`post`（字形名）。
struct Font<'a> {
    data: &'a [u8],
    family: String,
    cmap_offset: usize,
    post_offset: usize,
    post_version: u32,
    num_glyphs: u16,
}

impl<'a> Font<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, String> {
        let num_tables = read_u16(data, 4).ok_or("字体文件太小")?;
        let mut cmap_offset = None;
        let mut post_offset = None;
        let mut name_offset = None;

        for index in 0..num_tables as usize {
            let record = 12 + index * 16;
            let tag = read_tag(data, record).ok_or("表目录越界")?;
            let offset = read_u32(data, record + 8).ok_or("表目录越界")? as usize;
            match &tag {
                b"cmap" => cmap_offset = Some(offset),
                b"post" => post_offset = Some(offset),
                b"name" => name_offset = Some(offset),
                _ => {}
            }
        }

        let cmap_offset = cmap_offset.ok_or("字体里没有 cmap 表")?;
        let post_offset = post_offset.ok_or("字体里没有 post 表")?;
        let post_version = read_u32(data, post_offset).ok_or("post 表越界")?;
        if post_version != 0x0002_0000 {
            return Err(format!(
                "post 表版本是 0x{post_version:08X}，只有 2.0 才带字形名"
            ));
        }
        let num_glyphs =
            read_u16(data, post_offset + 32).ok_or("post 表缺少 numberOfGlyphs")?;

        let family = name_offset
            .and_then(|offset| read_family_name(data, offset))
            .unwrap_or_else(|| "<未知>".to_string());

        Ok(Self {
            data,
            family,
            cmap_offset,
            post_offset,
            post_version,
            num_glyphs,
        })
    }

    /// 字形 ID → 字形名。索引与字形 ID 对齐，`None` 表示该字形没有名字。
    fn glyph_names(&self) -> Vec<Option<String>> {
        let table = self.post_offset;
        let count = self.num_glyphs as usize;

        // 索引数组紧跟在 32 字节表头之后
        let mut indexes = Vec::with_capacity(count);
        for glyph in 0..count {
            match read_u16(self.data, table + 34 + glyph * 2) {
                Some(index) => indexes.push(index),
                None => break,
            }
        }

        // 索引 >= 258 的部分才是自定义名，按出现顺序排列的 Pascal 串
        let strings_start = table + 34 + count * 2;
        let mut custom: Vec<String> = Vec::new();
        let mut cursor = strings_start;
        while let Some(length) = read_u8(self.data, cursor) {
            let start = cursor + 1;
            let end = start + length as usize;
            if end > self.data.len() {
                break;
            }
            custom.push(String::from_utf8_lossy(&self.data[start..end]).into_owned());
            cursor = end;
        }

        indexes
            .iter()
            .map(|index| {
                let index = *index as usize;
                if index < 258 {
                    // Macintosh 标准名（.notdef 之类），图标用不到
                    None
                } else {
                    custom.get(index - 258).cloned()
                }
            })
            .collect()
    }

    /// 遍历所有统一码码位及其字形 ID。
    ///
    /// 支持 cmap format 4（BMP）与 format 12（分段覆盖），覆盖这张字体实际使用的两种。
    fn for_each_codepoint(&self, mut visit: impl FnMut(u32, u16)) {
        let base = self.cmap_offset;
        let Some(num_subtables) = read_u16(self.data, base + 2) else {
            return;
        };

        for index in 0..num_subtables as usize {
            let record = base + 4 + index * 8;
            let Some(platform) = read_u16(self.data, record) else {
                continue;
            };
            let Some(encoding) = read_u16(self.data, record + 2) else {
                continue;
            };
            let Some(subtable_offset) = read_u32(self.data, record + 4) else {
                continue;
            };
            // 只要 Unicode 子表：平台 0（Unicode），或平台 3（Windows）的 UCS-2/UCS-4
            let is_unicode = platform == 0 || (platform == 3 && matches!(encoding, 1 | 10));
            if !is_unicode {
                continue;
            }

            let subtable = base + subtable_offset as usize;
            match read_u16(self.data, subtable) {
                Some(4) => self.walk_format4(subtable, &mut visit),
                Some(12) => self.walk_format12(subtable, &mut visit),
                _ => {}
            }
        }
    }

    fn walk_format4(&self, table: usize, visit: &mut impl FnMut(u32, u16)) {
        let Some(seg_count_x2) = read_u16(self.data, table + 6) else {
            return;
        };
        let seg_count = seg_count_x2 as usize / 2;
        let end_codes = table + 14;
        let start_codes = end_codes + seg_count * 2 + 2; // 跳过 reservedPad
        let deltas = start_codes + seg_count * 2;
        let range_offsets = deltas + seg_count * 2;

        for segment in 0..seg_count {
            let Some(start) = read_u16(self.data, start_codes + segment * 2) else {
                return;
            };
            let Some(end) = read_u16(self.data, end_codes + segment * 2) else {
                return;
            };
            let Some(delta) = read_i16(self.data, deltas + segment * 2) else {
                return;
            };
            let Some(range_offset) = read_u16(self.data, range_offsets + segment * 2) else {
                return;
            };
            if start == 0xFFFF {
                continue; // 哨兵段
            }

            for codepoint in start..=end {
                // 0xFFFF 是哨兵，跳过
                if codepoint == 0xFFFF {
                    continue;
                }
                let glyph = if range_offset == 0 {
                    (codepoint as i32 + delta as i32) as u16
                } else {
                    // idRangeOffset 是从它自己所在位置算起的字节偏移
                    let word = range_offsets + segment * 2;
                    let glyph_index_at = word + range_offset as usize
                        + (codepoint - start) as usize * 2;
                    match read_u16(self.data, glyph_index_at) {
                        Some(0) | None => continue,
                        Some(raw) => (raw as i32 + delta as i32) as u16,
                    }
                };
                if glyph != 0 {
                    visit(codepoint as u32, glyph);
                }
            }
        }
    }

    fn walk_format12(&self, table: usize, visit: &mut impl FnMut(u32, u16)) {
        let Some(group_count) = read_u32(self.data, table + 12) else {
            return;
        };
        let groups = table + 16;
        for group in 0..group_count as usize {
            let base = groups + group * 12;
            let (Some(start), Some(end), Some(first_glyph)) = (
                read_u32(self.data, base),
                read_u32(self.data, base + 4),
                read_u32(self.data, base + 8),
            ) else {
                return;
            };
            if end < start {
                continue;
            }
            for codepoint in start..=end {
                let glyph = first_glyph + (codepoint - start);
                if glyph != 0 && glyph <= u16::MAX as u32 {
                    visit(codepoint, glyph as u16);
                }
            }
        }
    }

    #[allow(dead_code)]
    fn post_version(&self) -> u32 {
        self.post_version
    }
}

/// 读 `name` 表里 Windows 平台的 family 名（nameID 1）。
fn read_family_name(data: &[u8], table: usize) -> Option<String> {
    let count = read_u16(data, table + 2)?;
    let string_offset = table + read_u16(data, table + 4)? as usize;

    for index in 0..count as usize {
        let record = table + 6 + index * 12;
        let platform = read_u16(data, record)?;
        let name_id = read_u16(data, record + 6)?;
        let length = read_u16(data, record + 8)? as usize;
        let offset = read_u16(data, record + 10)? as usize;
        if name_id != 1 || platform != 3 {
            continue;
        }
        let start = string_offset + offset;
        let end = start + length;
        if end > data.len() {
            continue;
        }
        // Windows 平台的名字是 UTF-16BE
        let units: Vec<u16> = data[start..end]
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    None
}

// ───────────────────────────────────────────────────── 小工具

fn read_u8(data: &[u8], offset: usize) -> Option<u8> {
    data.get(offset).copied()
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_i16(data: &[u8], offset: usize) -> Option<i16> {
    read_u16(data, offset).map(|value| value as i16)
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_tag(data: &[u8], offset: usize) -> Option<[u8; 4]> {
    let bytes = data.get(offset..offset + 4)?;
    Some([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 越界读取返回_none_而不是_panic() {
        let data = [0u8; 4];
        assert!(read_u8(&data, 99).is_none());
        assert!(read_u16(&data, 3).is_none());
        assert!(read_u32(&data, 1).is_none());
        assert!(read_tag(&data, 2).is_none());
    }

    #[test]
    fn 读取大端整数() {
        let data = [0x12, 0x34, 0x56, 0x78];
        assert_eq!(read_u16(&data, 0), Some(0x1234));
        assert_eq!(read_u32(&data, 0), Some(0x12345678));
        assert_eq!(read_i16(&data, 0), Some(0x1234));
    }

    #[test]
    fn 非字体数据被拒绝() {
        let data = [0u8; 64];
        assert!(Font::parse(&data).is_err());
    }

    #[test]
    fn 真实字体能被解析并含有目标图标() {
        // 这条测试把"生成器能用"钉在 CI 里：字体换版本后如果名字变了，
        // 这里会失败，而不是等到界面显示成一堆方框。
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/fonts/FluentSystemIcons-Resizable.ttf");
        let Ok(data) = std::fs::read(&path) else {
            eprintln!("跳过：找不到字体 {}", path.display());
            return;
        };
        let font = Font::parse(&data).expect("应当能解析真实字体");
        assert_eq!(font.family, "FluentSystemIcons-Resizable");

        let names = font.glyph_names();
        assert!(names.len() > 1000, "字形名数量异常：{}", names.len());

        let mut by_name: BTreeMap<String, u32> = BTreeMap::new();
        font.for_each_codepoint(|codepoint, glyph| {
            if let Some(Some(name)) = names.get(glyph as usize) {
                by_name.entry(name.clone()).or_insert(codepoint);
            }
        });

        for (key, glyph_name) in ICONS {
            assert!(
                by_name.contains_key(*glyph_name),
                "字体里缺少图标 {key} -> {glyph_name}"
            );
        }
    }
}
