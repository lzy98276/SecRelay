//! UI ↔ Rust 的桥接层。
//!
//! 这一层是需求分析 §6.1 里"UI 层只做三件事"的落点：**显示帧、显示状态、发出意图**。
//! 它不认识 WebRTC，也不认识编码器；只跟 `secrelay-session` 的公开 API 打交道。
//!
//! # 两条设计约定
//!
//! 1. **界面里没有日志。** 所有诊断信息走 `tracing` 写文件，UI 只在设置页提供一个
//!    打开日志目录的入口。用户视角不应该感觉到日志的存在 —— 这是"无感"的一部分。
//! 2. **消息是独立的数据，不是日志。** 会话消息有自己的列表模型，与诊断信息彻底分开，
//!    否则用户会在"发消息"的地方看到一堆内部事件。
//!
//! # 当前是 M0 演示
//!
//! 会话跑在**进程内回环传输**上（两个 `Session` 直连）。**这不是真实 P2P** ——
//! 设置页与日志里都会明确标注，避免被误读成连通性已经跑通。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use secrelay_i18n::{Key, Lang};
use secrelay_media::{scale_for_width, to_rgba_scaled, CaptureError, RgbaImage};
use secrelay_protocol::{Channel, ControlMessage, DeviceId};
use secrelay_session::{Session, SessionConfig, SessionEvent};
use secrelay_theme::{Palette, ResolvedFont, Rgb};
use secrelay_transport::loopback_pair;
use slint::{ComponentHandle, ModelRc, VecModel, Weak};

use crate::{AppWindow, ChatMessage, SettingsWindow, Strings, Theme};

/// 从 UI 线程投递待发文字给会话线程。
///
/// 用无界通道：UI 回调是同步的，不能 `.await`，所以这里只做投递。
static OUTBOX: Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>> = Mutex::new(None);

/// 当前是否有活跃会话。
static CONNECTED: AtomicBool = AtomicBool::new(false);

/// 本机画面预览是否在跑。
static PREVIEWING: AtomicBool = AtomicBool::new(false);

/// 会话消息。UI 通过重建 `VecModel` 同步。
static MESSAGES: Mutex<Vec<ChatMessage>> = Mutex::new(Vec::new());

/// 预览画面的目标宽度。
///
/// 上限直接决定 CPU 开销：这条路径要在 CPU 上读满整帧、做盒式平均、再上传给 UI。
/// 960 宽大约把 2560x1600 压到 1/9 的像素量。真正的解法是 GPU 纹理零拷贝。
const PREVIEW_TARGET_WIDTH: u32 = 960;

/// 预览刷新上限（约 15fps）。
///
/// 刻意限流：这是 CPU 拷贝路径，实测跑满会吃掉一个多核心（见 `docs/measurements.md`）。
const PREVIEW_MIN_INTERVAL: Duration = Duration::from_millis(66);

/// 会话线程检查发件箱的间隔。
///
/// 用轮询而不是 `select!`：`Session::next_event` 需要 `&mut self`，而发送也需要同一个
/// `Session`，`select!` 的两个分支会抢同一个可变借用。50ms 的轮询对文字消息完全够用。
const OUTBOX_POLL_INTERVAL: Duration = Duration::from_millis(50);

// ────────────────────────────────────────────────────── 语言与文案

