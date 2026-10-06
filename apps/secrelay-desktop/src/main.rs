//! SecRelay 桌面客户端。
//!
//! # 分层
//!
//! ```text
//! ui/app.slint   界面：主窗口（导航 + 分页面）与设置窗口（独立，同尺寸带侧边栏）
//! src/bridge.rs  UI ↔ 核心 的桥接：只显示状态、发出意图
//! secrelay-*     核心（协议 / 传输 / 会话 / 采集 / 主题 / 字体 / i18n），不依赖任何 UI 框架
//! ```
//!
//! 这条边界是需求分析 §6.1 的核心建议：**UI 框架是可替换的一层壳**。
//! 所以这个 crate 是唯一允许依赖 Slint 的地方，`crates/*` 里没有一处 UI 依赖。
//!
//! # 外观设置的流向
//!
//! ```text
//! 配置文件 ──► Preferences ──┐
//!                            ├──► FontCatalog::resolve ──► Theme (family + weight)
//! 系统字体目录 ──► FontCatalog ┘
//! ```
//!
//! 界面只拿到"最终该用什么 family、什么字重"，中间的取舍（miSans 靠换 family 表达
//! 字重、系统字体挑最近可用字重）都在 `secrelay-theme` 里，有测试覆盖。
//!
//! # 日志
//!
//! 诊断信息**不进界面**，写进 [`bridge::log_dir`] 下的按天滚动文件；
//! 界面只在设置窗口提供一个"打开日志目录"的入口。

slint::include_modules!();

mod bridge;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use secrelay_i18n::{weight_name, Key, Lang};
use secrelay_theme::{FontCatalog, Palette, Preferences, ThemeMode};
use slint::{ComponentHandle, ModelRc, VecModel};

fn main() -> anyhow::Result<()> {
    // 持有 guard 直到进程退出，否则后台写日志线程会被提前停掉。
    let _log_guard = init_logging();

    // 语言协商：目前只有简体中文，所以直接用默认值。
    // 将来在这里接"系统语言 / 用户配置"，Lang::negotiate 已经准备好了。
    let lang = Lang::default();

    let preferences = Preferences::load();
    let accent = secrelay_theme::detect_accent();
    let system_scheme = secrelay_theme::platform::detect_color_scheme();

    // 枚举系统字体。要读系统字体目录，只做一次。
    let started = std::time::Instant::now();
    let catalog = Rc::new(FontCatalog::load());
    tracing::info!(
        families = catalog.families().len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "已枚举系统字体"
    );

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %bridge::log_dir().display(),
        accent = %accent.color,
        accent_source = ?accent.source,
        system_scheme = ?system_scheme,
        theme_mode = ?preferences.theme_mode,
        font_family = %preferences.font_family,
        font_weight = preferences.font_weight,
        "SecRelay 桌面客户端启动"
    );

    let font_state = Rc::new(RefCell::new(FontState {
        family: preferences.font_family.clone(),
        weight: preferences.font_weight,
    }));

    // --settings-only：只打开设置窗口。
    //
    // 用于测试与截图。Slint 的 `run()` 在主窗口不显示时会立刻返回，所以这条路径
    // 干脆不创建主窗口，而不是创建了再隐藏。
    if std::env::args().any(|arg| arg == "--settings-only") {
        let settings = SettingsWindow::new()?;
        bridge::apply_language_to_settings(&settings, lang);
        let palette = Palette::build(preferences.theme_mode.resolve(system_scheme), accent.color);
        bridge::apply_palette(settings.global::<Theme>(), &palette);
        {
            let state = font_state.borrow();
            let resolved = catalog.resolve(&state.family, state.weight);
            bridge::apply_fonts(settings.global::<Theme>(), &resolved);
            push_font_options(&settings, &catalog, &state.family, state.weight, lang);
        }
        settings.set_theme_mode(preferences.theme_mode.index());
        apply_version(&settings);
        if let Some(page) = arg_value("--settings-page").and_then(|value| value.parse::<i32>().ok()) {
            settings.set_settings_page(page.clamp(0, 3));
        }
        settings.on_open_log_dir_clicked(open_logs);
        settings.run()?;
        return Ok(());
    }

    let ui = AppWindow::new()?;
    let settings = SettingsWindow::new()?;

    bridge::apply_language(&ui, lang);
    bridge::apply_language_to_settings(&settings, lang);

    // 初始外观
    let palette = {
        let scheme = preferences.theme_mode.resolve(system_scheme);
        let palette = Palette::build(scheme, accent.color);
        bridge::apply_palette(ui.global::<Theme>(), &palette);
        bridge::apply_palette(settings.global::<Theme>(), &palette);
        palette
    };
    {
        let state = font_state.borrow();
        let resolved = catalog.resolve(&state.family, state.weight);
        bridge::apply_fonts(ui.global::<Theme>(), &resolved);
        bridge::apply_fonts(settings.global::<Theme>(), &resolved);
        push_font_options(&settings, &catalog, &state.family, state.weight, lang);
        tracing::info!(
            scheme = ?preferences.theme_mode.resolve(system_scheme),
            accent = %palette.accent,
            family = %resolved.family,
            weight = resolved.weight,
            bold_weight = resolved.bold_weight,
            "应用外观"
        );
    }

    settings.set_theme_mode(preferences.theme_mode.index());
    apply_version(&settings);
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

    // ── 字体切换
    //
    // 两处都只改字体：保留用户的主题选择。
    {
        let catalog = catalog.clone();
        let state = font_state.clone();
        let theme_mode = preferences.theme_mode;
        let ui_weak = ui.as_weak();
        let settings_weak = settings.as_weak();
        settings.on_family_selected(move |index| {
            let families = catalog.families();
            let Some(family) = families.get(index.max(0) as usize).cloned() else {
                return;
            };
            let weight = {
                let mut state = state.borrow_mut();
                state.family = family.clone();
                state.weight
            };
            save_font(theme_mode, &family, weight);
            apply_font_selection(&catalog, &ui_weak, &settings_weak, &family, weight, lang);
        });
    }

    {
        let catalog = catalog.clone();
        let state = font_state.clone();
        let theme_mode = preferences.theme_mode;
        let ui_weak = ui.as_weak();
        let settings_weak = settings.as_weak();
        settings.on_weight_selected(move |index| {
            let family = state.borrow().family.clone();
            // 按当前字体实际提供的档位取，而不是按标准九档
            let weights = catalog.selectable_weights(&family);
            let Some(weight) = weights.get(index.max(0) as usize).copied() else {
                return;
            };
            state.borrow_mut().weight = weight;
            save_font(theme_mode, &family, weight);
            apply_font_selection(&catalog, &ui_weak, &settings_weak, &family, weight, lang);
        });
    }

    // ── 主题切换
    {
        let state = font_state.clone();
        let ui_weak = ui.as_weak();
        let settings_weak = settings.as_weak();
        settings.on_theme_mode_changed(move |index| {
            let mode = ThemeMode::from_index(index);
            let (family, weight) = {
                let state = state.borrow();
                (state.family.clone(), state.weight)
            };
            // 只改主题，字体保持用户当前的选择。
            let updated = Preferences {
                theme_mode: mode,
                font_family: family,
                font_weight: weight,
            };
            if let Err(err) = updated.save() {
                // 写盘失败只记录：本次会话的选择仍然要生效。
                tracing::warn!("保存主题偏好失败：{err}");
            }

            let (Some(ui), Some(settings)) = (ui_weak.upgrade(), settings_weak.upgrade()) else {
                return;
            };
            settings.set_theme_mode(mode.index());
            let scheme = mode.resolve(system_scheme);
            let palette = Palette::build(scheme, accent.color);
            bridge::apply_palette(ui.global::<Theme>(), &palette);
            bridge::apply_palette(settings.global::<Theme>(), &palette);
            tracing::info!(mode = ?mode, scheme = ?scheme, accent = %palette.accent, "主题已切换");
        });
    }

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

