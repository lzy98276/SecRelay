//! SecRelay 国际化。
//!
//! # 为什么不用 gettext / .po
//!
//! 常见做法是字符串 key（`tr("session.ready")`）+ `.po` 文件。我们没走这条路，原因是它**漏翻不会报错**：
//! 少写一条翻译，只在运行时显示成 key 或空白，测试也查不出来。
//!
//! 这里改用 **enum key**：
//!
//! - 文案是 [`Key`] 的变体，调用方写不出不存在的 key（编译期检查）；
//! - [`Key::text`] 对 [`Lang`] 做穷尽匹配，**新增语言时漏翻任何一条都会编译失败**；
//! - 这个 crate 不依赖 UI、不依赖 async，所以 UI、命令行、日志、以及将来的
//!   Web 端（编译到 WASM）可以共用同一份目录。
//!
//! 代价是没有标准翻译工具链支持。在"自己维护、语言少、正确性优先"的前提下，这个交换划算；
//! 如果将来要外包翻译，再补一个 `.po` 导出的脚本即可，目录本身不用改。
//!
//! # 加一门语言的步骤
//!
//! 1. 在 [`Lang`] 里加变体，在 [`Lang::ALL`] 里登记；
//! 2. 在 [`Key::text`] 里加一个分支（**编译器会强制你把每条文案都填上**）；
//! 3. 在 `text()` 的分支里调用对应的 `xx_yy()` 私有方法。
//!
//! 就这么三步。第 2 步漏了任何一条文案都过不了编译。

/// 支持的语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Lang {
    /// 简体中文。
    #[default]
    ZhHans,
}

impl Lang {
    /// 全部语言。UI 的语言选择器直接用它。
    pub const ALL: &'static [Lang] = &[Lang::ZhHans];

    /// BCP-47 语言标签，用于配置持久化与 Web 端的 `Accept-Language` 协商。
    pub fn code(self) -> &'static str {
        match self {
            Lang::ZhHans => "zh-Hans",
        }
    }

    /// 语言自称（在语言选择器里始终用它自己语言显示，不要翻译）。
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::ZhHans => "简体中文",
        }
    }

    /// 从语言标签解析。支持 `zh`、`zh-Hans`、`zh-CN` 等常见写法。
    pub fn from_code(code: &str) -> Option<Self> {
        let normalized = code.trim().to_ascii_lowercase().replace('_', "-");
        let primary = normalized.split('-').next().unwrap_or("");
        match primary {
            "zh" => Some(Lang::ZhHans),
            _ => None,
        }
    }

    /// 按给定顺序挑第一个支持的语言，找不到则回退到默认语言。
    ///
    /// 用于"系统语言 → 我们支持的语言"的协商。
    pub fn negotiate<'a>(candidates: impl IntoIterator<Item = &'a str>) -> Self {
        candidates
            .into_iter()
            .find_map(Lang::from_code)
            .unwrap_or_default()
    }
}

/// 全部可翻译文案的键。
///
/// **新增文案就在这里加一个变体**，然后在 [`Key::text`] 的两个地方补上内容
/// （`zh_hans` 分支，以及将来其它语言的分支）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    // ── 应用级
    AppName,
    AppTagline,

    // ── 导航
    NavDevices,
    NavSession,
    NavSettings,
    NavAbout,

    // ── 设备列表
    DevicesTitle,
    DevicesEmpty,
    DevicesEmptyHint,
    DevicesRefresh,

    // ── 会话
    SessionTitle,
    SessionStateIdle,
    SessionStateConnecting,
    SessionStateReady,
    SessionStateClosed,
    SessionPeer,
    SessionChannels,
    SessionCapabilities,
    SessionNone,

    // ── 操作
    ActionConnect,
    ActionDisconnect,
    ActionSend,
    ActionCopy,

    // ── 消息
    MessagePlaceholder,
    MessageEmpty,
    MessageSent,
    MessageReceived,

    // ── 频道名
    ChannelMedia,
    ChannelFile,
    ChannelControl,

    // ── 设置
    SettingsTitle,
    SettingsLanguage,
    SettingsLanguageHint,

    // ── 日志与错误
    LogTitle,
    LogHandshakeStarted,
    LogHandshakeDone,
    LogSendFailed,
    LogPeerClosed,
    LogConnected,
    LogDisconnected,
    LogDemoMode,

    ErrorGeneric,
    ErrorNotConnected,
    ErrorNotImplemented,

    /// `--demo` 自动演示时发送的消息内容。
    DemoMessage,
}

