//! UI ↔ Rust 的桥接层。
//!
//! UI 层只做三件事：**显示帧、显示状态、发出意图**。它不认识 WebRTC，也不认识编码器；
//! 建连与收发文字都交给 `secrelay-connection` 与 `secrelay-session`。
//!
//! # 两条设计约定
//!
//! 1. **界面里没有日志。** 所有诊断信息走 `tracing` 写文件，UI 只在设置页提供一个
//!    打开日志目录的入口。
//! 2. **消息是独立的数据，不是日志。** 会话消息有自己的列表模型，与诊断信息彻底分开。
//!
//! # 线程模型
//!
//! Slint 跑在主线程，建连是异步的，所以：
//!
//! - 建连与收发都在**一条工作线程**上跑（线程内自建 current-thread 运行时）；
//! - 界面线程只做三件事：把用户意图投进通道、置取消标志、按定时器取状态；
//! - 状态用 `std::sync::mpsc` 回投，由 Slint 定时器搬到界面上（与字体枚举、中继探测同一套做法）。
//!
//! 连接状态**如实显示**：只有 `secrelay-connection` 真的返回了连接才显示"已连接"，
//! 并且区分"直连"与"经服务端转发"。

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use secrelay_connection::{ConnectOptions, Connector, Progress};
use secrelay_i18n::{Key, Lang};
use secrelay_protocol::{Channel, ControlMessage, DeviceId};
use secrelay_session::{PeerInfo, SessionEvent};
use secrelay_theme::ResolvedFont;
use slint::{ComponentHandle, ModelRc, VecModel, Weak};

use crate::{AppWindow, ChatMessage, SettingsWindow, Strings, UiFont};

/// 从 UI 线程投递待发文字给会话线程。
///
/// 用无界通道：UI 回调是同步的，不能 `.await`，所以这里只做投递。
static OUTBOX: Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>> = Mutex::new(None);

/// 当前是否有会话在跑（含"正在建连"）。
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// 会话消息。UI 通过重建 `VecModel` 同步。
static MESSAGES: Mutex<Vec<ChatMessage>> = Mutex::new(Vec::new());

/// 建连会话的代号。
///
/// 断开之后线程可能还在收尾，它只能清理**自己那一代**留下的全局引用，
/// 否则会把用户刚发起的新一轮建连踩掉。
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// 建连的取消入口，留给界面线程。
static CONNECTOR: Mutex<Option<(u64, Arc<Connector>)>> = Mutex::new(None);

/// 当前会话的只读摘要，供界面线程同步读取。
static CURRENT: Mutex<Option<Current>> = Mutex::new(None);

/// 一条已建好的会话里界面用得上的那几项。
///
/// 界面线程要的是几个值，而 `Connection` 必须留在会话线程上，所以建好之后摘一份出来。
#[derive(Clone)]
struct Current {
    session_code: String,
    peer: PeerInfo,
    relayed: bool,
    /// 这一轮是"直连尝试"还是"中继回退"。
    via_relay_candidates: bool,
}

/// 取当前会话摘要。
fn current() -> Option<Current> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// 状态回投给界面的那一端。
static REPORTER: Mutex<Option<Sender<Update>>> = Mutex::new(None);

/// 工作线程回投给界面的状态。
enum Update {
    /// 进度变化。
    Progress(LinkState),
    /// 会话结束（正常断开或失败）。
    Stopped { reason: String },
}

/// 界面上要显示的一档连接状态。
#[derive(Debug, Clone, PartialEq)]
enum LinkState {
    /// 正在建连；`detail` 是本轮的说明。
    Connecting { detail: String },
    /// 已经建好会话，正等对端进来（发起方才有这一档）。
    WaitingPeer { session_code: String },
    /// 会话已就绪。
    Ready {
        session_code: String,
        peer: String,
        channels: Vec<Channel>,
        capabilities: Vec<String>,
        relayed: bool,
        /// 这一轮是"直连优先"还是"中继回退"。
        via_relay_candidates: bool,
    },
}

