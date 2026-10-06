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
use crate::palette::AccentMode;
use crate::ThemeMode;

/// 配置文件名。
const FILE_NAME: &str = "config.txt";

/// 内置的默认中继基址。
///
/// 与 `secrelay-relay-client` 的同名常量保持一致：这里存的是"文本"，不引入那个依赖。
pub const DEFAULT_RELAY_BASE: &str = "https://secrelay-relay.sectl.cn";

/// 本地偏好。
#[derive(Debug, Clone, PartialEq)]
pub struct Preferences {
    /// 主题模式，默认跟随系统。
    pub theme_mode: ThemeMode,
    /// 界面字体族名，默认内置 miSans。
    pub font_family: String,
    /// 界面字重，默认 400。
    pub font_weight: u16,
    /// 强调色模式，默认跟随系统。
    pub accent_mode: AccentMode,
    /// 自定义强调色的色相（0-360）。
    pub hue: f32,
    /// 自定义强调色的饱和度（0-100）。
    pub saturation: f32,
    /// 中继列表与当前选中项。
    pub relays: Relays,
}

/// 中继列表。
///
/// 只做文本层面的校验（非空、`http`/`https` 前缀），完整规范化由中继客户端负责 ——
/// 这个 crate 不认识网络层，也就不该把 URL 解析规则复制一份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relays {
    urls: Vec<String>,
    selected: usize,
}

impl Default for Relays {
    fn default() -> Self {
        Self {
            urls: vec![DEFAULT_RELAY_BASE.to_string()],
            selected: 0,
        }
    }
}

impl Relays {
    /// 列表里的地址。
    pub fn urls(&self) -> &[String] {
        &self.urls
    }

    /// 当前选中的下标，恒小于 `urls().len()`。
    pub fn selected(&self) -> usize {
        self.selected.min(self.urls.len().saturating_sub(1))
    }

    /// 当前选中的地址。
    pub fn selected_url(&self) -> &str {
        &self.urls[self.selected()]
    }

    /// 从一组地址构造。非法地址被丢掉；空了就退回默认中继。
    pub fn from_urls(urls: impl IntoIterator<Item = String>) -> Self {
        let kept: Vec<String> = urls
            .into_iter()
            .map(|url| url.trim().to_string())
            .filter(|url| is_acceptable_relay_url(url))
            .collect();

        if kept.is_empty() {
            return Self::default();
        }
        Self {
            urls: dedup(kept),
            selected: 0,
        }
    }

    /// 加一个地址。重复的也返回成功，只把选中项挪过去。
    pub fn add(&mut self, url: &str) -> Result<usize, String> {
        let url = url.trim();
        if !is_acceptable_relay_url(url) {
            return Err("中继地址必须以 http:// 或 https:// 开头".to_string());
        }
        match self.urls.iter().position(|existing| existing == url) {
            Some(index) => {
                self.selected = index;
                Ok(index)
            }
            None => {
                self.urls.push(url.to_string());
                self.selected = self.urls.len() - 1;
                Ok(self.selected)
            }
        }
    }

    /// 删掉一个地址。删掉最后一条时回到默认中继，不留空列表。
    ///
    /// 返回是否真的删掉了。
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.urls.len() {
            return false;
        }
        self.urls.remove(index);
        if self.urls.is_empty() {
            *self = Self::default();
            return true;
        }
        // 删除位置之前的选中项要跟着往前挪，之后的不用动
        if index < self.selected {
            self.selected -= 1;
        }
        self.selected = self.selected.min(self.urls.len() - 1);
        true
    }

    /// 选中某个地址。
    pub fn select(&mut self, index: usize) {
        if index < self.urls.len() {
            self.selected = index;
        }
    }

    /// 序列化成一行，地址之间用 `;` 分隔；`;` 转义成 `%3B`。
    fn serialize(&self) -> String {
        self.urls
            .iter()
            .map(|url| url.replace(';', "%3B"))
            .collect::<Vec<_>>()
            .join(";")
    }

    /// 解析一行。空行与全是非法地址时退回默认中继。
    fn parse(value: &str) -> Self {
        let urls = value
            .split(';')
            .map(|part| part.trim().replace("%3B", ";"))
            .collect::<Vec<_>>();
        Self::from_urls(urls)
    }
}