/// 把文案注入某一棵树里的 `Strings` 全局。
///
/// Slint 的 global 是**按组件树各自实例化**的，所以主窗口与设置窗口要各注一次。
/// 逻辑集中在这里，避免两处漂移。
pub fn apply_strings(strings: Strings, lang: Lang) {
    let key = |k: Key| -> slint::SharedString { k.text(lang).into() };

    strings.set_app_title(key(Key::AppName));
    strings.set_tagline(key(Key::AppTagline));

    strings.set_group_connect(key(Key::GroupConnect));
    strings.set_group_see(key(Key::GroupSee));
    strings.set_group_transfer(key(Key::GroupTransfer));
    strings.set_group_talk(key(Key::GroupTalk));
    strings.set_group_system(key(Key::GroupSystem));

    strings.set_nav_devices(key(Key::NavDevices));
    strings.set_nav_screen(key(Key::NavScreen));
    strings.set_nav_camera(key(Key::NavCamera));
    strings.set_nav_files(key(Key::NavFiles));
    strings.set_nav_messages(key(Key::NavMessages));
    strings.set_nav_settings(key(Key::NavSettings));

    strings.set_devices_title(key(Key::DevicesTitle));
    strings.set_devices_empty(key(Key::DevicesEmpty));
    strings.set_devices_hint(key(Key::DevicesEmptyHint));
    strings.set_session_peer(key(Key::SessionPeer));
    strings.set_session_channels(key(Key::SessionChannels));
    strings.set_session_capabilities(key(Key::SessionCapabilities));
    strings.set_session_none(key(Key::SessionNone));

    strings.set_action_connect(key(Key::ActionConnect));
    strings.set_action_disconnect(key(Key::ActionDisconnect));
    strings.set_action_send(key(Key::ActionSend));
    strings.set_action_preview_start(key(Key::ActionPreviewStart));
    strings.set_action_preview_stop(key(Key::ActionPreviewStop));
    strings.set_action_open_log_dir(key(Key::ActionOpenLogDir));
    strings.set_action_close(key(Key::ActionClose));

    strings.set_message_placeholder(key(Key::MessagePlaceholder));
    strings.set_messages_empty(key(Key::MessagesEmpty));

    strings.set_preview_title(key(Key::PreviewTitle));
    strings.set_preview_empty(key(Key::PreviewEmpty));
    strings.set_preview_empty_hint(key(Key::PreviewEmptyHint));
    strings.set_preview_disclaimer(key(Key::PreviewDisclaimer));

    strings.set_settings_title(key(Key::SettingsTitle));
    strings.set_settings_appearance(key(Key::SettingsAppearance));
    strings.set_settings_about(key(Key::SettingsAbout));
    strings.set_about_fonts(key(Key::AboutFonts));
    strings.set_about_misans(key(Key::AboutMiSans));
    strings.set_about_fluent_icons(key(Key::AboutFluentIcons));
    strings.set_about_license_hint(key(Key::AboutLicenseHint));
    strings.set_settings_language(key(Key::SettingsLanguage));
    strings.set_settings_language_hint(key(Key::SettingsLanguageHint));
    strings.set_settings_theme(key(Key::SettingsTheme));
    strings.set_settings_theme_hint(key(Key::SettingsThemeHint));
    strings.set_settings_font(key(Key::SettingsFont));
    strings.set_settings_font_hint(key(Key::SettingsFontHint));
    strings.set_settings_diagnostics(key(Key::SettingsDiagnostics));
    strings.set_settings_log_dir_hint(key(Key::SettingsLogDirHint));

    strings.set_theme_follow_system(key(Key::ThemeFollowSystem));
    strings.set_theme_light(key(Key::ThemeLight));
    strings.set_theme_dark(key(Key::ThemeDark));

    strings.set_font_system(key(Key::FontSystem));
    strings.set_font_misans(key(Key::FontMiSans));

    strings.set_account_title(key(Key::AccountTitle));
    strings.set_account_not_logged_in(key(Key::AccountNotLoggedIn));
    strings.set_action_login(key(Key::ActionLogin));

    strings.set_not_available_title(key(Key::NotAvailableTitle));
    strings.set_not_available_hint(key(Key::NotAvailableHint));
}

/// 把文案注入主窗口。
///
/// **加一门语言只需要改这里传入的 `lang`** —— `ui/app.slint` 里没有任何面向用户的字面量。
pub fn apply_language(ui: &AppWindow, lang: Lang) {
    apply_strings(ui.global::<Strings>(), lang);

    // 语言自称永远用它自己的语言显示，不参与翻译。
    ui.set_language_name(lang.native_name().into());
    ui.set_log_dir(log_dir().display().to_string().into());

    // 账号入口：登录功能尚未接入，先如实显示"未登录"。
    ui.set_account_text(Key::AccountNotLoggedIn.text(lang).into());
    ui.set_account_subtitle(Key::ActionLogin.text(lang).into());

    // 未连接时的初始动态值
    if !is_connected() {
        ui.set_state_text(Key::SessionStateIdle.text(lang).into());
        ui.set_peer_text(Key::SessionNone.text(lang).into());
        ui.set_channels_text(Key::SessionNone.text(lang).into());
        ui.set_capabilities_text(Key::SessionNone.text(lang).into());
    }
}