/// 收发循环检查发件箱与取消标志的间隔。
///
/// 用轮询而不是 `select!`：`next_event` 与 `send_control` 要同一个 `Session`，
/// 轮询一次只借一处，代码也不必为借用问题绕路。50ms 对文字消息完全够用。
const OUTBOX_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 界面取状态的间隔。
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(120);

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
    strings.set_session_code_label(key(Key::SessionCodeLabel));
    strings.set_session_code_input_hint(key(Key::SessionCodeInputHint));
    strings.set_device_link_label(key(Key::DeviceLinkLabel));
    strings.set_device_link_direct(key(Key::DeviceLinkDirect));
    strings.set_device_link_relayed(key(Key::DeviceLinkRelayed));
    strings.set_device_join_error(key(Key::DeviceJoinError));
    strings.set_device_start_error(key(Key::DeviceStartError));
    strings.set_device_session_failed(key(Key::DeviceSessionFailed));

    strings.set_action_connect(key(Key::ActionConnect));
    strings.set_action_disconnect(key(Key::ActionDisconnect));
    strings.set_action_join(key(Key::ActionJoin));
    strings.set_action_send(key(Key::ActionSend));
    strings.set_action_open_log_dir(key(Key::ActionOpenLogDir));

    strings.set_message_placeholder(key(Key::MessagePlaceholder));
    strings.set_messages_empty(key(Key::MessagesEmpty));

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
    strings.set_settings_accent(key(Key::SettingsAccent));
    strings.set_accent_follow_system(key(Key::AccentFollowSystem));
    strings.set_accent_custom(key(Key::AccentCustom));
    strings.set_settings_hue(key(Key::SettingsHue));
    strings.set_settings_saturation(key(Key::SettingsSaturation));
    strings.set_settings_font(key(Key::SettingsFont));
    strings.set_settings_font_hint(key(Key::SettingsFontHint));
    strings.set_settings_diagnostics(key(Key::SettingsDiagnostics));
    strings.set_settings_log_dir_hint(key(Key::SettingsLogDirHint));

    strings.set_settings_relay(key(Key::SettingsRelay));
    strings.set_relay_hint(key(Key::RelayHint));
    strings.set_relay_list(key(Key::RelayList));
    strings.set_relay_default(key(Key::RelayDefault));
    strings.set_relay_add(key(Key::RelayAdd));
    strings.set_relay_add_placeholder(key(Key::RelayAddPlaceholder));
    strings.set_relay_add_hint(key(Key::RelayAddHint));
    strings.set_relay_remove(key(Key::RelayRemove));
    strings.set_relay_selected(key(Key::RelaySelected));
    strings.set_relay_id_local(key(Key::RelayIdLocal));
    strings.set_relay_id_advertised(key(Key::RelayIdAdvertised));
    strings.set_relay_id_match(key(Key::RelayIdMatch));
    strings.set_relay_id_mismatch(key(Key::RelayIdMismatch));
    strings.set_relay_id_mismatch_hint(key(Key::RelayIdMismatchHint));
    strings.set_relay_id_malformed(key(Key::RelayIdMalformed));
    strings.set_relay_probe(key(Key::RelayProbe));
    strings.set_relay_probing(key(Key::RelayProbing));
    strings.set_relay_reachable(key(Key::RelayReachable));
    strings.set_relay_unreachable(key(Key::RelayUnreachable));
    strings.set_relay_turn_ready(key(Key::RelayTurnReady));
    strings.set_relay_turn_missing(key(Key::RelayTurnMissing));
    strings.set_relay_not_checked(key(Key::RelayNotChecked));
    strings.set_relay_proto_mismatch(key(Key::RelayProtoMismatch));
    strings.set_relay_no_active(key(Key::RelayNoActive));
    strings.set_relay_signaling(key(Key::RelaySignaling));
    strings.set_relay_current(key(Key::RelayCurrent));

    strings.set_theme_follow_system(key(Key::ThemeFollowSystem));
    strings.set_theme_light(key(Key::ThemeLight));
    strings.set_theme_dark(key(Key::ThemeDark));
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
        ui.set_session_code_text("".into());
        ui.set_link_text("".into());
        ui.set_has_link(false);
        ui.set_join_error_text("".into());
        ui.set_busy(false);
    }
}

/// 把文案注入设置窗口（独立窗口，有自己的一份 global 实例）。
pub fn apply_language_to_settings(settings: &SettingsWindow, lang: Lang) {
    apply_strings(settings.global::<Strings>(), lang);
    settings.set_language_name(lang.native_name().into());
    settings.set_log_dir(log_dir().display().to_string().into());
}