/// 地址是否是本程序能接受的形态：非空且以 `http://` 或 `https://` 开头（大小写不敏感）。
fn is_acceptable_relay_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// 去重，保留首次出现的顺序。
fn dedup(urls: Vec<String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for url in urls {
        if !seen.contains(&url) {
            seen.push(url);
        }
    }
    seen
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme_mode: ThemeMode::System,
            font_family: BUILTIN_FAMILY.to_string(),
            font_weight: DEFAULT_WEIGHT,
            accent_mode: AccentMode::System,
            hue: 28.0,
            saturation: 78.0,
            relays: Relays::default(),
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
        let mut selected: Option<usize> = None;

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
                "accent" => {
                    if let Some(mode) = AccentMode::from_key(value) {
                        prefs.accent_mode = mode;
                    }
                }
                "hue" => {
                    if let Ok(hue) = value.parse::<f32>() {
                        if (0.0..=360.0).contains(&hue) {
                            prefs.hue = hue;
                        }
                    }
                }
                "saturation" => {
                    if let Ok(saturation) = value.parse::<f32>() {
                        if (0.0..=100.0).contains(&saturation) {
                            prefs.saturation = saturation;
                        }
                    }
                }
                // relays 与 relay_url 是同一个位置的两种写法：前者是列表，后者是旧版的单条。
                "relays" | "relay_url" => {
                    prefs.relays = Relays::parse(value);
                }
                "relay_selected" => {
                    if let Ok(index) = value.parse::<usize>() {
                        selected = Some(index);
                    }
                }
                _ => {}
            }
        }

        // 选中项等所有行读完再应用：列表可能出现在它后面
        if let Some(index) = selected {
            prefs.relays.select(index);
        }
        prefs
    }

    /// 序列化为配置文本。
    pub fn serialize(&self) -> String {
        format!(
            "# SecRelay 本地配置\n\
             # 手工编辑后重启生效。\n\
             # theme:          system / light / dark\n\
             # font_family:    系统里任意已安装字体的名字，misans 内置字体叫 MiSans\n\
             # font_weight:    100 - 900，实际会用该字体最接近的可用字重\n\
             # accent:         system / custom\n\
             # hue:            0 - 360（仅 accent=custom 时生效）\n\
             # saturation:     0 - 100（仅 accent=custom 时生效）\n\
             # relays:         中继基址，多条用 ; 分隔，只认 http:// 与 https://\n\
             # relay_selected: 当前选中的中继下标，从 0 开始\n\
             theme={}\n\
             font_family={}\n\
             font_weight={}\n\
             accent={}\n\
             hue={}\n\
             saturation={}\n\
             relays={}\n\
             relay_selected={}\n",
            self.theme_mode.key(),
            self.font_family,
            self.font_weight,
            self.accent_mode.key(),
            self.hue,
            self.saturation,
            self.relays.serialize(),
            self.relays.selected()
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
        // 默认中继开箱可用
        assert_eq!(prefs.relays.urls(), [DEFAULT_RELAY_BASE]);
        assert_eq!(prefs.relays.selected(), 0);
        assert_eq!(prefs.relays.selected_url(), DEFAULT_RELAY_BASE);
    }

    #[test]
    fn 往返一致() {
        for mode in ThemeMode::ALL {
            for weight in [100_u16, 400, 600, 900] {
                for accent_mode in AccentMode::ALL {
                    let prefs = Preferences {
                        theme_mode: mode,
                        font_family: "微软雅黑".to_string(),
                        font_weight: weight,
                        accent_mode,
                        hue: 123.5,
                        saturation: 42.0,
                        relays: Relays::from_urls([
                            "https://relay.example.com".to_string(),
                            "http://127.0.0.1:8080".to_string(),
                        ]),
                    };
                    let text = prefs.serialize();
                    assert_eq!(Preferences::parse(&text), prefs, "往返不一致：{text}");
                }
            }
        }
    }

    #[test]
    fn 中继列表往返一致且选中项保住() {
        let mut relays = Relays::from_urls([
            "https://relay.example.com".to_string(),
            "http://127.0.0.1:8080".to_string(),
            "https://relay.secrelay.dev".to_string(),
        ]);
        relays.select(1);

        let prefs = Preferences {
            relays: relays.clone(),
            ..Preferences::default()
        };
        let text = prefs.serialize();
        let parsed = Preferences::parse(&text);
        assert_eq!(parsed.relays.urls(), relays.urls());
        assert_eq!(parsed.relays.selected(), 1);
        assert_eq!(parsed.relays.selected_url(), "http://127.0.0.1:8080");
        assert_eq!(parsed, prefs);
    }

    #[test]
    fn 强调色字段的边界() {
        assert_eq!(Preferences::parse("hue=400").hue, 28.0, "越界色相应被忽略");
        assert_eq!(Preferences::parse("hue=-1").hue, 28.0);
        assert_eq!(Preferences::parse("saturation=200").saturation, 78.0);
        assert_eq!(Preferences::parse("hue=200.5").hue, 200.5);
        assert_eq!(Preferences::parse("accent=custom").accent_mode, AccentMode::Custom);
        assert_eq!(Preferences::parse("accent=乱写").accent_mode, AccentMode::System);
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

    // ─────────────────────────────── 中继

    #[test]
    fn 中继列表里非_http_的地址被丢掉() {
        let prefs = Preferences::parse(
            "relays=https://a.example;ftp://b.example;ws://c.example;http://d.example;不是地址",
        );
        assert_eq!(
            prefs.relays.urls(),
            ["https://a.example", "http://d.example"],
            "只留 http/https"
        );
    }

    #[test]
    fn 中继列表为空时退回默认中继() {
        for text in [
            "relays=",
            "relays=;;;",
            "relays=ftp://a.example",
            "relay_url=",
        ] {
            assert_eq!(
                Preferences::parse(text).relays,
                Relays::default(),
                "{text} 应当退回默认中继"
            );
        }
    }

    #[test]
    fn 中继列表去重且顺序保留() {
        let prefs = Preferences::parse(
            "relays=https://a.example;https://b.example;https://a.example",
        );
        assert_eq!(prefs.relays.urls(), ["https://a.example", "https://b.example"]);
    }

    #[test]
    fn 旧的单条中继字段仍然能读() {
        let prefs = Preferences::parse("relay_url=http://127.0.0.1:8080\n");
        assert_eq!(prefs.relays.urls(), ["http://127.0.0.1:8080"]);
    }

    #[test]
    fn 选中项越界时回到第一条() {
        let prefs = Preferences::parse("relays=https://a.example\nrelay_selected=9");
        assert_eq!(prefs.relays.selected(), 0);
        assert_eq!(prefs.relays.selected_url(), "https://a.example");
    }

    #[test]
    fn 选中项写在列表前面也能生效() {
        let prefs = Preferences::parse(
            "relay_selected=1\nrelays=https://a.example;https://b.example",
        );
        assert_eq!(prefs.relays.selected_url(), "https://b.example");
    }

    #[test]
    fn 增删中继与选中项联动() {
        let mut relays = Relays::default();
        let index = relays.add("https://b.example").unwrap();
        assert_eq!(index, 1);
        assert_eq!(relays.selected(), 1, "新加的会被选中");

        // 重复添加只移动选中项，不新增
        let same = relays.add("https://b.example").unwrap();
        assert_eq!(same, 1);
        assert_eq!(relays.urls().len(), 2);

        assert!(relays.add("ftp://c.example").is_err());
        assert!(relays.add("c.example").is_err());
        assert_eq!(relays.urls().len(), 2, "非法地址不该进列表");

        // 删掉选中项之前的一条，选中项要跟着往前挪
        relays.select(1);
        assert!(relays.remove(0));
        assert_eq!(relays.urls(), ["https://b.example"]);
        assert_eq!(relays.selected(), 0);

        assert!(!relays.remove(5), "越界删除返回 false");

        // 删到空就退回默认中继
        assert!(relays.remove(0));
        assert_eq!(relays, Relays::default());
    }

    #[test]
    fn 删掉别的条目不会改变选中项() {
        let mut relays = Relays::from_urls([
            "https://a.example".to_string(),
            "https://b.example".to_string(),
            "https://c.example".to_string(),
        ]);
        relays.select(2);
        assert!(relays.remove(0));
        assert_eq!(relays.selected_url(), "https://c.example");
    }

    #[test]
    fn 中继地址里的分号不会破坏格式() {
        let mut relays = Relays::default();
        relays.add("https://a.example/x;y").unwrap();
        let prefs = Preferences {
            relays: relays.clone(),
            ..Preferences::default()
        };
        let text = prefs.serialize();
        assert_eq!(Preferences::parse(&text).relays.urls(), relays.urls());
    }

    #[test]
    fn 选中接口拒绝越界下标() {
        let mut relays = Relays::default();
        relays.select(99);
        assert_eq!(relays.selected(), 0);
    }
}
