//! 本地偏好设置。
//!
//! # 为什么用 `key=value` 而不是 TOML/JSON
//!
//! 要存的东西只有几项外观设置，为它引入序列化框架不划算。
//! 这个格式**任何人都能手工改**（排查问题时很有用），解析器只有几十行，
//! 而且格式错误时能安全回退到默认值而不是崩在启动阶段。
//!
//! # 原则
//!
//! - **读配置永远不失败**：文件不存在、损坏、字段未知，都退回默认值。
//! - **写配置失败只记录**：用户选择该生效于当前会话，不能因为写盘失败就丢。

use std::path::PathBuf;

use crate::fonts::{BUILTIN_FAMILY, DEFAULT_WEIGHT};
use crate::ThemeMode;

/// 配置文件名。
const FILE_NAME: &str = "config.txt";

/// 本地偏好。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preferences {
    /// 主题模式，默认跟随系统。
    pub theme_mode: ThemeMode,
    /// 界面字体族名，默认内置 miSans。
    pub font_family: String,
    /// 界面字重，默认 400。
    pub font_weight: u16,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme_mode: ThemeMode::System,
            font_family: BUILTIN_FAMILY.to_string(),
            font_weight: DEFAULT_WEIGHT,
        }
    }
}

impl Preferences {
    /// 配置目录：`%LOCALAPPDATA%\SecRelay` 或 XDG 下的对应位置。
    pub fn config_dir() -> PathBuf {
        let base = if cfg!(target_os = "windows") {
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        };
        base.unwrap_or_else(|| PathBuf::from(".")).join("SecRelay")
    }

    /// 配置文件完整路径。
    pub fn config_path() -> PathBuf {
        Self::config_dir().join(FILE_NAME)
    }

    /// 读取偏好。**永不失败** —— 任何问题都退回默认值。
    pub fn load() -> Self {
        match std::fs::read_to_string(Self::config_path()) {
            Ok(text) => Self::parse(&text),
            Err(_) => Self::default(),
        }
    }

    /// 写入偏好。
    pub fn save(&self) -> std::io::Result<()> {
        let dir = Self::config_dir();
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(FILE_NAME), self.serialize())
    }

    /// 解析配置文本。
    ///
    /// 容忍：**UTF-8 BOM**、注释（`#` 开头）、空行、未知键、缺 `=` 的行、无法识别的取值。
    /// 这些一律忽略，不影响其它字段。
    ///
    /// 剥 BOM 是必须的：Windows 记事本默认就写 BOM，而这个文件号称"可手工编辑"。
    /// `char::is_whitespace` 不把 U+FEFF 当空白，所以 `trim()` 挡不住它 ——
    /// 第一行的键会变成 `\u{FEFF}theme`，静默失效。
    pub fn parse(text: &str) -> Self {
        let mut prefs = Self::default();
        for line in text.trim_start_matches('\u{FEFF}').lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "theme" => {
                    if let Some(mode) = ThemeMode::from_key(value) {
                        prefs.theme_mode = mode;
                    }
                }
                // 字体名不校验存在性：用户可能在别的机器上装了某个字体，
                // 换机器后名字还在配置里。渲染时会自动回退（见 FontCatalog::resolve）。
                "font_family" if !value.is_empty() => prefs.font_family = value.to_string(),
                "font_weight" => {
                    if let Ok(weight) = value.parse::<u16>() {
                        if (100..=900).contains(&weight) {
                            prefs.font_weight = weight;
                        }
                    }
                }
                _ => {}
            }
        }
        prefs
    }

    /// 序列化为配置文本。
    pub fn serialize(&self) -> String {
        format!(
            "# SecRelay 本地配置\n\
             # 手工编辑后重启生效。\n\
             # theme:       system / light / dark\n\
             # font_family: 系统里任意已安装字体的名字，misans 内置字体叫 MiSans\n\
             # font_weight: 100 - 900，实际会用该字体最接近的可用字重\n\
             theme={}\n\
             font_family={}\n\
             font_weight={}\n",
            self.theme_mode.key(),
            self.font_family,
            self.font_weight
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 默认是跟随系统加内置字体() {
        let prefs = Preferences::default();
        assert_eq!(prefs.theme_mode, ThemeMode::System);
        assert_eq!(prefs.font_family, "MiSans");
        assert_eq!(prefs.font_weight, 400);
    }

    #[test]
    fn 往返一致() {
        for mode in ThemeMode::ALL {
            for weight in [100_u16, 400, 600, 900] {
                let prefs = Preferences {
                    theme_mode: mode,
                    font_family: "微软雅黑".to_string(),
                    font_weight: weight,
                };
                let text = prefs.serialize();
                assert_eq!(Preferences::parse(&text), prefs, "往返不一致：{text}");
            }
        }
    }

    #[test]
    fn 空文本与垃圾文本都退回默认() {
        assert_eq!(Preferences::parse(""), Preferences::default());
        assert_eq!(Preferences::parse("这不是配置"), Preferences::default());
        assert_eq!(Preferences::parse("theme=未知"), Preferences::default());
        assert_eq!(Preferences::parse("theme"), Preferences::default());
    }

    #[test]
    fn 忽略注释空行与未知键() {
        let text = "# 注释\n\n  \ntheme=dark\n未来字段=1\n另一个 = 值\n";
        assert_eq!(Preferences::parse(text).theme_mode, ThemeMode::Dark);
    }

    #[test]
    fn 键名两侧空白被容忍() {
        assert_eq!(
            Preferences::parse("  theme  =  light  ").theme_mode,
            ThemeMode::Light
        );
    }

    #[test]
    fn 容忍_utf8_bom() {
        // Windows 记事本默认写 BOM。不剥掉的话第一行的键会变成 "\u{FEFF}theme"，
        // 配置静默失效 —— 这条测试就是为了防止回归。
        assert_eq!(
            Preferences::parse("\u{FEFF}theme=light\n").theme_mode,
            ThemeMode::Light
        );
        assert_eq!(
            Preferences::parse("\u{FEFF}# 注释\ntheme=dark\n").theme_mode,
            ThemeMode::Dark
        );
        // 第二行的字体也不能因为 BOM 被吃掉
        let prefs = Preferences::parse("\u{FEFF}theme=dark\nfont_weight=600\n");
        assert_eq!(prefs.font_weight, 600);
    }

    #[test]
    fn 非法字重被忽略() {
        assert_eq!(Preferences::parse("font_weight=abc").font_weight, 400);
        assert_eq!(Preferences::parse("font_weight=50").font_weight, 400);
        assert_eq!(Preferences::parse("font_weight=1000").font_weight, 400);
        assert_eq!(Preferences::parse("font_weight=").font_weight, 400);
    }

    #[test]
    fn 字体名保留原样不校验() {
        // 换机器后字体可能不存在，但配置值要保住，方便用户换回来
        let prefs = Preferences::parse("font_family=某台机器上才有的字体");
        assert_eq!(prefs.font_family, "某台机器上才有的字体");
        // 空值不覆盖默认
        assert_eq!(Preferences::parse("font_family=").font_family, "MiSans");
    }

    #[test]
    fn 配置路径在用户目录下() {
        let path = Preferences::config_path();
        let text = path.to_string_lossy().to_lowercase();
        assert!(text.contains("secrelay"), "实际：{text}");
        assert!(text.ends_with("config.txt"), "实际：{text}");
    }
}