/// 把文案注入设置窗口（独立窗口，有自己的一份 global 实例）。
pub fn apply_language_to_settings(settings: &SettingsWindow, lang: Lang) {
    apply_strings(settings.global::<Strings>(), lang);
    settings.set_language_name(lang.native_name().into());
    settings.set_log_dir(log_dir().display().to_string().into());
}

/// 把界面字体注入某一棵树里的 `Theme` 全局。
///
/// 传入的是**解析后**的结果而不是用户原始选择：字体族名可能与用户选的不同
/// （miSans 的粗体是另一个 family），字重也可能被调成该字体实际存在的档位。
/// 解析逻辑在 `secrelay-theme::fonts`，有测试覆盖。
pub fn apply_fonts(theme: Theme, font: &ResolvedFont) {
    theme.set_ui_font(font.family.as_str().into());
    theme.set_ui_font_bold(font.bold_family.as_str().into());
    theme.set_ui_weight(i32::from(font.weight));
    theme.set_ui_weight_bold(i32::from(font.bold_weight));
}

/// 把调色板注入某一棵树里的 `Theme` 全局。
///
/// 颜色不写在 `.slint` 里，是因为"跟随系统"要在运行时切换，而且强调色要按
/// 浅色/深色分别做可读性调整 —— 那部分逻辑在 `secrelay-theme` 里且有测试。
pub fn apply_palette(theme: Theme, palette: &Palette) {
    let color = |c: Rgb| slint::Color::from_rgb_u8(c.r, c.g, c.b);

    theme.set_bg(color(palette.bg));
    theme.set_nav(color(palette.nav));
    theme.set_surface(color(palette.surface));
    theme.set_surface_hi(color(palette.surface_hi));
    theme.set_stage(color(palette.stage));
    theme.set_border(color(palette.border));
    theme.set_text(color(palette.text));
    theme.set_text_dim(color(palette.text_dim));
    theme.set_text_faint(color(palette.text_faint));
    theme.set_idle(color(palette.idle));
    theme.set_accent(color(palette.accent));
    theme.set_accent_soft(color(palette.accent_soft));
    theme.set_on_accent(color(palette.on_accent));
}

// ────────────────────────────────────────────────────── 日志目录

/// 日志目录。界面不显示日志，只提供打开这个目录的入口。
pub fn log_dir() -> PathBuf {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
    };
    base.unwrap_or_else(|| PathBuf::from("."))
        .join("SecRelay")
        .join("logs")
}

/// 用系统文件管理器打开日志目录。
pub fn open_log_dir() -> std::io::Result<()> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir)?;

    if cfg!(target_os = "windows") {
        std::process::Command::new("explorer").arg(&dir).spawn()?;
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&dir).spawn()?;
    } else {
        std::process::Command::new("xdg-open").arg(&dir).spawn()?;
    }
    Ok(())
}

// ────────────────────────────────────────────────────── 消息

/// 会话消息 → UI 模型。
fn publish_messages(ui: &Weak<AppWindow>) {
    let snapshot = MESSAGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let ui = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            ui.set_messages(ModelRc::new(VecModel::from(snapshot)));
        }
    });
}

/// 追加一条会话消息。
fn push_message(ui: &Weak<AppWindow>, text: String, outgoing: bool) {
    MESSAGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(ChatMessage {
            text: text.into(),
            outgoing,
        });
    publish_messages(ui);
}

fn clear_messages(ui: &Weak<AppWindow>) {
    MESSAGES.lock().unwrap_or_else(|e| e.into_inner()).clear();
    publish_messages(ui);
}

// ────────────────────────────────────────────────────── UI 更新小工具