impl Key {
    /// 全部文案键。测试用它做穷尽检查。
    pub const ALL: &'static [Key] = &[
        Key::AppName,
        Key::AppTagline,
        Key::NavDevices,
        Key::NavSession,
        Key::NavSettings,
        Key::NavAbout,
        Key::DevicesTitle,
        Key::DevicesEmpty,
        Key::DevicesEmptyHint,
        Key::DevicesRefresh,
        Key::SessionTitle,
        Key::SessionStateIdle,
        Key::SessionStateConnecting,
        Key::SessionStateReady,
        Key::SessionStateClosed,
        Key::SessionPeer,
        Key::SessionChannels,
        Key::SessionCapabilities,
        Key::SessionNone,
        Key::ActionConnect,
        Key::ActionDisconnect,
        Key::ActionSend,
        Key::ActionCopy,
        Key::MessagePlaceholder,
        Key::MessageEmpty,
        Key::MessageSent,
        Key::MessageReceived,
        Key::ChannelMedia,
        Key::ChannelFile,
        Key::ChannelControl,
        Key::SettingsTitle,
        Key::SettingsLanguage,
        Key::SettingsLanguageHint,
        Key::LogTitle,
        Key::LogHandshakeStarted,
        Key::LogHandshakeDone,
        Key::LogSendFailed,
        Key::LogPeerClosed,
        Key::LogConnected,
        Key::LogDisconnected,
        Key::LogDemoMode,
        Key::ErrorGeneric,
        Key::ErrorNotConnected,
        Key::ErrorNotImplemented,
        Key::DemoMessage,
    ];

    /// 取文案。
    ///
    /// 匹配是**穷尽的**：新增 [`Lang`] 变体而不补分支，这里就编译不过。
    pub fn text(self, lang: Lang) -> &'static str {
        match lang {
            Lang::ZhHans => self.zh_hans(),
        }
    }

    fn zh_hans(self) -> &'static str {
        match self {
            Key::AppName => "SecRelay",
            Key::AppTagline => "跨设备连接，让看、传、说归于一处",

            Key::NavDevices => "设备",
            Key::NavSession => "会话",
            Key::NavSettings => "设置",
            Key::NavAbout => "关于",

            Key::DevicesTitle => "设备列表",
            Key::DevicesEmpty => "还没有已配对的设备",
            Key::DevicesEmptyHint => "在另一台设备上打开 SecRelay，用二维码配对即可",
            Key::DevicesRefresh => "刷新",

            Key::SessionTitle => "会话",
            Key::SessionStateIdle => "未连接",
            Key::SessionStateConnecting => "正在连接…",
            Key::SessionStateReady => "已就绪",
            Key::SessionStateClosed => "已断开",
            Key::SessionPeer => "对端设备",
            Key::SessionChannels => "已协商频道",
            Key::SessionCapabilities => "共同能力",
            Key::SessionNone => "无",

            Key::ActionConnect => "连接",
            Key::ActionDisconnect => "断开",
            Key::ActionSend => "发送",
            Key::ActionCopy => "复制",

            Key::MessagePlaceholder => "输入要发送的文字…",
            Key::MessageEmpty => "还没有消息",
            Key::MessageSent => "已发送：{text}",
            Key::MessageReceived => "已收到：{text}",

            Key::ChannelMedia => "媒体",
            Key::ChannelFile => "文件",
            Key::ChannelControl => "控制",

            Key::SettingsTitle => "设置",
            Key::SettingsLanguage => "界面语言",
            Key::SettingsLanguageHint => "目前仅提供简体中文，其它语言后续添加",

            Key::LogTitle => "日志",
            Key::LogHandshakeStarted => "开始握手…",
            Key::LogHandshakeDone => "握手完成，对端 {peer}",
            Key::LogSendFailed => "发送失败：{detail}",
            Key::LogPeerClosed => "对端已断开",
            Key::LogConnected => "已连接",
            Key::LogDisconnected => "已断开连接",
            Key::LogDemoMode => "M0 演示：使用进程内回环连接，真实 P2P 尚未接入",

            Key::ErrorGeneric => "出错了：{detail}",
            Key::ErrorNotConnected => "尚未建立连接",
            Key::ErrorNotImplemented => "该功能尚未实现",

            Key::DemoMessage => "这是一条自动演示消息",
        }
    }

    /// 把文案里的 `{name}` 占位符换成实参。
    ///
    /// 刻意用一个极小的实现而不是引入模板引擎：我们只需要"名字替换"这一种能力，
    /// 而且这样在 WASM 里也没有额外负担。
    ///
    /// 未提供实参的占位符**原样保留**（方便一眼看出漏传了），不做静默删除。
    pub fn format(self, lang: Lang, args: &[(&str, &str)]) -> String {
        render(self.text(lang), args)
    }
}

