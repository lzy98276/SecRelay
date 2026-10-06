//! SecRelay 桌面客户端。
//!
//! # 分层
//!
//! ```text
//! ui/app.slint   界面：左侧导航 + 分页面内容（不含任何面向用户的字面量，全部由 i18n 注入）
//! src/bridge.rs  UI ↔ 核心 的桥接：只显示状态、发出意图
//! secrelay-*     核心（协议 / 传输 / 会话 / 采集），不依赖任何 UI 框架
//! ```
//!
//! 这条边界是需求分析 §6.1 的核心建议：**UI 框架是可替换的一层壳**。
//! 所以这个 crate 是唯一允许依赖 Slint 的地方，`crates/*` 里没有一处 UI 依赖。
//!
//! # 日志
//!
//! 诊断信息**不进界面**，写进 [`bridge::log_dir`] 下的按天滚动文件；
//! 界面只在设置页提供一个"打开日志目录"的入口。用户视角应该感觉不到日志的存在。

slint::include_modules!();

mod bridge;

use std::time::Duration;

use secrelay_i18n::{Key, Lang};
use slint::ComponentHandle;

fn main() -> anyhow::Result<()> {
    // 持有 guard 直到进程退出，否则后台写日志线程会被提前停掉。
    let _log_guard = init_logging();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %bridge::log_dir().display(),
        "SecRelay 桌面客户端启动"
    );

    // 语言协商：目前只有简体中文，所以直接用默认值。
    // 将来在这里接"系统语言 / 用户配置"，Lang::negotiate 已经准备好了。
    let lang = Lang::default();

    let ui = AppWindow::new()?;
    bridge::apply_language(&ui, lang);
    apply_system_accent(&ui);

    // 连接 / 断开
    let weak = ui.as_weak();
    ui.on_connect_clicked(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if bridge::is_connected() {
            // 断开：状态由会话线程收尾，避免这里和后台线程抢着写界面。
            bridge::request_disconnect();
        } else {
            bridge::spawn_demo_session(ui.as_weak(), lang);
        }
    });

    // 发送文字
    let weak = ui.as_weak();
    ui.on_send_clicked(move |text| {
        let text = text.to_string();
        if text.trim().is_empty() {
            return;
        }
        if bridge::send_text(text) {
            // 发送成功才清空输入框；失败时保留内容，免得用户白打一遍。
            if let Some(ui) = weak.upgrade() {
                ui.set_message_input("".into());
            }
        }
    });

    // 本机画面预览开关
    let weak = ui.as_weak();
    ui.on_preview_toggle_clicked(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if bridge::is_previewing() {
            bridge::stop_preview();
        } else {
            bridge::start_preview(ui.as_weak(), lang);
        }
    });

    // 打开日志目录
    ui.on_open_log_dir_clicked(|| {
        if let Err(err) = bridge::open_log_dir() {
            tracing::warn!("打开日志目录失败：{err}");
        }
    });

    // --page N：启动即打开指定页面（方便截图与演示）
    if let Some(page) = arg_value("--page").and_then(|v| v.parse::<i32>().ok()) {
        ui.set_current_page(page.clamp(0, 5));
    }

    // --demo：自动连接并发两条消息，方便截图、录屏与给别人演示。
    if std::env::args().any(|arg| arg == "--demo") {
        schedule_demo(&ui, lang);
    }

    // --preview：启动即开始采集本机画面（配合 --demo 可一次跑出完整截图）。
    if std::env::args().any(|arg| arg == "--preview") {
        bridge::start_preview(ui.as_weak(), lang);
    }

    ui.run()?;
    Ok(())
}

/// 读取 `--flag value` 形式的参数值。
fn arg_value(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let index = args.iter().position(|a| a == flag)?;
    args.get(index + 1).cloned()
}

/// 深色界面基底色。**必须与 `ui/app.slint` 里 `Theme.bg` 一致** ——
/// 强调色的"柔和底色"要在 Rust 里按这个基底混合。
const DARK_BASE: secrelay_theme::Rgb = secrelay_theme::Rgb::new(0x15, 0x16, 0x1A);

/// 把系统强调色注入 UI。
///
/// 取不到系统主题色时会用内置默认色（见 `secrelay-theme` 的取值顺序）。
/// 无论哪种情况都会记一条日志 —— 用户反馈"颜色不对"时，这是第一个要看的线索。
fn apply_system_accent(ui: &AppWindow) {
    let accent = secrelay_theme::system_accent(DARK_BASE);
    tracing::info!(
        color = %accent.color,
        source = ?accent.source,
        "应用主题色"
    );

    let theme = ui.global::<Theme>();
    theme.set_accent(slint::Color::from_rgb_u8(
        accent.color.r,
        accent.color.g,
        accent.color.b,
    ));
    theme.set_accent_soft(slint::Color::from_rgb_u8(
        accent.soft.r,
        accent.soft.g,
        accent.soft.b,
    ));
}

/// `--demo` 的自动播放脚本。
fn schedule_demo(ui: &AppWindow, lang: Lang) {
    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_millis(700), move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_connect_clicked();
        }
    });

    // 握手需要一点时间，稍后再发消息；两条消息能看出双向链路都通。
    for delay in [2000_u64, 2700] {
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(delay), move || {
            if let Some(ui) = weak.upgrade() {
                let text: slint::SharedString = Key::DemoMessage.text(lang).into();
                ui.invoke_send_clicked(text);
            }
        });
    }
}

/// 初始化日志：写按天滚动的文件，不输出到界面。
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::EnvFilter;

    let dir = bridge::log_dir();
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!("无法创建日志目录 {}：{err}", dir.display());
        return None;
    }

    let appender = tracing_appender::rolling::daily(&dir, "secrelay.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init();

    Some(guard)
}