fn set_state(ui: &Weak<AppWindow>, text: &str) {
    let ui = ui.clone();
    let value: slint::SharedString = text.into();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            ui.set_state_text(value);
        }
    });
}

enum Field {
    Peer,
    Channels,
    Capabilities,
}

fn set_field(ui: &Weak<AppWindow>, field: Field, value: String) {
    let ui = ui.clone();
    let value: slint::SharedString = value.into();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            match field {
                Field::Peer => ui.set_peer_text(value),
                Field::Channels => ui.set_channels_text(value),
                Field::Capabilities => ui.set_capabilities_text(value),
            }
        }
    });
}

fn set_connected_flag(ui: &Weak<AppWindow>, value: bool) {
    let ui = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            ui.set_connected(value);
        }
    });
}

// ────────────────────────────────────────────────────── 频道展示

/// 频道名（用于 UI 展示）。
pub fn channel_text(channel: Channel, lang: Lang) -> &'static str {
    match channel {
        Channel::Media => Key::ChannelMedia.text(lang),
        Channel::File => Key::ChannelFile.text(lang),
        Channel::Control => Key::ChannelControl.text(lang),
    }
}

/// 把一组频道拼成可读的一行。
pub fn channels_text(channels: &[Channel], lang: Lang) -> String {
    if channels.is_empty() {
        return Key::SessionNone.text(lang).to_string();
    }
    channels
        .iter()
        .map(|c| channel_text(*c, lang))
        .collect::<Vec<_>>()
        .join(" / ")
}

// ────────────────────────────────────────────────────── 会话

/// 是否已有活跃会话。
pub fn is_connected() -> bool {
    CONNECTED.load(Ordering::SeqCst)
}

/// 请求断开当前会话。
pub fn request_disconnect() {
    if !CONNECTED.swap(false, Ordering::SeqCst) {
        return;
    }
    // 丢弃发件箱会让会话线程的 `recv` 返回 None，从而退出循环。
    *OUTBOX.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// UI 点击"发送"时调用。
pub fn send_text(text: String) -> bool {
    let guard = OUTBOX.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(tx) => tx.send(text).is_ok(),
        None => false,
    }
}

/// 启动一次 M0 演示会话。
pub fn spawn_demo_session(ui: Weak<AppWindow>, lang: Lang) {
    if CONNECTED.swap(true, Ordering::SeqCst) {
        return; // 已经在会话中
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    *OUTBOX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);

    clear_messages(&ui);
    set_state(&ui, Key::SessionStateConnecting.text(lang));
    tracing::info!(target: "session", "M0 演示会话启动（进程内回环，非真实 P2P）");

    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                tracing::error!(target: "session", "创建 tokio 运行时失败：{err}");
                CONNECTED.store(false, Ordering::SeqCst);
                set_state(&ui, Key::SessionStateClosed.text(lang));
                return;
            }
        };

        runtime.block_on(run_demo_session(ui, lang, rx));
    });
}