/// 当前字体选择。放在 `Rc<RefCell<..>>` 里，供几个回调共享。
struct FontState {
    family: String,
    weight: u16,
}

/// 把字体选择推给两个窗口。
fn apply_font_selection(
    catalog: &FontCatalog,
    ui: &slint::Weak<AppWindow>,
    settings: &slint::Weak<SettingsWindow>,
    family: &str,
    weight: u16,
    lang: Lang,
) {
    let (Some(ui), Some(settings)) = (ui.upgrade(), settings.upgrade()) else {
        return;
    };
    let resolved = catalog.resolve(family, weight);
    bridge::apply_fonts(ui.global::<Theme>(), &resolved);
    bridge::apply_fonts(settings.global::<Theme>(), &resolved);
    // 字重可能被调整到该字体实际存在的档位，所以下拉框要重新同步
    push_font_options(&settings, catalog, family, weight, lang);
    tracing::info!(
        requested = %family,
        resolved_family = %resolved.family,
        weight = resolved.weight,
        bold_weight = resolved.bold_weight,
        "界面字体已切换"
    );
}

/// 把字体列表 / 字重列表 / 当前选中项推给设置窗口。
fn push_font_options(
    settings: &SettingsWindow,
    catalog: &FontCatalog,
    family: &str,
    weight: u16,
    lang: Lang,
) {
    let families = catalog.families();
    let family_index = families.iter().position(|f| f == family).unwrap_or(0) as i32;

    let weights = catalog.selectable_weights(family);
    // 用解析结果反查下标：这样下拉框显示的就是真正生效的那个字重，
    // 而不是用户点了但实际不存在的档位。
    let effective = catalog.resolve(family, weight).weight;
    let weight_index = weights.iter().position(|w| *w == effective).unwrap_or(0) as i32;

    let labels: Vec<slint::SharedString> = weights
        .iter()
        .map(|w| {
            let name = weight_name(*w, lang);
            let text = if name.is_empty() {
                format!("{w}")
            } else {
                format!("{w}   {name}")
            };
            slint::SharedString::from(text)
        })
        .collect();

    settings.set_families(ModelRc::new(VecModel::from(
        families
            .iter()
            .map(|f| slint::SharedString::from(f.as_str()))
            .collect::<Vec<_>>(),
    )));
    settings.set_family_index(family_index);
    settings.set_weights(ModelRc::new(VecModel::from(labels)));
    settings.set_weight_index(weight_index);
}

/// 保存字体选择（保留主题选择）。
fn save_font(theme_mode: ThemeMode, family: &str, weight: u16) {
    let updated = Preferences {
        theme_mode,
        font_family: family.to_string(),
        font_weight: weight,
    };
    if let Err(err) = updated.save() {
        tracing::warn!("保存字体偏好失败：{err}");
    }
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

/// 版本号显示在「关于」页。编译期确定，不用运行时读。
fn apply_version(settings: &SettingsWindow) {
    settings.set_version(concat!("v", env!("CARGO_PKG_VERSION")).into());
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
