//! SecRelay 桌面客户端。
//!
//! 界面用 Slint 官方 Material 组件库搭（见 `ui/app.slint`），本文件只负责：
//! 组装窗口、把配置与探测结果注入 `Strings` / `UiFont`、转发用户操作。

slint::include_modules!();

mod bridge;
mod relay;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use secrelay_i18n::{weight_name, Key, Lang};
use secrelay_theme::{accent_from_hsv, AccentMode, ColorScheme as ThemeColorScheme, FontCatalog, Preferences, Relays, Rgb, ThemeMode};
use slint::{ComponentHandle, ModelRc, VecModel};

fn main() -> anyhow::Result<()> {
    let _log_guard = init_logging();

    let lang = Lang::default();
    let preferences = Preferences::load();
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
        relays: preferences.relays.clone(),
        relay_ui: relay::RelayUi::new(&preferences.relays),
        relay_timer: None,
    }));

    // --settings-only：只创建设置窗口，用于测试与截图。
    // Slint 的 run() 会在没有可见窗口时立刻返回，所以这条路径不创建主窗口。
    if std::env::args().any(|arg| arg == "--settings-only") {
        let settings = SettingsWindow::new()?;
        bridge::apply_language_to_settings(&settings, lang);
        apply_appearance(&settings, None, &state.borrow(), detected_accent.color, system_scheme, &catalog, lang);
        push_font_options(&settings, &catalog, &state.borrow(), lang);
        apply_version(&settings);
        push_relays(&settings, &state.borrow(), lang);
        if let Some(page) = arg_value("--settings-page").and_then(|value| value.parse::<i32>().ok()) {
            settings.set_settings_page(page.clamp(0, 3));
        }
        settings.on_open_log_dir_clicked(open_logs);
        wire_relay_callbacks(&settings, &state, lang);
        start_relay_watch(&settings, &state, lang);
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
        push_relays(&settings, &state, lang);
    }
    apply_version(&settings);
    settings.set_theme_mode(preferences.theme_mode.index());
    settings.hide()?;

    // 中继的核对在后台跑，结果由定时器搬到界面上
    wire_relay_callbacks(&settings, &state, lang);
    start_relay_watch(&settings, &state, lang);

    // 建连状态由另一个定时器从工作线程搬过来
    bridge::install_status_pump(ui.as_weak());

    if let Some(page) = arg_value("--page").and_then(|value| value.parse::<i32>().ok()) {
        ui.set_current_page(page.clamp(0, 3));
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
    //
    // 建连跑在工作线程上，这里只决定"发起 / 加入 / 断开"，把活交给 bridge。
    // 本机 UDP 绑定地址默认是所有网卡的随机端口，`--bind` 可以指定。
    let bind = bind_addrs();
    {
        let weak = ui.as_weak();
        let connect_state = Rc::clone(&state);
        let connect_bind = Rc::clone(&bind);
        ui.on_connect_clicked(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            match bridge::connect_action(&ui.get_join_code()) {
                bridge::ConnectAction::Disconnect => bridge::request_disconnect(),
                bridge::ConnectAction::Offer => {
                    begin_session(&ui, &connect_state, &connect_bind, lang, "")
                }
                bridge::ConnectAction::Join => {
                    let code = ui.get_join_code().to_string();
                    if !bridge::session_code_valid(&code) {
                        ui.set_join_error_text(join_error(lang, &code));
                        return;
                    }
                    ui.set_join_error_text("".into());
                    begin_session(&ui, &connect_state, &connect_bind, lang, &code);
                }
            }
        });

        // ── 加入按钮：语义同"填了会话码再点连接"
        let weak = ui.as_weak();
        let join_state = Rc::clone(&state);
        let join_bind = Rc::clone(&bind);
        ui.on_join_clicked(move |code| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let code = code.to_string();
            if !bridge::session_code_valid(&code) {
                ui.set_join_error_text(join_error(lang, &code));
                return;
            }
            ui.set_join_error_text("".into());
            begin_session(&ui, &join_state, &join_bind, lang, &code);
        });
    }

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

    ui.on_open_log_dir_clicked(open_logs);
    settings.on_open_log_dir_clicked(open_logs);

    // 登录入口已就位（顶部账号栏），账号系统尚未接入，不做假登录
    ui.on_login_clicked(|| {
        tracing::info!("登录入口被点击：账号系统尚未接入");
    });

    if std::env::args().any(|arg| arg == "--settings") {
        settings.show()?;
    }

    // 脚本化入口：两个实例可以用窗口按钮走完整流程，也可以用这几个参数自动走一遍。
    if let Some(code) = arg_value("--join") {
        ui.set_join_code(code.into());
    }
    if std::env::args().any(|arg| arg == "--connect") || arg_value("--join").is_some() {
        let weak = ui.as_weak();
        let state = state.clone();
        slint::Timer::single_shot(Duration::from_millis(900), move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let code = ui.get_join_code().to_string();
            begin_session(&ui, &state, &bind, lang, &code);
        });
    }
    if let Some(text) = arg_value("--send") {
        // 会话建好之前发不出去，所以按定时器重试，直到界面真的连上。
        let weak = ui.as_weak();
        let attempts = Rc::new(std::cell::Cell::new(0_u32));
        let timer = Rc::new(slint::Timer::default());
        let stop = timer.clone();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(300),
            move || {
                let count = attempts.get() + 1;
                attempts.set(count);
                if count > 60 {
                    stop.stop();
                    return;
                }
                let Some(ui) = weak.upgrade() else {
                    stop.stop();
                    return;
                };
                if !bridge::is_connected() {
                    return;
                }
                if bridge::send_text(text.clone()) {
                    ui.set_message_input("".into());
                    stop.stop();
                }
            },
        );
    }

    ui.run()?;
    Ok(())
}

