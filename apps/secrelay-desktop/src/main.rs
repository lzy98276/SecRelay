//! SecRelay 桌面客户端。
//!
//! 界面用 Slint 官方 Material 组件库搭（见 `ui/app.slint`），本文件只负责：
//! 组装窗口、把配置与探测结果注入 `Strings` / `UiFont`、转发用户操作。

slint::include_modules!();

mod bridge;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use secrelay_i18n::{weight_name, Key, Lang};
use secrelay_theme::{accent_from_hsv, AccentMode, ColorScheme as ThemeColorScheme, FontCatalog, Preferences, Rgb, ThemeMode};
use slint::{ComponentHandle, ModelRc, VecModel};

fn main() -> anyhow::Result<()> {
    let _log_guard = init_logging();

    let lang = Lang::default();
    let mut preferences = Preferences::load();
    let detected_accent = secrelay_theme::detect_accent();
    let system_scheme = secrelay_theme::platform::detect_color_scheme();

    let started = std::time::Instant::now();
    let catalog = Rc::new(FontCatalog::load());
    tracing::info!(
        families = catalog.families().len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "已枚举系统字体"
    );
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        accent = %detected_accent.color,
        accent_source = ?detected_accent.source,
        system_scheme = ?system_scheme,
        "启动"
    );

    let state = Rc::new(RefCell::new(UiState {
        theme_mode: preferences.theme_mode,
        accent_mode: preferences.accent_mode,
        hue: preferences.hue,
        saturation: preferences.saturation,
        family: preferences.font_family.clone(),
        weight_tier: preferences.font_weight,
    }));

    // --settings-only：只创建设置窗口，用于测试与截图。
    // Slint 的 run() 会在没有可见窗口时立刻返回，所以这条路径不创建主窗口。
    if std::env::args().any(|arg| arg == "--settings-only") {
        let settings = SettingsWindow::new()?;
        bridge::apply_language_to_settings(&settings, lang);
        apply_appearance(&settings, None, &state.borrow(), detected_accent.color, system_scheme, &catalog, lang);
        push_font_options(&settings, &catalog, &state.borrow(), lang);
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
    {
        let state = state.borrow();
        apply_appearance(&settings, Some(&ui), &state, detected_accent.color, system_scheme, &catalog, lang);
        push_font_options(&settings, &catalog, &state, lang);
    }
    apply_version(&settings);
    settings.set_theme_mode(preferences.theme_mode.index());
    settings.hide()?;

    if let Some(page) = arg_value("--page").and_then(|value| value.parse::<i32>().ok()) {
        ui.set_current_page(page.clamp(0, 4));
    }

    // ── 设置窗口开关
    let settings_weak = settings.as_weak();
    ui.on_settings_clicked(move || {
        if let Some(settings) = settings_weak.upgrade() {
            if let Err(err) = settings.show() {
                tracing::warn!("打开设置窗口失败：{err}");
            }
        }
    });

    let settings_weak = settings.as_weak();
    settings.window().on_close_requested(move || {
        if let Some(settings) = settings_weak.upgrade() {
            let _ = settings.hide();
        }
        slint::CloseRequestResponse::HideWindow
    });

    let settings_weak = settings.as_weak();
    ui.window().on_close_requested(move || {
        if let Some(settings) = settings_weak.upgrade() {
            let _ = settings.hide();
        }
        slint::CloseRequestResponse::HideWindow
    });

    // ── 主题（浅色/深色）
    {
        let ui_weak = ui.as_weak();
        let settings_weak = settings.as_weak();
        let state = state.clone();
        settings.on_theme_mode_changed(move |index| {
            let mode = ThemeMode::from_index(index);
            state.borrow_mut().theme_mode = mode;
            save(&current_prefs(&state.borrow()));
            if let (Some(ui), Some(settings)) = (ui_weak.upgrade(), settings_weak.upgrade()) {
                settings.set_theme_mode(mode.index());
                apply_accent(&ui, &settings, &state.borrow(), detected_accent.color);
            }
        });
    }

    // ── 主题色模式
    {
        let ui_weak = ui.as_weak();
        let settings_weak = settings.as_weak();
        let state = state.clone();
        let catalog = catalog.clone();
        settings.on_accent_mode_changed(move |index| {
            let mode = AccentMode::from_index(index);
            state.borrow_mut().accent_mode = mode;
            save(&current_prefs(&state.borrow()));
            if let (Some(ui), Some(settings)) = (ui_weak.upgrade(), settings_weak.upgrade()) {
                settings.set_accent_mode(mode.index());
                settings.set_accent_preview(color_of(accent_seed(&state.borrow(), detected_accent.color)));
                apply_accent(&ui, &settings, &state.borrow(), detected_accent.color);
                let state = state.borrow();
                push_font_options(&settings, &catalog, &state, lang);
            }
        });
    }

    // ── 色盘拖拽
    {
        let ui_weak = ui.as_weak();
        let settings_weak = settings.as_weak();
        let state = state.clone();
        settings.on_accent_changed(move |hue, saturation| {
            {
                let mut state = state.borrow_mut();
                state.hue = hue;
                state.saturation = saturation;
            }
            save(&current_prefs(&state.borrow()));
            if let (Some(ui), Some(settings)) = (ui_weak.upgrade(), settings_weak.upgrade()) {
                let seed = accent_from_hsv(hue, saturation / 100.0);
                settings.set_hue(hue);
                settings.set_saturation(saturation);
                settings.set_accent_preview(color_of(seed));
                apply_accent(&ui, &settings, &state.borrow(), detected_accent.color);
            }
        });
    }

    // ── 字体与字重
    {
        let ui_weak = ui.as_weak();
        let settings_weak = settings.as_weak();
        let state = state.clone();
        let catalog = catalog.clone();
        settings.on_family_selected(move |index| {
            let families = catalog.families();
            let Some(family) = families.get(index.max(0) as usize).cloned() else {
                return;
            };
            state.borrow_mut().family = family.clone();
            save(&current_prefs(&state.borrow()));
            let state_ref = state.borrow();
            apply_font(&ui_weak, &settings_weak, &catalog, &state_ref);
            if let Some(settings) = settings_weak.upgrade() {
                push_font_options(&settings, &catalog, &state_ref, lang);
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let settings_weak = settings.as_weak();
        let state = state.clone();
        let catalog = catalog.clone();
        settings.on_weight_selected(move |index| {
            let family = state.borrow().family.clone();
            let tiers = catalog.selectable_weights(&family);
            let Some(tier) = tiers.get(index.max(0) as usize).copied() else {
                return;
            };
            state.borrow_mut().weight_tier = tier;
            save(&current_prefs(&state.borrow()));
            let state_ref = state.borrow();
            apply_font(&ui_weak, &settings_weak, &catalog, &state_ref);
            if let Some(settings) = settings_weak.upgrade() {
                push_font_options(&settings, &catalog, &state_ref, lang);
            }
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
            // 发送成功才清空输入框，失败时保留内容
            if let Some(ui) = weak.upgrade() {
                ui.set_message_input("".into());
            }
        }
    });

    // ── 本机画面预览
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

    ui.on_open_log_dir_clicked(open_logs);
    settings.on_open_log_dir_clicked(open_logs);

    // 登录入口已就位（顶部账号栏），账号系统尚未接入，不做假登录
    ui.on_login_clicked(|| {
        tracing::info!("登录入口被点击：账号系统尚未接入");
    });

    if std::env::args().any(|arg| arg == "--demo") {
        schedule_demo(&ui, lang);
    }
    if std::env::args().any(|arg| arg == "--preview") {
        bridge::start_preview(ui.as_weak(), lang);
    }
    if std::env::args().any(|arg| arg == "--settings") {
        settings.show()?;
    }

    ui.run()?;
    Ok(())
}

/// 当前外观选择。
struct UiState {
    theme_mode: ThemeMode,
    accent_mode: AccentMode,
    hue: f32,
    saturation: f32,
    family: String,
    weight_tier: u16,
}

/// 强调色种子：跟随系统时用探测到的系统色，自定义时由色相 + 饱和度算。
fn accent_seed(state: &UiState, detected: Rgb) -> Rgb {
    match state.accent_mode {
        AccentMode::System => detected,
        AccentMode::Custom => accent_from_hsv(state.hue, state.saturation / 100.0),
    }
}

fn color_of(rgb: Rgb) -> slint::Color {
    slint::Color::from_rgb_u8(rgb.r, rgb.g, rgb.b)
}

/// 把强调色交给 Slint 的内置风格（Fluent 风格据此推导整套配色）。
///
/// 走的是私有 API：`WindowInner::context()` 与 `SlintContext::set_accent_color` 都是 pub，
/// 但被放在 `private_unstable_api` 里，Slint 升级时不保证兼容。升级 Slint 后要重新确认。
fn set_accent<C: ComponentHandle>(component: &C, color: slint::Color) {
    use slint::private_unstable_api::re_exports::WindowInner;
    WindowInner::from_pub(component.window())
        .context()
        .set_accent_color(color);
}

/// 把强调色推给两个窗口。内置风格据此推导整套配色，布局里的 `Palette.*` 也跟着变。
fn apply_accent(ui: &AppWindow, settings: &SettingsWindow, state: &UiState, detected: Rgb) {
    let color = color_of(accent_seed(state, detected));
    set_accent(ui, color);
    set_accent(settings, color);
}

/// 注入外观相关的一切：强调色、设置项的当前值、字体。
fn apply_appearance(
    settings: &SettingsWindow,
    ui: Option<&AppWindow>,
    state: &UiState,
    detected: Rgb,
    _system_scheme: ThemeColorScheme,
    catalog: &FontCatalog,
    lang: Lang,
) {
    let seed = accent_seed(state, detected);
    let color = color_of(seed);
    if let Some(ui) = ui {
        set_accent(ui, color);
    }
    set_accent(settings, color);

    settings.set_accent_mode(state.accent_mode.index());
    settings.set_hue(state.hue);
    settings.set_saturation(state.saturation);
    settings.set_accent_preview(color);
    settings.set_theme_mode(state.theme_mode.index());
    settings.set_theme_options(options(&[
        Key::ThemeFollowSystem,
        Key::ThemeLight,
        Key::ThemeDark,
    ], lang));
    settings.set_accent_options(options(&[Key::AccentFollowSystem, Key::AccentCustom], lang));

    let resolved = catalog.resolve(&state.family, state.weight_tier);
    bridge::apply_fonts(settings.global::<UiFont>(), &resolved);
    if let Some(ui) = ui {
        bridge::apply_fonts(ui.global::<UiFont>(), &resolved);
    }
    tracing::info!(
        accent = %seed,
        family = %resolved.family,
        weight = resolved.weight,
        "应用外观"
    );
}

/// 把一组文案做成下拉框的模型。
fn options(keys: &[Key], lang: Lang) -> ModelRc<slint::SharedString> {
    let items: Vec<slint::SharedString> = keys
        .iter()
        .map(|key| slint::SharedString::from(key.text(lang)))
        .collect();
    ModelRc::new(VecModel::from(items))
}

fn apply_font(
    ui: &slint::Weak<AppWindow>,
    settings: &slint::Weak<SettingsWindow>,
    catalog: &FontCatalog,
    state: &UiState,
) {
    let resolved = catalog.resolve(&state.family, state.weight_tier);
    if let Some(ui) = ui.upgrade() {
        bridge::apply_fonts(ui.global::<UiFont>(), &resolved);
    }
    if let Some(settings) = settings.upgrade() {
        bridge::apply_fonts(settings.global::<UiFont>(), &resolved);
    }
    tracing::info!(
        requested = %state.family,
        family = %resolved.family,
        weight = resolved.weight,
        "界面字体已切换"
    );
}

/// 把字体列表 / 字重列表 / 当前选中项推给设置窗口。
fn push_font_options(
    settings: &SettingsWindow,
    catalog: &FontCatalog,
    state: &UiState,
    lang: Lang,
) {
    let families = catalog.families();
    let family_index = families.iter().position(|f| f == &state.family).unwrap_or(0) as i32;

    let tiers = catalog.selectable_weights(&state.family);
    let tier = catalog.nearest_tier(&state.family, state.weight_tier);
    let weight_index = tiers.iter().position(|t| *t == tier).unwrap_or(0) as i32;

    let weight_labels: Vec<slint::SharedString> = tiers
        .iter()
        .map(|tier| {
            let name = weight_name(*tier, lang);
            let text = if name.is_empty() {
                format!("{tier}")
            } else {
                format!("{tier}   {name}")
            };
            slint::SharedString::from(text)
        })
        .collect();

    let family_labels: Vec<slint::SharedString> = families
        .iter()
        .map(|f| slint::SharedString::from(f.as_str()))
        .collect();
    settings.set_families(options_model(family_labels));
    settings.set_family_index(family_index);
    settings.set_weights(options_model(weight_labels));
    settings.set_weight_index(weight_index);
}

/// 下拉框的字符串模型。
fn options_model(labels: Vec<slint::SharedString>) -> ModelRc<slint::SharedString> {
    ModelRc::new(VecModel::from(labels))
}
fn save(preferences: &Preferences) {
    // 写盘失败只记录，本次会话的选择仍然生效
    if let Err(err) = preferences.save() {
        tracing::warn!("保存偏好失败：{err}");
    }
}

/// 从当前状态拼出要落盘的偏好，几个回调共用。
fn current_prefs(state: &UiState) -> Preferences {
    Preferences {
        theme_mode: state.theme_mode,
        font_family: state.family.clone(),
        font_weight: state.weight_tier,
        accent_mode: state.accent_mode,
        hue: state.hue,
        saturation: state.saturation,
    }
}

fn open_logs() {
    if let Err(err) = bridge::open_log_dir() {
        tracing::warn!("打开日志目录失败：{err}");
    }
}

fn arg_value(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let index = args.iter().position(|arg| arg == flag)?;
    args.get(index + 1).cloned()
}

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

/// 日志写按天滚动的文件，不输出到界面。
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