/// 把界面字体注入某一棵树里的 `UiFont` 全局。
///
/// 传入的是解析后的结果：字体族名与字重可能都跟用户选的原始值不同
/// （miSans 的 Light/Medium/Demibold 各自是独立字体族）。
pub fn apply_fonts(ui_font: UiFont, font: &ResolvedFont) {
    ui_font.set_family(font.family.as_str().into());
    ui_font.set_weight(i32::from(font.weight));
}

// ────────────────────────────────────────────────────── 状态回投

/// 在工作线程当前这一代上报一次状态。
fn report(state: LinkState) {
    let sender = REPORTER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(sender) = sender.as_ref() {
        let _ = sender.send(Update::Progress(state));
    }
}

fn report_stopped(sender: &Sender<Update>, reason: String) {
    let _ = sender.send(Update::Stopped { reason });
}

/// 界面这边收状态的通道，与 `REPORTER` 是一对。
static STATUS_UPDATES: Mutex<Option<Receiver<Update>>> = Mutex::new(None);

/// 在界面线程上装好状态定时器。必须在 `ui.run()` 之前调用。
pub fn install_status_pump(ui: Weak<AppWindow>) {
    let (sender, receiver) = channel::<Update>();
    *STATUS_UPDATES.lock().unwrap_or_else(|e| e.into_inner()) = Some(receiver);
    *REPORTER.lock().unwrap_or_else(|e| e.into_inner()) = Some(sender);

    let timer = Rc::new(slint::Timer::default());
    let running = timer.clone();
    timer.start(
        slint::TimerMode::Repeated,
        STATUS_POLL_INTERVAL,
        move || poll_status(&ui, &running),
    );
}

/// 取走工作线程回投的状态并刷新界面。
///
/// `running` 是定时器自己的句柄：界面没了就停掉，不然定时器会一直空转。
fn poll_status(ui: &Weak<AppWindow>, running: &Rc<slint::Timer>) {
    let Some(ui) = ui.upgrade() else {
        running.stop();
        return;
    };
    let lang = Lang::default();

    loop {
        let update = {
            let guard = STATUS_UPDATES.lock().unwrap_or_else(|e| e.into_inner());
            match guard.as_ref() {
                Some(receiver) => receiver.try_recv(),
                None => return,
            }
        };
        match update {
            Err(std::sync::mpsc::TryRecvError::Empty)
            | Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            Ok(Update::Progress(state)) => apply_state(&ui, state, lang),
            Ok(Update::Stopped { reason }) => {
                if reason.is_empty() {
                    ui.set_state_text(Key::DeviceStateDisconnected.text(lang).into());
                } else {
                    ui.set_state_text(
                        Key::DeviceSessionFailed
                            .format(lang, &[("reason", &reason)])
                            .into(),
                    );
                }
                ui.set_connected(false);
                ui.set_busy(false);
                ui.set_has_link(false);
                ui.set_link_text("".into());
                ui.set_peer_text(Key::SessionNone.text(lang).into());
                ui.set_channels_text(Key::SessionNone.text(lang).into());
                ui.set_capabilities_text(Key::SessionNone.text(lang).into());
            }
        }
    }
}

/// 把一档状态刷到界面上。
fn apply_state(ui: &AppWindow, state: LinkState, lang: Lang) {
    let none = slint::SharedString::from(Key::SessionNone.text(lang));

    match state {
        LinkState::Connecting { detail } => {
            ui.set_state_text(Key::SessionStateConnecting.text(lang).into());
            ui.set_connected(false);
            ui.set_busy(true);
            ui.set_has_link(false);
            ui.set_link_text(detail.into());
            ui.set_peer_text(none);
        }
        LinkState::WaitingPeer { session_code } => {
            ui.set_state_text(Key::DeviceStateWaitingPeer.text(lang).into());
            ui.set_session_code_text(session_code.into());
            ui.set_connected(false);
            ui.set_busy(true);
        }
        LinkState::Ready {
            session_code,
            peer,
            channels,
            capabilities,
            relayed,
            via_relay_candidates,
        } => {
            ui.set_state_text(
                if relayed {
                    Key::DeviceStateConnectedRelayed.text(lang)
                } else {
                    Key::DeviceStateConnectedDirect.text(lang)
                }
                .into(),
            );
            ui.set_session_code_text(session_code.into());
            ui.set_peer_text(peer.into());
            ui.set_channels_text(channels_text(&channels, lang).into());
            ui.set_capabilities_text(
                if capabilities.is_empty() {
                    none
                } else {
                    capabilities.join(", ").into()
                },
            );
            ui.set_link_text(
                if relayed {
                    Key::DeviceLinkRelayed.text(lang)
                } else {
                    Key::DeviceLinkDirect.text(lang)
                }
                .into(),
            );
            ui.set_has_link(true);
            ui.set_connected(true);
            ui.set_busy(false);
            tracing::info!(
                target: "session",
                relayed,
                via_relay_candidates,
                "界面已显示连接结果"
            );
        }
    }
}