/// 本机 UDP 绑定地址：`--bind addr[,addr]`，缺省绑所有网卡的随机端口。
fn bind_addrs() -> Rc<Vec<String>> {
    Rc::new(
        arg_value("--bind")
            .map(|value| {
                value
                    .split(',')
                    .map(|addr| addr.trim().to_string())
                    .filter(|addr| !addr.is_empty())
                    .collect()
            })
            .unwrap_or_else(secrelay_transport::ice::default_udp_addrs),
    )
}

/// 会话码不合法时的提示文案。
fn join_error(lang: Lang, code: &str) -> slint::SharedString {
    Key::DeviceJoinError
        .format(lang, &[("detail", code.trim())])
        .into()
}

/// 按当前选中的中继发起一次建连。
///
/// 失败只提示、不假装连上：会在界面与日志里如实写出原因。
fn begin_session(
    ui: &AppWindow,
    state: &Rc<RefCell<UiState>>,
    bind: &[String],
    lang: Lang,
    session_code: &str,
) {
    let endpoint = state.borrow().relays.selected_url().to_string();
    if let Err(err) = bridge::start_session(ui, lang, &endpoint, bind, session_code) {
        tracing::warn!("无法开始建连：{err}");
        ui.set_join_error_text(Key::DeviceStartError.format(lang, &[("detail", &err)]).into());
    }
}

/// 当前外观选择。
struct UiState {
    theme_mode: ThemeMode,
    accent_mode: AccentMode,
    hue: f32,
    saturation: f32,
    family: String,
    weight_tier: u16,
    /// 中继列表与当前选中项。
    relays: Relays,
    /// 各中继的探测状态。
    relay_ui: relay::RelayUi,
    /// 轮询中继探测结果的定时器；`UiState` 活多久它就跑多久。
    relay_timer: Option<Rc<slint::Timer>>,
}

// ────────────────────────────────────────────────────── 中继

/// 把中继列表与探测状态推给设置窗口。
fn push_relays(settings: &SettingsWindow, state: &UiState, lang: Lang) {
    relay::push(settings, &state.relays, &state.relay_ui, lang);
}

