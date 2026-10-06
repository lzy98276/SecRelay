//! SecRelay 桌面客户端。
//!
//! # 分层
//!
//! ```text
//! ui/app.slint   界面：主窗口（导航 + 分页面）与设置窗口（独立）
//! src/bridge.rs  UI ↔ 核心 的桥接：只显示状态、发出意图
//! secrelay-*     核心（协议 / 传输 / 会话 / 采集 / 主题 / i18n），不依赖任何 UI 框架
//! ```
//!
//! 这条边界是需求分析 §6.1 的核心建议：**UI 框架是可替换的一层壳**。
//! 所以这个 crate 是唯一允许依赖 Slint 的地方，`crates/*` 里没有一处 UI 依赖。
//!
//! # 两个窗口
//!
//! - **主窗口**：左侧导航（功能页面 + 底部账号与设置），右侧当前页面。
//! - **设置窗口**：独立窗口，由导航栏底部的「设置」打开。主题、语言、诊断都在这里。
//!
//! # 日志
//!
//! 诊断信息**不进界面**，写进 [`bridge::log_dir`] 下的按天滚动文件；
//! 界面只在设置窗口提供一个"打开日志目录"的入口。

slint::include_modules!();

mod bridge;

use std::time::Duration;

use secrelay_i18n::{Key, Lang};
use secrelay_theme::{ColorScheme, Palette, Preferences, Rgb, ThemeMode};
use slint::ComponentHandle;