/// 进度 → 界面状态。
fn state_of(progress: &Progress) -> Option<LinkState> {
    let state = match progress {
        Progress::Discovering => LinkState::Connecting {
            detail: "从中继取配置".to_string(),
        },
        Progress::Discovered { .. } => LinkState::Connecting {
            detail: "已取到配置".to_string(),
        },
        Progress::Signaling => LinkState::Connecting {
            detail: "正在连信令".to_string(),
        },
        Progress::SessionReady { session_code, role } => LinkState::WaitingPeer {
            session_code: format!("{session_code}（{}）", role.label()),
        },
        Progress::PeerJoined { .. } => LinkState::Connecting {
            detail: "对端已进入会话".to_string(),
        },
        Progress::Connecting { kind } => LinkState::Connecting {
            detail: kind.label().to_string(),
        },
        Progress::FallbackToRelay { reason } => LinkState::Connecting {
            detail: format!("直连失败（{reason}），改用中继候选"),
        },
        // 已连接与失败都有专门的回投路径，这里不重复处理
        Progress::Connected { .. } | Progress::Failed { .. } | Progress::Cancelled => return None,
    };
    Some(state)
}

/// 日志里记一条进度；状态本身由 `report` 出去。
fn log_progress(progress: &Progress) {
    match progress {
        Progress::SessionReady { session_code, role } => tracing::info!(
            target: "session",
            session_code = %session_code,
            role = role.label(),
            "会话码已就绪"
        ),
        Progress::PeerJoined { peer_id } => tracing::info!(
            target: "session",
            peer = %peer_id,
            "对端已进入会话"
        ),
        Progress::Connected { kind, relayed } => tracing::info!(
            target: "session",
            attempt = kind.label(),
            relayed,
            "建连完成"
        ),
        Progress::Failed { reason } => {
            tracing::warn!(target: "session", "建连失败：{reason}")
        }
        Progress::FallbackToRelay { reason } => {
            tracing::warn!(target: "session", "直连失败，回退中继：{reason}")
        }
        other => tracing::debug!(target: "session", "进度：{other:?}"),
    }
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

// ────────────────────────────────────────────────────── 建连与收发

/// 是否已有活跃会话（含正在建连）。
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

/// 是否已经连上。
pub fn is_connected() -> bool {
    is_active() && current().is_some()
}

/// 会话码的形态校验：八位十六进制。与建连层的判定一致，界面先挡一道好给提示。
pub fn session_code_valid(code: &str) -> bool {
    let code = code.trim();
    code.len() == 8 && code.chars().all(|c| c.is_ascii_hexdigit())
}

/// 界面上的按钮该显示什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectAction {
    /// 本机生成会话码，等对端加入。
    Offer,
    /// 加入对端给出的会话码。
    Join,
    /// 正在建连或已连接，按钮是"断开"。
    Disconnect,
}

/// 按当前会话码输入框的内容决定按钮语义。
pub fn connect_action(session_code: &str) -> ConnectAction {
    if is_active() {
        return ConnectAction::Disconnect;
    }
    if session_code.trim().is_empty() {
        ConnectAction::Offer
    } else {
        ConnectAction::Join
    }
}

/// 断开当前会话（含"正在建连"时取消）。
pub fn request_disconnect() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }

    // 丢掉发件箱会让收发循环的 `recv` 返回 None，从而退出循环。
    *OUTBOX.lock().unwrap_or_else(|e| e.into_inner()) = None;

    // 会话线程会自己收尾；这里只负责让它停下来。
    let connector = CONNECTOR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .map(|(_, connector)| connector);
    if let Some(connector) = connector {
        connector.cancel();
    }
}

/// UI 点击"发送"时调用。
pub fn send_text(text: String) -> bool {
    let guard = OUTBOX.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(tx) => tx.send(text).is_ok(),
        None => false,
    }
}