/// 接上中继相关的界面回调。
fn wire_relay_callbacks(settings: &SettingsWindow, state: &Rc<RefCell<UiState>>, lang: Lang) {
    {
        let state = state.clone();
        let weak = settings.as_weak();
        settings.on_relay_selected(move |index| {
            if index < 0 {
                return;
            }
            let url = {
                let mut state = state.borrow_mut();
                state.relays.select(index as usize);
                save(&current_prefs(&state));
                state.relays.selected_url().to_string()
            };
            state.borrow().relay_ui.check(&url);
            // 选中态要立刻反映在列表上
            if let Some(settings) = weak.upgrade() {
                push_relays(&settings, &state.borrow(), lang);
            }
        });
    }

    {
        let state = state.clone();
        let weak = settings.as_weak();
        settings.on_relay_removed(move |url| {
            let removed = {
                let mut state = state.borrow_mut();
                let index = state
                    .relays
                    .urls()
                    .iter()
                    .position(|existing| existing == url.as_str());
                match index {
                    Some(index) => {
                        state.relays.remove(index);
                        let relays = state.relays.clone();
                        state.relay_ui.resync(&relays);
                        save(&current_prefs(&state));
                        true
                    }
                    None => false,
                }
            };
            if removed {
                tracing::info!(relay = %url, "已删除中继");
            }
            if let Some(settings) = weak.upgrade() {
                push_relays(&settings, &state.borrow(), lang);
            }
        });
    }

    {
        let state = state.clone();
        let weak = settings.as_weak();
        settings.on_relay_add_clicked(move |input| {
            let outcome = {
                let mut state = state.borrow_mut();
                match state.relays.add(input.as_str()) {
                    Ok(_) => {
                        let relays = state.relays.clone();
                        state.relay_ui.resync(&relays);
                        save(&current_prefs(&state));
                        Ok(())
                    }
                    Err(err) => Err(err),
                }
            };
            if let Some(settings) = weak.upgrade() {
                match outcome {
                    Ok(()) => {
                        settings.set_relay_input("".into());
                        settings.set_relay_input_error("".into());
                        // 新地址的核对交给定时器，这里不等网络
                    }
                    Err(err) => settings.set_relay_input_error(err.into()),
                }
                push_relays(&settings, &state.borrow(), lang);
            }
        });
    }

    {
        let weak = settings.as_weak();
        let state = state.clone();
        settings.on_relay_probe_clicked(move || {
            let url = {
                let state = state.borrow();
                state.relays.selected_url().to_string()
            };
            state.borrow().relay_ui.check(&url);
            if let Some(settings) = weak.upgrade() {
                push_relays(&settings, &state.borrow(), lang);
            }
        });
    }
}

/// 定时把中继的探测结果搬到界面上。
///
/// 界面线程不做网络等待：这里只取工作线程回投的结果，顺带把还没核对过的地址排上。
fn start_relay_watch(settings: &SettingsWindow, state: &Rc<RefCell<UiState>>, lang: Lang) {
    const WATCH_INTERVAL: Duration = Duration::from_millis(250);

    let weak = settings.as_weak();
    let watched = state.clone();
    let timer = Rc::new(slint::Timer::default());
    timer.start(slint::TimerMode::Repeated, WATCH_INTERVAL, move || {
        if weak.upgrade().is_none() {
            return;
        }
        let changed = {
            let mut state = watched.borrow_mut();
            let changed = state.relay_ui.apply_updates();
            state.relay_ui.check_pending();
            changed
        };
        if changed {
            if let Some(settings) = weak.upgrade() {
                push_relays(&settings, &watched.borrow(), lang);
            }
        }
    });
    // Timer 一 drop 就停，所以要挂在状态上
    state.borrow_mut().relay_timer = Some(timer);
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
        relays: state.relays.clone(),
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
