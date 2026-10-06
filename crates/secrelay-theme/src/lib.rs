//! SecRelay 主题色：跟随系统强调色。
//!
//! # 为什么单独一个 crate
//!
//! 它**不依赖任何 UI 框架**（只读系统配置），所以可以放在 `crates/` 下被桌面端、
//! 命令行端、将来的 Web 端共用；同时解析逻辑是纯函数，可以在**每个平台**上跑测试 ——
//! 即使本机是 Windows，也能验证 macOS / Linux 的解析分支。
//!
//! # 取值顺序
//!
//! | 平台 | 来源（按顺序尝试） |
//! |---|---|
//! | Windows | 注册表 `HKCU\Software\Microsoft\Windows\DWM` → `AccentColor`，退回 `Explorer\Accent` → `AccentColorMenu` |
//! | macOS | `defaults read -g AppleAccentColor`（索引） |
//! | Linux | GNOME `gsettings … accent-color` → KDE `kdeglobals` → GTK `gtk.css` 的 `theme_selected_bg_color` |
//! | 全部失败 | [`FALLBACK_ACCENT`] |
//!
//! # 一处刻意的调整
//!
//! 系统强调色有可能非常暗（例如接近黑色的自定义强调色）。直接拿来当深色界面上的
//! 强调文字/指示条会**看不见**。所以这里只在**亮度不足**时把它提亮到可读阈值，
//! 其余情况原样使用 —— 见 [`Rgb::readable_on_dark`]。

mod platform;

/// 取不到系统强调色时的回退色：**Windows 出厂默认强调色**（Fluent 蓝）。
///
/// 刻意不用 SecRelay 自己的品牌色：用户看到的第一眼应该是"和系统一致"，
/// 而不是"这个应用有它自己的想法"。品牌色只在明确的品牌位（图标、关于页）出现。
pub const FALLBACK_ACCENT: Rgb = Rgb::new(0x00, 0x78, 0xD4);

/// 深色界面上强调色所需的最低相对亮度。
///
/// 这个值刻意定得**很低**：目的只是拦住"几乎看不见"的颜色（例如接近黑的自定义强调色），
/// 而不是去评判系统色好不好看。定高了会误伤标准系统色 ——
/// 实测 Apple 蓝 `#0A84FF` 的亮度约 0.235、GNOME 蓝 `#3584E4` 约 0.226，
/// 阈值一旦到 0.25 就会把这两个官方颜色也一起改掉，那就不是"跟随系统"了。
pub const MIN_ACCENT_LUMINANCE: f32 = 0.12;

/// sRGB 颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// 形如 `#3DDC84` 的十六进制串。
    pub fn to_hex_string(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    /// 相对亮度（sRGB 线性化后加权，0.0 ~ 1.0），用于判断可读性。
    pub fn relative_luminance(self) -> f32 {
        fn linear(channel: u8) -> f32 {
            let c = f32::from(channel) / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    /// 在深色背景上是否足够醒目。
    pub fn is_readable_on_dark(self) -> bool {
        self.relative_luminance() >= MIN_ACCENT_LUMINANCE
    }

    /// 如果太暗就整体提亮到可读阈值；否则原样返回。
    ///
    /// 提亮是按比例拉向白色，保持色相不变。
    pub fn readable_on_dark(self) -> Rgb {
        if self.is_readable_on_dark() {
            return self;
        }
        // 逐步拉向白色，直到够亮。上限 20 步，避免极端输入下死循环。
        for step in 1..=20 {
            let t = step as f32 / 20.0;
            let lightened = self.mix(Rgb::new(255, 255, 255), t);
            if lightened.is_readable_on_dark() {
                return lightened;
            }
        }
        Rgb::new(255, 255, 255)
    }

    /// 按 `t`（0.0 = self，1.0 = other）线性混合。
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let blend = |a: u8, b: u8| -> u8 {
            let a = f32::from(a);
            let b = f32::from(b);
            (a + (b - a) * t).round().clamp(0.0, 255.0) as u8
        };
        Rgb::new(blend(self.r, other.r), blend(self.g, other.g), blend(self.b, other.b))
    }
}

impl std::fmt::Display for Rgb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex_string())
    }
}