/// 发起一次建连。
///
/// `session_code` 为空时本机新建会话并等对端加入（发起方），非空时加入该会话（应答方）。
/// `endpoint` 是用户选中的中继基址。
pub fn start_session(
    ui: &AppWindow,
    lang: Lang,
    endpoint: &str,
    bind: &[String],
    session_code: &str,
) -> Result<(), String> {
    if ACTIVE.swap(true, Ordering::SeqCst) {
        return Err("已有会话在跑".to_string());
    }

    let reporter = REPORTER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or_else(|| "状态通道还没装好".to_string())?;

    let endpoint = endpoint.trim().to_string();
    if endpoint.is_empty() {
        ACTIVE.store(false, Ordering::SeqCst);
        return Err("还没选中继".to_string());
    }

    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let peer_id = local_peer_id();
    let options = {
        let options = ConnectOptions::new(endpoint.clone(), peer_id.clone())
            .with_device_id(DeviceId::new(peer_id.clone()).expect("本机 ID 合法"))
            .with_bind(bind.to_vec());
        if session_code.trim().is_empty() {
            options.as_offerer()
        } else {
            options.joining(session_code.trim())
        }
    };

    let connector = Arc::new(Connector::new(options));
    *CONNECTOR.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, connector.clone()));

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    *OUTBOX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = None;

    clear_messages(&ui.as_weak());
    ui.set_join_error_text("".into());

    tracing::info!(
        target: "session",
        endpoint = %endpoint,
        session_code = %session_code.trim(),
        peer_id = %peer_id,
        bind = ?bind,
        "开始建连"
    );

    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                ACTIVE.store(false, Ordering::SeqCst);
                release(generation);
                report_stopped(&reporter, format!("创建 tokio 运行时失败：{err}"));
                return;
            }
        };
        runtime.block_on(run_session(weak, lang, generation, connector, rx, reporter));
    });

    Ok(())
}

