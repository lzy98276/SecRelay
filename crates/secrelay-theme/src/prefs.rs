//! 本地偏好设置。
//!
//! # 为什么用 `key=value` 而不是 TOML/JSON
//!
//! 要存的东西目前只有"主题模式"一项，为它引入序列化框架不划算。
//! 这个格式**任何人都能手工改**（排查问题时很有用），解析器只有几十行，
//! 而且格式错误时能安全回退到默认值而不是崩在启动阶段。
//!
//! # 原则
//!
//! - **读配置永远不失败**：文件不存在、损坏、字段未知，都退回默认值。
//! - **写配置失败只记录**：用户选择该生效于当前会话，不能因为写盘失败就丢。

use std::path::PathBuf;

use crate::ThemeMode;

/// 配置文件名。
const FILE_NAME: &str = "config.txt";

/// 本地偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preferences {
    /// 主题模式，默认跟随系统。
    pub theme_mode: ThemeMode,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme_mode: ThemeMode::System,
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
    /// 容忍：注释（`#` 开头）、空行、未知键、缺 `=` 的行、无法识别的取值。
    /// 这些一律忽略，不影响其它字段。
    pub fn parse(text: &str) -> Self {
        let mut prefs = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() == "theme" {
                if let Some(mode) = ThemeMode::from_key(value) {
                    prefs.theme_mode = mode;
                }
            }
        }
        prefs
    }

    /// 序列化为配置文本。
    pub fn serialize(&self) -> String {
        format!(
            "# SecRelay 本地配置\n\
             # 手工编辑后重启生效。取值：system / light / dark\n\
             theme={}\n",
            self.theme_mode.key()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 默认跟随系统() {
        assert_eq!(Preferences::default().theme_mode, ThemeMode::System);
    }

    #[test]
    fn 往返一致() {
        for mode in ThemeMode::ALL {
            let prefs = Preferences { theme_mode: mode };
            let text = prefs.serialize();
            assert_eq!(Preferences::parse(&text), prefs, "模式 {mode:?} 往返不一致");
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
        let prefs = Preferences::parse(text);
        assert_eq!(prefs.theme_mode, ThemeMode::Dark);
    }

    #[test]
    fn 键名两侧空白被容忍() {
        assert_eq!(
            Preferences::parse("  theme  =  light  ").theme_mode,
            ThemeMode::Light
        );
    }

    #[test]
    fn 配置路径在用户目录下() {
        let path = Preferences::config_path();
        let text = path.to_string_lossy().to_lowercase();
        assert!(text.contains("secrelay"), "实际：{text}");
        assert!(text.ends_with("config.txt"), "实际：{text}");
    }
}