fn main() -> anyhow::Result<()> {
    // 持有 guard 直到进程退出，否则后台写日志线程会被提前停掉。
    let _log_guard = init_logging();

    // 语言协商：目前只有简体中文，所以直接用默认值。
    // 将来在这里接"系统语言 / 用户配置"，Lang::negotiate 已经准备好了。
    let lang = Lang::default();

    // 主题：读用户偏好 + 探测系统设置
    let preferences = Preferences::load();
    let accent = secrelay_theme::detect_accent();
    let system_scheme = secrelay_theme::platform::detect_color_scheme();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %bridge::log_dir().display(),
        accent = %accent.color,
        accent_source = ?accent.source,
        system_scheme = ?system_scheme,
        theme_mode = ?preferences.theme_mode,
        "SecRelay 桌面客户端启动"
    );

    // --settings-only：只打开设置窗口。
    //
    // 用于测试与截图。Slint 的 `run()` 在主窗口不显示时会立刻返回，所以这条路径
    // 干脆不创建主窗口，而不是创建了再隐藏。
    if std::env::args().any(|arg| arg == "--settings-only") {
        let settings = SettingsWindow::new()?;
        bridge::apply_language_to_settings(&settings, lang);
        let palette = Palette::build(preferences.theme_mode.resolve(system_scheme), accent.color);
        bridge::apply_palette(settings.global::<Theme>(), &palette);
        settings.set_theme_mode(preferences.theme_mode.index());
        settings.on_open_log_dir_clicked(open_logs);
        settings.run()?;
        return Ok(());
    }

    let ui = AppWindow::new()?;
    let settings = SettingsWindow::new()?;

    bridge::apply_language(&ui, lang);
    bridge::apply_language_to_settings(&settings, lang);

    // 初始主题
    let accent_color = accent.color;
    let palette = push_theme(
        &ui,
        &settings,
        preferences.theme_mode,
        accent_color,
        system_scheme,
    );
    tracing::info!(
        scheme = ?preferences.theme_mode.resolve(system_scheme),
        accent = %palette.accent,
        "应用主题"
    );
    settings.set_theme_mode(preferences.theme_mode.index());
    settings.hide()?;

    // --page N：启动即打开指定页面（方便截图与演示）
    if let Some(page) = arg_value("--page").and_then(|value| value.parse::<i32>().ok()) {
        ui.set_current_page(page.clamp(0, 4));
    }

    // ── 设置窗口的开关
    let settings_weak = settings.as_weak();
    ui.on_settings_clicked(move || {
        if let Some(settings) = settings_weak.upgrade() {
            if let Err(err) = settings.show() {
                tracing::warn!("打开设置窗口失败：{err}");
            }
        }
    });

    let settings_weak = settings.as_weak();
    settings.on_close_clicked(move || {
        if let Some(settings) = settings_weak.upgrade() {
            let _ = settings.hide();
        }
    });

    // 用户点设置窗口的关闭按钮：隐藏而不是退出（退出由主窗口负责）
    //
    // `close-requested` 不在组件上，要通过 Window 句柄拿。
    let settings_weak = settings.as_weak();
    settings.window().on_close_requested(move || {
        if let Some(settings) = settings_weak.upgrade() {
            let _ = settings.hide();
        }
        slint::CloseRequestResponse::HideWindow
    });

    // 主窗口关闭时把设置窗口一起收掉，否则 Slint 会认为还有窗口没关。
    let settings_weak = settings.as_weak();
    ui.window().on_close_requested(move || {
        if let Some(settings) = settings_weak.upgrade() {
            let _ = settings.hide();
        }
        slint::CloseRequestResponse::HideWindow
    });

    // ── 主题切换
    let ui_weak = ui.as_weak();
    let settings_weak = settings.as_weak();
    settings.on_theme_mode_changed(move |index| {
        let mode = ThemeMode::from_index(index);
        let updated = Preferences { theme_mode: mode };
        if let Err(err) = updated.save() {
            // 写盘失败只记录：本次会话的选择仍然要生效。
            tracing::warn!("保存主题偏好失败：{err}");
        }

        let (Some(ui), Some(settings)) = (ui_weak.upgrade(), settings_weak.upgrade()) else {
            return;
        };
        settings.set_theme_mode(mode.index());
        let palette = push_theme(&ui, &settings, mode, accent_color, system_scheme);
        tracing::info!(
            mode = ?mode,
            scheme = ?mode.resolve(system_scheme),
            accent = %palette.accent,
            "主题已切换"
        );
    });

    // ── 连接 / 断开
    let weak = ui.as_weak();
    ui.on_connect_clicked(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if bridge::is_connected() {
            bridge::request_disconnect();
        } else {
            bridge::spawn_demo_session(ui.as_weak(), lang);
        }
    });

    // ── 发送文字
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

    // ── 本机画面预览开关
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

    // ── 打开日志目录（两个窗口都有入口）
    ui.on_open_log_dir_clicked(open_logs);
    settings.on_open_log_dir_clicked(open_logs);

    // ── 登录入口
    // 位置已经就位（导航栏底部、设置上方），但账号系统接入尚未实现。
    // 设计见 docs/账号系统接入.md；这里不做假登录。
    ui.on_login_clicked(|| {
        tracing::info!("登录入口被点击：SECTL-auth 接入尚未实现（见 docs/账号系统接入.md）");
    });

    // --demo：自动连接并发两条消息，方便截图、录屏与给别人演示。
    if std::env::args().any(|arg| arg == "--demo") {
        schedule_demo(&ui, lang);
    }

    // --preview：启动即开始采集本机画面（配合 --demo 可一次跑出完整截图）。
    if std::env::args().any(|arg| arg == "--preview") {
        bridge::start_preview(ui.as_weak(), lang);
    }

    // --settings：启动即打开设置窗口。
    if std::env::args().any(|arg| arg == "--settings") {
        settings.show()?;
    }

    ui.run()?;
    Ok(())
}

/// 按主题模式重新计算调色板并注入两个窗口。
fn push_theme(
    ui: &AppWindow,
    settings: &SettingsWindow,
    mode: ThemeMode,
    accent: Rgb,
    system: ColorScheme,
) -> Palette {
    let palette = Palette::build(mode.resolve(system), accent);
    bridge::apply_palette(ui.global::<Theme>(), &palette);
    bridge::apply_palette(settings.global::<Theme>(), &palette);
    palette
}

fn open_logs() {
    if let Err(err) = bridge::open_log_dir() {
        tracing::warn!("打开日志目录失败：{err}");
    }
}

/// 读取 `--flag value` 形式的参数值。
fn arg_value(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let index = args.iter().position(|arg| arg == flag)?;
    args.get(index + 1).cloned()
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