/// 只清掉本代留下的全局引用。
fn release(generation: u64) {
    let guard = CONNECTOR.lock().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref().map(|(id, _)| *id) == Some(generation) {
        drop(guard);
        *CONNECTOR.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// 一条会话线程的完整生命周期：建连 → 收发 → 收尾。
async fn run_session(
    ui: Weak<AppWindow>,
    lang: Lang,
    generation: u64,
    connector: Arc<Connector>,
    mut outbox: tokio::sync::mpsc::UnboundedReceiver<String>,
    reporter: Sender<Update>,
) {
    // 建连与进度上报并行：发起方会在等对端加入时卡住，会话码必须在那之前送到界面上。
    let mut feed = connector.subscribe();
    let connect = connector.connect();
    let watch = async {
        while let Some(progress) = feed.changed().await {
            log_progress(&progress);
            if let Some(state) = state_of(&progress) {
                report(state);
            }
            if matches!(
                progress,
                Progress::Connected { .. } | Progress::Failed { .. } | Progress::Cancelled
            ) {
                break;
            }
        }
    };
    let (result, ()) = tokio::join!(connect, watch);

    let mut connection = match result {
        Ok(connection) => connection,
        Err(error) => {
            tracing::warn!(target: "session", "建连没有成功：{error}");
            *OUTBOX.lock().unwrap_or_else(|e| e.into_inner()) = None;
            ACTIVE.store(false, Ordering::SeqCst);
            release(generation);
            if !error.is_cancelled() {
                report_stopped(&reporter, error.reason());
            }
            return;
        }
    };

    // 会话已建好：先摘一份界面要的值，再把连接挪进全局，界面线程才点的动"断开"
    let Some(peer) = connection.peer().cloned() else {
        let _ = connection.close("握手没有留下对端信息").await;
        *OUTBOX.lock().unwrap_or_else(|e| e.into_inner()) = None;
        ACTIVE.store(false, Ordering::SeqCst);
        release(generation);
        report_stopped(&reporter, "握手没有留下对端信息".to_string());
        return;
    };
    let summary = Current {
        session_code: connection.session_code().to_string(),
        peer,
        relayed: connection.is_relayed(),
        via_relay_candidates: connection.connected().kind
            == secrelay_connection::AttemptKind::RelayOnly,
    };
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(summary.clone());

    tracing::info!(
        target: "session",
        session_code = %summary.session_code,
        peer = %summary.peer.device_id,
        relayed = summary.relayed,
        via_relay_candidates = summary.via_relay_candidates,
        "会话已就绪，进入收发"
    );

    report(LinkState::Ready {
        session_code: summary.session_code.clone(),
        peer: summary.peer.device_id.to_string(),
        channels: summary.peer.channels.clone(),
        capabilities: summary.peer.capabilities.clone(),
        relayed: summary.relayed,
        via_relay_candidates: summary.via_relay_candidates,
    });

    // ── 收发循环
    let mut stop_reason = String::new();
    loop {
        if !ACTIVE.load(Ordering::SeqCst) {
            break;
        }

        // 先把待发消息发完
        while let Ok(text) = outbox.try_recv() {
            let text = text.trim().to_string();
            if text.is_empty() {
                continue;
            }
            if let Err(err) = connection
                .session_mut()
                .send_control(ControlMessage::Text { body: text.clone() })
                .await
            {
                tracing::warn!(target: "session", "发送失败：{err}");
                let text = Key::LogSendFailed.format(lang, &[("detail", &err.to_string())]);
                push_message(&ui, text, false);
                continue;
            }
            tracing::info!(target: "session", bytes = text.len(), "已发送一条文字消息");
            push_message(&ui, text, true);
        }

        match tokio::time::timeout(
            OUTBOX_POLL_INTERVAL,
            connection.session_mut().next_event(),
        )
        .await
        {
            Err(_) => continue,
            Ok(Ok(SessionEvent::Control(ControlMessage::Text { body }))) => {
                tracing::info!(target: "session", bytes = body.len(), "收到一条文字消息");
                push_message(&ui, body, false);
            }
            Ok(Ok(SessionEvent::PeerClosed)) => {
                tracing::info!(target: "session", "对端已关闭会话");
                break;
            }
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                stop_reason = err.to_string();
                tracing::error!(target: "session", "会话错误：{err}");
                break;
            }
        }
    }

    let _ = connection.close("界面主动断开").await;

    // 清掉摘要，否则界面会把一次已经结束的会话当成活跃会话
    {
        let mut guard = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
    *OUTBOX.lock().unwrap_or_else(|e| e.into_inner()) = None;
    ACTIVE.store(false, Ordering::SeqCst);
    release(generation);

    report_stopped(&reporter, stop_reason);
    tracing::info!(target: "session", "会话已结束");
}

/// 本机在信令里的匿名身份。只在本次进程内有意义。
fn local_peer_id() -> String {
    format!("secrelay-desktop-{}", std::process::id())
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
        ACTIVE.store(false, Ordering::SeqCst);
        assert!(!send_text("测试".into()));
    }

    #[test]
    fn 日志目录在用户数据目录下且不以项目目录结尾() {
        let dir = log_dir();
        let text = dir.to_string_lossy().to_lowercase();
        assert!(text.contains("secrelay"), "实际：{text}");
        assert!(text.ends_with("logs"), "实际：{text}");
    }

    #[test]
    fn 会话码校验与建连层一致() {
        assert!(session_code_valid("ab12cd34"));
        assert!(session_code_valid("  AB12CD34 "));
        for bad in ["", "abc", "abcd1234z", "1234567890"] {
            assert!(!session_code_valid(bad), "{bad} 应当被拒");
        }
    }

    #[test]
    fn 按钮语义跟着输入框走() {
        ACTIVE.store(false, Ordering::SeqCst);
        assert_eq!(connect_action(""), ConnectAction::Offer);
        assert_eq!(connect_action("   "), ConnectAction::Offer);
        assert_eq!(connect_action("ab12cd34"), ConnectAction::Join);
    }

    #[test]
    fn 直连与经服务端转发是两档不同的文案() {
        let direct = Key::DeviceStateConnectedDirect.text(Lang::ZhHans);
        let relayed = Key::DeviceStateConnectedRelayed.text(Lang::ZhHans);
        assert_ne!(direct, relayed);
        assert!(direct.contains("直连"), "{direct}");
        assert!(relayed.contains("转发"), "{relayed}");
    }

    #[test]
    fn 失败状态带上原因() {
        let text = Key::DeviceSessionFailed.format(Lang::ZhHans, &[("reason", "连接信令失败")]);
        assert!(text.contains("连接信令失败"), "{text}");
    }

    #[test]
    fn 连上之前不显示已连接() {
        ACTIVE.store(false, Ordering::SeqCst);
        *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = None;
        assert!(!is_connected());

        ACTIVE.store(true, Ordering::SeqCst);
        assert!(is_active(), "建连中也算活跃，按钮要能取消");
        assert!(!is_connected(), "还没连上就不能显示已连接");

        ACTIVE.store(false, Ordering::SeqCst);
    }
}