/// 强调色的来源。UI 与日志用它说明"这个颜色是哪来的"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccentSource {
    /// 来自系统主题。附带来源名称，便于排查。
    System(&'static str),
    /// 系统没有可用的主题色，用了 [`FALLBACK_ACCENT`]。
    Fallback,
}

/// 最终采用的强调色。
#[derive(Debug, Clone, Copy)]
pub struct Accent {
    pub color: Rgb,
    pub source: AccentSource,
    /// 与之配套的"柔和底色"（用于消息气泡等大面积色块）。
    pub soft: Rgb,
}

/// 读取系统强调色；**永不失败** —— 取不到就用 [`FALLBACK_ACCENT`]。
///
/// `soft` 是与强调色配套的低饱和底色，按深色界面基底混合得到。
pub fn system_accent(dark_base: Rgb) -> Accent {
    let accent = match platform::detect() {
        Some((color, name)) => Accent {
            color: color.readable_on_dark(),
            source: AccentSource::System(name),
            soft: Rgb::new(0, 0, 0), // 下面统一计算
        },
        None => Accent {
            color: FALLBACK_ACCENT.readable_on_dark(),
            source: AccentSource::Fallback,
            soft: Rgb::new(0, 0, 0),
        },
    };
    Accent {
        soft: accent.color.mix(dark_base, 0.78),
        ..accent
    }
}

// ─────────────────────────────────────────────────────── 纯解析函数
//
// 刻意不做 cfg 门控：这样在任何一个平台上都能测到所有平台的分支。
// 平台模块只负责"怎么拿到那段文本"，解析逻辑全在这里。

/// 从 `reg query` 的输出里取 `REG_DWORD` 值，按 Windows 的 `0xAABBGGRR` 解释。
///
/// Windows 的强调色 DWORD 字节序是 **ABGR**：低字节是 R。
pub fn parse_windows_accent_output(output: &str) -> Option<Rgb> {
    let value = output
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            let hex = trimmed.strip_prefix("0x").or_else(|| {
                // 有的输出形如 "AccentColor    REG_DWORD    0x00ff8040"
                trimmed.split_whitespace().find(|t| t.starts_with("0x"))
                    .map(|t| &t[2..])
            })?;
            u32::from_str_radix(hex, 16).ok()
        })?;

    Some(Rgb::new(
        (value & 0xFF) as u8,
        ((value >> 8) & 0xFF) as u8,
        ((value >> 16) & 0xFF) as u8,
    ))
}

/// macOS `AppleAccentColor` 的索引 → 颜色。
///
/// 索引定义见系统偏好设置的外观面板：0 红 / 1 橙 / 2 黄 / 3 绿 / 4 蓝 /
/// 5 紫 / 6 粉 / 7 石墨。缺失或 `-1` 表示"多彩"，系统默认是蓝色。
pub fn accent_from_macos_index(index: i32) -> Rgb {
    match index {
        0 => Rgb::new(0xFF, 0x45, 0x3A), // 红
        1 => Rgb::new(0xFF, 0x9F, 0x0A), // 橙
        2 => Rgb::new(0xFF, 0xD6, 0x0A), // 黄
        3 => Rgb::new(0x32, 0xD7, 0x4B), // 绿
        5 => Rgb::new(0xBF, 0x5A, 0xF2), // 紫
        6 => Rgb::new(0xFF, 0x37, 0x5F), // 粉
        7 => Rgb::new(0x8E, 0x8E, 0x93), // 石墨
        // 4 = 蓝，以及"多彩"/未知都落到系统默认蓝
        _ => Rgb::new(0x0A, 0x84, 0xFF),
    }
}

/// 主题色名称 → 颜色。
///
/// 覆盖 GNOME 的 `accent-color` 取值（blue / teal / green / yellow / orange /
/// red / pink / purple / slate）。未知名称返回 `None`，让调用方继续尝试下一个来源。
pub fn accent_from_name(name: &str) -> Option<Rgb> {
    let normalized = name.trim().trim_matches('\'').trim_matches('"').to_ascii_lowercase();
    let color = match normalized.as_str() {
        "blue" => Rgb::new(0x35, 0x84, 0xE4),
        "teal" => Rgb::new(0x21, 0x90, 0xA4),
        "green" => Rgb::new(0x3A, 0x94, 0x4A),
        "yellow" => Rgb::new(0xC8, 0x88, 0x00),
        "orange" => Rgb::new(0xED, 0x5B, 0x00),
        "red" => Rgb::new(0xE6, 0x2D, 0x42),
        "pink" => Rgb::new(0xD5, 0x61, 0x99),
        "purple" => Rgb::new(0x91, 0x41, 0xAC),
        "slate" => Rgb::new(0x6F, 0x83, 0x96),
        _ => return None,
    };
    Some(color)
}