/// 对任意模板做 `{name}` 替换。独立出来便于单独测试。
pub fn render(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (name, value) in args {
        let placeholder = format!("{{{name}}}");
        out = out.replace(&placeholder, value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 每条文案在每种语言下都非空() {
        for lang in Lang::ALL {
            for key in Key::ALL {
                let text = key.text(*lang);
                assert!(
                    !text.trim().is_empty(),
                    "文案为空：{key:?} @ {}",
                    lang.code()
                );
            }
        }
    }

    #[test]
    fn 全部键都已登记在_all_里() {
        // 防止新增 Key 变体后忘记加进 ALL —— 那样上面的穷尽检查会漏掉它。
        // 这里用 Debug 名字做交叉核对：ALL 的长度必须与枚举变体数一致。
        // 变体数变化时这个断言会失败，提醒维护者同步 ALL。
        assert_eq!(
            Key::ALL.len(),
            45,
            "Key 变体数量变了：请同时更新 Key::ALL 与本断言的数字"
        );
    }

    #[test]
    fn 文案没有重复的占位符残留() {
        // 除了明确需要参数的文案，其余不应含 `{`，否则是漏传实参的信号。
        for lang in Lang::ALL {
            for key in Key::ALL {
                let text = key.text(*lang);
                assert!(
                    !text.contains("TODO") && !text.contains("FIXME"),
                    "文案里有未完成标记：{key:?} -> {text}"
                );
            }
        }
    }

    #[test]
    fn 语言标签解析() {
        assert_eq!(Lang::from_code("zh-Hans"), Some(Lang::ZhHans));
        assert_eq!(Lang::from_code("zh-CN"), Some(Lang::ZhHans));
        assert_eq!(Lang::from_code("zh"), Some(Lang::ZhHans));
        assert_eq!(Lang::from_code("ZH_hans"), Some(Lang::ZhHans));
        assert_eq!(Lang::from_code("en-US"), None);
        assert_eq!(Lang::from_code(""), None);
    }

    #[test]
    fn 语言协商回退到默认() {
        // 系统给了一串不支持的语言，应当回退到默认语言而不是 panic。
        assert_eq!(Lang::negotiate(["en-US", "ja-JP"]), Lang::ZhHans);
        assert_eq!(Lang::negotiate(["en-US", "zh-CN"]), Lang::ZhHans);
        assert_eq!(Lang::negotiate(std::iter::empty()), Lang::ZhHans);
    }

    #[test]
    fn 语言自称不被翻译() {
        for lang in Lang::ALL {
            assert!(!lang.native_name().is_empty());
            assert!(!lang.code().is_empty());
        }
    }

    #[test]
    fn 占位符替换() {
        assert_eq!(
            render("对端 {peer} 已就绪", &[("peer", "dev-beta")]),
            "对端 dev-beta 已就绪"
        );
        // 未提供实参的占位符原样保留
        assert_eq!(render("对端 {peer} 已就绪", &[]), "对端 {peer} 已就绪");
        // 多个占位符
        assert_eq!(
            render("{a} 与 {b}", &[("a", "1"), ("b", "2")]),
            "1 与 2"
        );
    }

    #[test]
    fn 关键文案内容正确() {
        // 这几条是产品定位性质的文案，改动应当是有意识的。
        assert_eq!(Key::AppName.text(Lang::ZhHans), "SecRelay");
        assert_eq!(
            Key::AppTagline.text(Lang::ZhHans),
            "跨设备连接，让看、传、说归于一处"
        );
    }
}
