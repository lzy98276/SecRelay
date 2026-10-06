//! 主题模式与调色板。
//!
//! # 为什么整套调色板放在 Rust 而不是 `.slint`
//!
//! 因为"跟随系统"要求同一套颜色在运行时切换，而**强调色本来就要按系统主题算**
//! （见 [`crate::Rgb::readable_on_dark`]）。把颜色集中到一处，才能保证
//! 浅色/深色/强调色三者之间的可读性约束被真正检查到 —— 放在 `.slint` 里就只能靠眼睛。
//!
//! `ui/app.slint` 里的 `Theme` 全局只声明 `in-out` 属性，值全部由这里注入。

use crate::Rgb;

/// 用户可选的三种主题模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    /// 跟随系统（默认）。
    #[default]
    System,
    /// 始终浅色。
    Light,
    /// 始终深色。
    Dark,
}

impl ThemeMode {
    /// 全部模式，顺序即 UI 里的展示顺序。
    pub const ALL: [ThemeMode; 3] = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark];

    /// 从配置文件里的值解析。无法识别时返回 `None`（由调用方决定回退）。
    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "system" => Some(ThemeMode::System),
            "light" => Some(ThemeMode::Light),
            "dark" => Some(ThemeMode::Dark),
            _ => None,
        }
    }

    /// 写进配置文件的值。
    pub fn key(self) -> &'static str {
        match self {
            ThemeMode::System => "system",
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        }
    }

    /// 解析成实际使用的配色方案。
    pub fn resolve(self, system: ColorScheme) -> ColorScheme {
        match self {
            ThemeMode::System => system,
            ThemeMode::Light => ColorScheme::Light,
            ThemeMode::Dark => ColorScheme::Dark,
        }
    }

    /// 给 UI 用的序号（`SegmentedOptions` 的选中值）。
    pub fn index(self) -> i32 {
        match self {
            ThemeMode::System => 0,
            ThemeMode::Light => 1,
            ThemeMode::Dark => 2,
        }
    }

    /// 从 UI 序号还原。越界时回退到默认值。
    pub fn from_index(index: i32) -> Self {
        ThemeMode::ALL
            .get(index.max(0) as usize)
            .copied()
            .unwrap_or_default()
    }
}

/// 实际配色方案（"跟随系统"解析之后的结果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    Light,
    Dark,
}

impl ColorScheme {
    pub fn is_dark(self) -> bool {
        matches!(self, ColorScheme::Dark)
    }
}

/// 一套完整的界面颜色。
///
/// 字段与 `ui/app.slint` 里 `Theme` 全局的属性一一对应 —— 两边改动必须同步，
/// 所以这里用 `#[derive]` 之外的显式结构，而不是一个 `HashMap<String, Rgb>`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub bg: Rgb,
    pub nav: Rgb,
    pub surface: Rgb,
    pub surface_hi: Rgb,
    pub stage: Rgb,
    pub border: Rgb,
    pub text: Rgb,
    pub text_dim: Rgb,
    pub text_faint: Rgb,
    pub idle: Rgb,
    /// 强调色（已按当前配色方案做过可读性调整）。
    pub accent: Rgb,
    /// 强调色的柔和底色，用于消息气泡等大面积色块。
    pub accent_soft: Rgb,
    /// 叠在强调色上的文字颜色（用于填充式按钮）。
    pub on_accent: Rgb,
}

impl Palette {
    /// 按配色方案与系统强调色构造调色板。
    pub fn build(scheme: ColorScheme, accent: Rgb) -> Self {
        match scheme {
            ColorScheme::Dark => Self::dark(accent),
            ColorScheme::Light => Self::light(accent),
        }
    }

    fn dark(accent: Rgb) -> Self {
        let bg = Rgb::new(0x15, 0x16, 0x1A);
        let accent = accent.readable_on(bg);
        Self {
            bg,
            nav: Rgb::new(0x10, 0x11, 0x16),
            surface: Rgb::new(0x1B, 0x1C, 0x21),
            surface_hi: Rgb::new(0x26, 0x2A, 0x31),
            stage: Rgb::new(0x0B, 0x0C, 0x10),
            border: Rgb::new(0x24, 0x26, 0x2D),
            text: Rgb::new(0xE6, 0xE9, 0xEF),
            text_dim: Rgb::new(0x96, 0x9B, 0xA5),
            text_faint: Rgb::new(0x5F, 0x64, 0x6E),
            idle: Rgb::new(0x4A, 0x4E, 0x59),
            accent,
            // 深色下的柔和底色：向深色基底混合
            accent_soft: accent.mix(bg, 0.78),
            on_accent: Rgb::new(0x0B, 0x0C, 0x10),
        }
    }