async fn run_demo_session(
    ui: Weak<AppWindow>,
    lang: Lang,
    mut outbox: tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    // M0：两个 Session 通过进程内回环直连。真实 P2P 接入后这里换成 ICE 打洞。
    let (transport_a, transport_b) = loopback_pair();
    let mut local = Session::new(
        transport_a,
        SessionConfig::new(DeviceId::new("dev-local-a").expect("固定设备 ID 合法")),
    );
    let mut peer = Session::new(
        transport_b,
        SessionConfig::new(DeviceId::new("dev-local-b").expect("固定设备 ID 合法")),
    );

    let (local_result, peer_result) = tokio::join!(local.connect(), peer.accept());
    let peer_info = match (local_result, peer_result) {
        (Ok(info), Ok(_)) => info,
        (Err(err), _) | (_, Err(err)) => {
            tracing::error!(target: "session", "握手失败：{err}");
            set_state(&ui, Key::SessionStateClosed.text(lang));
            set_connected_flag(&ui, false);
            CONNECTED.store(false, Ordering::SeqCst);
            return;
        }
    };

    tracing::info!(
        target: "session",
        peer = %peer_info.device_id,
        channels = ?peer_info.channels,
        "握手完成"
    );

    set_field(&ui, Field::Peer, peer_info.device_id.to_string());
    set_field(&ui, Field::Channels, channels_text(&peer_info.channels, lang));
    set_field(
        &ui,
        Field::Capabilities,
        if peer_info.capabilities.is_empty() {
            Key::SessionNone.text(lang).to_string()
        } else {
            peer_info.capabilities.join(", ")
        },
    );
    set_state(&ui, Key::SessionStateReady.text(lang));
    set_connected_flag(&ui, true);

    loop {
        if !CONNECTED.load(Ordering::SeqCst) {
            break;
        }

        // 先把待发消息发完
        while let Ok(text) = outbox.try_recv() {
            let text = text.trim().to_string();
            if text.is_empty() {
                continue;
            }
            if let Err(err) = local
                .send_control(ControlMessage::Text { body: text.clone() })
                .await
            {
                tracing::warn!(target: "session", "发送失败：{err}");
                continue;
            }
            push_message(&ui, text.clone(), true);

            // 让对端真正收下这条消息，演示双向链路
            match peer.next_event().await {
                Ok(SessionEvent::Control(ControlMessage::Text { body })) => {
                    push_message(&ui, body, false);
                }
                Ok(SessionEvent::PeerClosed) => {
                    tracing::info!(target: "session", "对端已关闭会话");
                    CONNECTED.store(false, Ordering::SeqCst);
                    break;
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::error!(target: "session", "接收消息失败：{err}");
                    CONNECTED.store(false, Ordering::SeqCst);
                    break;
                }
            }
        }

        // 再看本端有没有事件（心跳由会话层自动处理，不会冒泡到这里）
        match tokio::time::timeout(OUTBOX_POLL_INTERVAL, local.next_event()).await {
            Err(_) => continue, // 没有事件，回到循环再检查发件箱
            Ok(Ok(SessionEvent::PeerClosed)) => {
                tracing::info!(target: "session", "对端已断开");
                break;
            }
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                tracing::error!(target: "session", "会话错误：{err}");
                break;
            }
        }
    }

    let _ = local.close("界面主动断开").await;
    set_state(&ui, Key::SessionStateClosed.text(lang));
    set_connected_flag(&ui, false);
    CONNECTED.store(false, Ordering::SeqCst);
    *OUTBOX.lock().unwrap_or_else(|e| e.into_inner()) = None;
    tracing::info!(target: "session", "会话已结束");
}

// ────────────────────────────────────────────────────── 本机画面预览

/// 预览是否在跑。
pub fn is_previewing() -> bool {
    PREVIEWING.load(Ordering::SeqCst)
}

fn set_previewing(ui: &Weak<AppWindow>, value: bool) {
    let ui = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            ui.set_previewing(value);
            if !value {
                ui.set_preview_stats("".into());
            }
        }
    });
}

/// 停止预览。
pub fn stop_preview() {
    PREVIEWING.store(false, Ordering::SeqCst);
}