/// 从 GTK 样式表文本里找 `theme_selected_bg_color`。
pub fn parse_gtk_accent(css: &str) -> Option<Rgb> {
    for line in css.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('@') && trimmed.contains("theme_selected_bg_color") {
            if let Some(hex) = trimmed.split_whitespace().last() {
                if let Some(color) = parse_hex(hex) {
                    return Some(color);
                }
            }
        }
    }
    None
}

/// 解析 KDE 的强调色写法 `"R,G,B"` 或 `R,G,B`。
pub fn parse_kde_accent(value: &str) -> Option<Rgb> {
    let cleaned = value.trim().trim_matches(',').replace(['"', '\''], "");
    let parts: Vec<&str> = cleaned.split(',').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut channels = [0u8; 3];
    for (index, part) in parts.iter().enumerate() {
        channels[index] = part.trim().parse::<u8>().ok()?;
    }
    Some(Rgb::new(channels[0], channels[1], channels[2]))
}

/// 解析 `#RRGGBB`。
///
/// 容忍常见包装：引号、前导 `#`、行尾的 `;` 或 `,`（样式表里经常带）。
pub fn parse_hex(text: &str) -> Option<Rgb> {
    let hex = text
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_start_matches('#')
        .trim_end_matches(';')
        .trim_end_matches(',')
        .trim();
    if hex.len() != 6 {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some(Rgb::new(
        ((value >> 16) & 0xFF) as u8,
        ((value >> 8) & 0xFF) as u8,
        (value & 0xFF) as u8,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Windows

    #[test]
    fn 解析_windows_注册表输出() {
        // AccentColor 是 0xAABBGGRR：低字节是 R
        let sample = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\DWM\r\n    AccentColor    REG_DWORD    0x00ff8040\r\n";
        let color = parse_windows_accent_output(sample).expect("应当解析出颜色");
        assert_eq!(color, Rgb::new(0x40, 0x80, 0xFF), "低字节是 R，所以是 4080FF");
    }

    #[test]
    fn 解析_windows_输出失败时返回_none() {
        assert!(parse_windows_accent_output("").is_none());
        assert!(parse_windows_accent_output("没有十六进制值").is_none());
    }

    // ── macOS

    #[test]
    fn macos_索引映射() {
        assert_eq!(accent_from_macos_index(0), Rgb::new(0xFF, 0x45, 0x3A));
        assert_eq!(accent_from_macos_index(3), Rgb::new(0x32, 0xD7, 0x4B));
        // 4 = 蓝；-1 = 多彩，也落到系统默认蓝
        assert_eq!(accent_from_macos_index(4), accent_from_macos_index(-1));
        // 越界不应 panic
        assert_eq!(accent_from_macos_index(99), accent_from_macos_index(4));
    }

    // ── 名称映射

    #[test]
    fn 名称映射带引号也能解析() {
        assert_eq!(accent_from_name("'blue'"), accent_from_name("blue"));
        assert_eq!(accent_from_name("\"Blue\""), accent_from_name("blue"));
        assert_eq!(accent_from_name("  teal  "), Some(Rgb::new(0x21, 0x90, 0xA4)));
        assert!(accent_from_name("未知颜色").is_none());
    }

    // ── GTK / KDE / 十六进制

    #[test]
    fn 解析_kde_写法() {
        assert_eq!(parse_kde_accent("61,174,233"), Some(Rgb::new(61, 174, 233)));
        assert_eq!(parse_kde_accent("\"61,174,233\""), Some(Rgb::new(61, 174, 233)));
        assert!(parse_kde_accent("1,2").is_none());
        assert!(parse_kde_accent("a,b,c").is_none());
    }

    #[test]
    fn 解析十六进制() {
        assert_eq!(parse_hex("#0078D4"), Some(FALLBACK_ACCENT));
        assert_eq!(parse_hex("0078d4"), Some(FALLBACK_ACCENT));
        assert_eq!(parse_hex("#FFF"), None);
        assert_eq!(parse_hex("xyzxyz"), None);
    }

    #[test]
    fn 解析_gtk_样式表() {
        let css = "/* 注释 */\n@define-color theme_selected_bg_color #3584E4;\n@define-color theme_bg_color #fafafa;";
        assert_eq!(parse_gtk_accent(css), Some(Rgb::new(0x35, 0x84, 0xE4)));
        assert!(parse_gtk_accent("@define-color theme_bg_color #ffffff;").is_none());
    }

    // ── 亮度与可读性

    #[test]
    fn 回退色是_windows_出厂默认强调色() {
        // 这条是在钉住一个产品决定：取不到系统色时用"Windows 出厂默认"，
        // 而不是 SecRelay 自己的品牌色。改动它应当是有意识的。
        assert_eq!(FALLBACK_ACCENT.to_hex_string(), "#0078D4");
        assert!(FALLBACK_ACCENT.is_readable_on_dark());
    }

    #[test]
    fn 默认色在深色背景上可读() {
        assert!(
            FALLBACK_ACCENT.is_readable_on_dark(),
            "回退色的亮度是 {}",
            FALLBACK_ACCENT.relative_luminance()
        );
    }

    #[test]
    fn 很暗的强调色会被提亮() {
        let almost_black = Rgb::new(0x10, 0x10, 0x10);
        assert!(!almost_black.is_readable_on_dark());
        let fixed = almost_black.readable_on_dark();
        assert!(fixed.is_readable_on_dark(), "提亮后仍不可读：{fixed}");
        assert!(fixed.relative_luminance() > almost_black.relative_luminance());
    }

    #[test]
    fn 已经够亮的颜色不被改动() {
        // 这几个都是**官方系统色**，必须原样使用 —— 阈值定高了会把它们改掉。
        for color in [
            FALLBACK_ACCENT,
            Rgb::new(0x0A, 0x84, 0xFF), // Apple 蓝，亮度约 0.235
            Rgb::new(0x35, 0x84, 0xE4), // GNOME 蓝，亮度约 0.226
            Rgb::new(0xFF, 0xFF, 0xFF),
        ] {
            assert_eq!(
                color.readable_on_dark(),
                color,
                "{color}（亮度 {:.3}）不该被改动",
                color.relative_luminance()
            );
        }
    }

    #[test]
    fn 解析十六进制容忍样式表写法() {
        assert_eq!(parse_hex("#3584E4;"), Some(Rgb::new(0x35, 0x84, 0xE4)));
        assert_eq!(parse_hex("'#3584E4'"), Some(Rgb::new(0x35, 0x84, 0xE4)));
        assert_eq!(parse_hex("#3584E4,"), Some(Rgb::new(0x35, 0x84, 0xE4)));
    }

    #[test]
    fn 提亮保持色相方向() {
        // 暗红提亮后应当仍然是"红占优"，而不是变成灰
        let dark_red = Rgb::new(0x40, 0x00, 0x00);
        let lifted = dark_red.readable_on_dark();
        assert!(lifted.r > lifted.g && lifted.r > lifted.b, "实际：{lifted}");
    }

    #[test]
    fn 混合() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        assert_eq!(black.mix(white, 0.0), black);
        assert_eq!(black.mix(white, 1.0), white);
        assert_eq!(black.mix(white, 0.5), Rgb::new(128, 128, 128));
        // 越界参数被夹住，不 panic
        assert_eq!(black.mix(white, -1.0), black);
        assert_eq!(black.mix(white, 2.0), white);
    }

    #[test]
    fn 十六进制字符串格式() {
        assert_eq!(FALLBACK_ACCENT.to_hex_string(), "#0078D4");
        assert_eq!(Rgb::new(0, 0, 0).to_hex_string(), "#000000");
    }

    // ── 端到端

    #[test]
    fn 总能拿到一个可读的强调色() {
        // 无论本机是什么系统、有没有主题色，都必须返回可读的颜色。
        let accent = system_accent(Rgb::new(0x15, 0x16, 0x1A));
        assert!(accent.color.is_readable_on_dark(), "{:?}", accent);
        // soft 应当比原色更接近背景（更暗）
        assert!(
            accent.soft.relative_luminance() < accent.color.relative_luminance(),
            "soft 应当更暗：{accent:?}"
        );
    }
}