    fn light(accent: Rgb) -> Self {
        let bg = Rgb::new(0xF3, 0xF4, 0xF6);
        let accent = accent.readable_on(bg);
        Self {
            bg,
            nav: Rgb::new(0xE9, 0xEA, 0xEE),
            surface: Rgb::new(0xFF, 0xFF, 0xFF),
            surface_hi: Rgb::new(0xE6, 0xE8, 0xEC),
            stage: Rgb::new(0xD9, 0xDB, 0xE0),
            border: Rgb::new(0xD5, 0xD8, 0xDE),
            text: Rgb::new(0x1B, 0x1D, 0x22),
            text_dim: Rgb::new(0x5A, 0x5F, 0x6A),
            text_faint: Rgb::new(0x8B, 0x90, 0x9A),
            idle: Rgb::new(0xB6, 0xBA, 0xC2),
            accent,
            // 浅色下的柔和底色：向白色混合
            accent_soft: accent.mix(Rgb::new(0xFF, 0xFF, 0xFF), 0.82),
            on_accent: Rgb::new(0xFF, 0xFF, 0xFF),
        }
    }

    /// 检查这套配色是否满足基本的可读性要求。
    ///
    /// 返回不满足的项，供测试与"诊断"页使用；空表示全部通过。
    pub fn contrast_problems(&self) -> Vec<&'static str> {
        let mut problems = Vec::new();
        // 正文字对比背景：粗阈值，只用来拦住"根本看不清"的配色
        if self.text.relative_luminance() - self.bg.relative_luminance() > 0.0
            && (self.text.relative_luminance() - self.bg.relative_luminance()).abs() < 0.25
        {
            problems.push("text 与 bg 的亮度太接近");
        }
        if self.text_dim.relative_luminance() - self.bg.relative_luminance() > 0.0
            && (self.text_dim.relative_luminance() - self.bg.relative_luminance()).abs() < 0.15
        {
            problems.push("text_dim 与 bg 的亮度太接近");
        }
        // 强调色必须能从背景上区分出来
        let accent_gap = (self.accent.relative_luminance() - self.bg.relative_luminance()).abs();
        if accent_gap < 0.08 {
            problems.push("accent 与 bg 的亮度太接近");
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCENTS: [Rgb; 5] = [
        crate::FALLBACK_ACCENT,
        Rgb::new(0x0A, 0x84, 0xFF), // Apple 蓝
        Rgb::new(0x3A, 0x94, 0x4A), // GNOME 绿
        Rgb::new(0xFF, 0xD6, 0x0A), // macOS 黄（很亮）
        Rgb::new(0x10, 0x10, 0x10), // 近黑（很暗）
    ];

    #[test]
    fn 主题模式解析与序号() {
        assert_eq!(ThemeMode::from_key("system"), Some(ThemeMode::System));
        assert_eq!(ThemeMode::from_key("DARK"), Some(ThemeMode::Dark));
        assert_eq!(ThemeMode::from_key("  light "), Some(ThemeMode::Light));
        assert_eq!(ThemeMode::from_key("蓝色"), None);

        for mode in ThemeMode::ALL {
            assert_eq!(ThemeMode::from_index(mode.index()), mode);
        }
        // 越界回退到默认
        assert_eq!(ThemeMode::from_index(99), ThemeMode::System);
        assert_eq!(ThemeMode::from_index(-1), ThemeMode::System);
    }

    #[test]
    fn 跟随系统会采用系统方案() {
        assert_eq!(ThemeMode::System.resolve(ColorScheme::Light), ColorScheme::Light);
        assert_eq!(ThemeMode::System.resolve(ColorScheme::Dark), ColorScheme::Dark);
        assert_eq!(ThemeMode::Light.resolve(ColorScheme::Dark), ColorScheme::Light);
        assert_eq!(ThemeMode::Dark.resolve(ColorScheme::Light), ColorScheme::Dark);
    }

    #[test]
    fn 两种配色下所有强调色都可读() {
        for scheme in [ColorScheme::Light, ColorScheme::Dark] {
            for accent in ACCENTS {
                let palette = Palette::build(scheme, accent);
                assert!(
                    palette.contrast_problems().is_empty(),
                    "{scheme:?} + {accent} 的配色有问题：{:?}",
                    palette.contrast_problems()
                );
            }
        }
    }

    #[test]
    fn 浅色与深色的基底确实相反() {
        let dark = Palette::build(ColorScheme::Dark, crate::FALLBACK_ACCENT);
        let light = Palette::build(ColorScheme::Light, crate::FALLBACK_ACCENT);
        assert!(
            dark.bg.relative_luminance() < light.bg.relative_luminance(),
            "深色背景应当比浅色背景暗"
        );
        assert!(
            dark.text.relative_luminance() > dark.bg.relative_luminance(),
            "深色下文字应当比背景亮"
        );
        assert!(
            light.text.relative_luminance() < light.bg.relative_luminance(),
            "浅色下文字应当比背景暗"
        );
    }

    #[test]
    fn 很亮的强调色在浅色界面上会被压暗() {
        let bright = Rgb::new(0xFF, 0xFF, 0x00);
        let palette = Palette::build(ColorScheme::Light, bright);
        assert_ne!(palette.accent, bright, "纯黄在白色背景上必须被压暗");
        assert!(
            palette.accent.relative_luminance() < bright.relative_luminance()
        );
    }

    #[test]
    fn 柔和底色朝各自基底混合() {
        let accent = Rgb::new(0x00, 0x78, 0xD4);
        let dark = Palette::build(ColorScheme::Dark, accent);
        let light = Palette::build(ColorScheme::Light, accent);
        assert!(
            dark.accent_soft.relative_luminance() < light.accent_soft.relative_luminance(),
            "深色的柔和底色应当更暗"
        );
    }
}