/// 启动本机画面预览：把采集到的桌面画面直接回显到界面上。
///
/// # 这是 M0 探针，不是产品功能
///
/// 它**不做编码、不走网络、不是远程画面**，唯一目的是把"采集 → 像素转换 → 界面显示"
/// 这条链路跑通，并量出 CPU 拷贝路径的真实开销。界面上有对应文案说明这一点。
pub fn start_preview(ui: Weak<AppWindow>, lang: Lang) {
    if PREVIEWING.swap(true, Ordering::SeqCst) {
        return; // 已经在预览
    }
    set_previewing(&ui, true);

    std::thread::spawn(move || {
        let mut source = match secrelay_media::open_default_source() {
            Ok(source) => source,
            Err(err) => {
                tracing::error!(target: "preview", "打开采集源失败：{err}");
                PREVIEWING.store(false, Ordering::SeqCst);
                set_previewing(&ui, false);
                return;
            }
        };

        tracing::info!(target: "preview", source = %source.description(), "本机画面预览启动");

        let mut last_push = Instant::now();
        let mut previous: Option<secrelay_media::VideoFrame> = None;
        let mut frames_in_window: u64 = 0;
        let mut window_start = Instant::now();
        let mut fps: f64 = 0.0;

        while PREVIEWING.load(Ordering::SeqCst) {
            let frame = match source.next_frame(Duration::from_millis(200)) {
                Ok(Some(frame)) => frame,
                // 桌面没变化：正常现象，继续等。
                Ok(None) => continue,
                Err(CaptureError::Timeout(_)) => continue,
                Err(err) => {
                    tracing::error!(target: "preview", "采集失败：{err}");
                    break;
                }
            };

            // 限流：CPU 拷贝路径下跑到采集帧率会吃掉一个多核心。
            if last_push.elapsed() < PREVIEW_MIN_INTERVAL {
                continue;
            }
            last_push = Instant::now();

            let change = previous
                .as_ref()
                .map(|prev| prev.diff_ratio(&frame))
                .unwrap_or(0.0);
            previous = Some(frame.clone());

            let scale = scale_for_width(frame.width, PREVIEW_TARGET_WIDTH);
            let image = match to_rgba_scaled(&frame, scale) {
                Ok(image) => image,
                Err(err) => {
                    tracing::error!(target: "preview", "像素转换失败：{err}");
                    break;
                }
            };

            frames_in_window += 1;
            let elapsed = window_start.elapsed();
            if elapsed >= Duration::from_secs(1) {
                fps = frames_in_window as f64 / elapsed.as_secs_f64();
                frames_in_window = 0;
                window_start = Instant::now();
            }

            let stats = format!(
                "{}   {}   {}",
                Key::StatsFps.format(lang, &[("fps", &format!("{fps:.0}"))]),
                Key::StatsSize.format(
                    lang,
                    &[
                        ("width", &image.width.to_string()),
                        ("height", &image.height.to_string()),
                    ],
                ),
                Key::StatsChange.format(lang, &[("ratio", &format!("{:.2}", change * 100.0))]),
            );

            push_preview(&ui, image, stats);
        }

        PREVIEWING.store(false, Ordering::SeqCst);
        set_previewing(&ui, false);
        tracing::info!(target: "preview", "本机画面预览停止");
    });
}

/// 把一帧画面推给 UI。
///
/// 这里是 CPU 路径的关键开销点：`clone_from_slice` 会把 RGBA 缓冲**再拷一份**
/// 交给 Slint，然后由渲染后端上传。这就是为什么预览要限流 + 降采样。
fn push_preview(ui: &Weak<AppWindow>, image: RgbaImage, stats: String) {
    let ui = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                &image.data,
                image.width,
                image.height,
            );
            ui.set_preview(slint::Image::from_rgba8(buffer));
            ui.set_preview_stats(stats.into());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 频道名跟随语言() {
        assert_eq!(channel_text(Channel::Media, Lang::ZhHans), "媒体");
        assert_eq!(channel_text(Channel::File, Lang::ZhHans), "文件");
        assert_eq!(channel_text(Channel::Control, Lang::ZhHans), "控制");
    }

    #[test]
    fn 频道列表拼接() {
        let all = [Channel::Media, Channel::File, Channel::Control];
        assert_eq!(channels_text(&all, Lang::ZhHans), "媒体 / 文件 / 控制");
        assert_eq!(channels_text(&[Channel::Control], Lang::ZhHans), "控制");
        assert_eq!(channels_text(&[], Lang::ZhHans), "无");
    }

    #[test]
    fn 未连接时发消息失败而不是崩溃() {
        *OUTBOX.lock().unwrap() = None;
        CONNECTED.store(false, Ordering::SeqCst);
        assert!(!send_text("测试".into()));
    }

    #[test]
    fn 日志目录在用户数据目录下且不以项目目录结尾() {
        let dir = log_dir();
        let text = dir.to_string_lossy().to_lowercase();
        assert!(text.contains("secrelay"), "实际：{text}");
        assert!(text.ends_with("logs"), "实际：{text}");
    }
}
