//! UI ↔ Rust 的桥接层。
//!
//! 这一层是需求分析 §6.1 里"UI 层只做三件事"的落点：**显示帧、显示状态、发出意图**。
//! 它不认识 WebRTC，也不认识编码器；只跟 `secrelay-session` 的公开 API 打交道。
//!
//! # 当前是 M0 演示
//!
//! 会话跑在**进程内回环传输**上（两个 `Session` 直连），用来把 UI 与核心真正接起来。
//! **这不是真实 P2P** —— 界面上会明确标注这一点，避免被误读成连通性已经跑通。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use secrelay_i18n::{Key, Lang};
use secrelay_protocol::{Channel, ControlMessage, DeviceId};
use secrelay_session::{Session, SessionConfig, SessionEvent};
use secrelay_transport::loopback_pair;
use slint::Weak;

use crate::AppWindow;

/// 从 UI 线程投递待发文字给会话线程。
///
/// 用无界通道：UI 回调是同步的，不能 `.await`，所以这里只做投递。
static OUTBOX: Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>> = Mutex::new(None);

/// 当前是否有活跃会话。用原子量避免 UI 回调里加锁。
static CONNECTED: AtomicBool = AtomicBool::new(false);

/// 会话线程检查发件箱的间隔。
///
/// 之所以用轮询而不是 `select!`：`Session::next_event` 需要 `&mut self`，
/// 而发送也需要同一个 `Session`，`select!` 的两个分支会抢同一个可变借用。
/// 50ms 的轮询对文字消息完全够用，且实现简单不会出错。
const OUTBOX_POLL_INTERVAL: Duration = Duration::from_millis(50);

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

/// 是否已有活跃会话。
pub fn is_connected() -> bool {
    CONNECTED.load(Ordering::SeqCst)
}

/// 把所有静态文案从 i18n 注入 UI。
///
/// **加一门语言只需要改这里传入的 `lang`** —— `ui/app.slint` 里没有任何面向用户的字面量。
pub fn apply_language(ui: &AppWindow, lang: Lang) {
    let key = |k: Key| -> slint::SharedString { k.text(lang).into() };

    ui.set_app_title(key(Key::AppName));
    ui.set_tagline(key(Key::AppTagline));
    ui.set_nav_devices(key(Key::NavDevices));
    ui.set_nav_session(key(Key::NavSession));
    ui.set_nav_settings(key(Key::NavSettings));
    ui.set_devices_title(key(Key::DevicesTitle));
    ui.set_devices_empty(key(Key::DevicesEmpty));
    ui.set_devices_hint(key(Key::DevicesEmptyHint));
    ui.set_session_title(key(Key::SessionTitle));
    ui.set_peer_label(key(Key::SessionPeer));
    ui.set_channels_label(key(Key::SessionChannels));
    ui.set_capabilities_label(key(Key::SessionCapabilities));
    ui.set_log_title(key(Key::LogTitle));
    ui.set_connect_label(key(Key::ActionConnect));
    ui.set_disconnect_label(key(Key::ActionDisconnect));
    ui.set_send_label(key(Key::ActionSend));
    ui.set_message_placeholder(key(Key::MessagePlaceholder));
    ui.set_settings_title(key(Key::SettingsTitle));
    ui.set_language_label(key(Key::SettingsLanguage));
    ui.set_language_hint(key(Key::SettingsLanguageHint));

    // 语言自称永远用它自己的语言显示，不参与翻译。
    ui.set_language_name(lang.native_name().into());

    // 未连接时的初始动态值
    if !is_connected() {
        ui.set_state_text(key(Key::SessionStateIdle));
        ui.set_peer_text(key(Key::SessionNone));
        ui.set_channels_text(key(Key::SessionNone));
        ui.set_capabilities_text(key(Key::SessionNone));
    }
}

// ────────────────────────────────────────────────────── UI 更新小工具

/// 向 UI 追加一行日志。
fn append_log(ui: &Weak<AppWindow>, line: String) {
    let ui = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            let mut text = ui.get_log_text().to_string();
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&line);
            ui.set_log_text(text.into());
        }
    });
}

fn set_state(ui: &Weak<AppWindow>, text: &str) {
    let ui = ui.clone();
    let value: slint::SharedString = text.into();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            ui.set_state_text(value);
        }
    });
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

enum Field {
    Peer,
    Channels,
    Capabilities,
}

// ────────────────────────────────────────────────────── 会话

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

    set_state(&ui, Key::SessionStateConnecting.text(lang));
    append_log(&ui, Key::LogDemoMode.text(lang).to_string());
    append_log(&ui, Key::LogHandshakeStarted.text(lang).to_string());

    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                append_log(
                    &ui,
                    Key::ErrorGeneric.format(lang, &[("detail", &err.to_string())]),
                );
                CONNECTED.store(false, Ordering::SeqCst);
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
            append_log(
                &ui,
                Key::ErrorGeneric.format(lang, &[("detail", &err.to_string())]),
            );
            set_state(&ui, Key::SessionStateClosed.text(lang));
            set_connected_flag(&ui, false);
            CONNECTED.store(false, Ordering::SeqCst);
            return;
        }
    };

    append_log(
        &ui,
        Key::LogHandshakeDone.format(lang, &[("peer", peer_info.device_id.as_str())]),
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
                .send_control(ControlMessage::Text {
                    body: text.clone(),
                })
                .await
            {
                append_log(
                    &ui,
                    Key::LogSendFailed.format(lang, &[("detail", &err.to_string())]),
                );
                continue;
            }
            append_log(
                &ui,
                Key::MessageSent.format(lang, &[("text", &text)]),
            );

            // 让对端真正收下这条消息，演示双向链路
            match peer.next_event().await {
                Ok(SessionEvent::Control(ControlMessage::Text { body })) => {
                    append_log(&ui, Key::MessageReceived.format(lang, &[("text", &body)]));
                }
                Ok(SessionEvent::PeerClosed) => {
                    append_log(&ui, Key::LogPeerClosed.text(lang).to_string());
                    CONNECTED.store(false, Ordering::SeqCst);
                    break;
                }
                Ok(_) => {}
                Err(err) => {
                    append_log(
                        &ui,
                        Key::ErrorGeneric.format(lang, &[("detail", &err.to_string())]),
                    );
                    CONNECTED.store(false, Ordering::SeqCst);
                    break;
                }
            }
        }

        // 再看本端有没有事件（心跳由会话层自动处理，不会冒泡到这里）
        match tokio::time::timeout(OUTBOX_POLL_INTERVAL, local.next_event()).await {
            Err(_) => continue, // 没有事件，回到循环再检查发件箱
            Ok(Ok(SessionEvent::PeerClosed)) => {
                append_log(&ui, Key::LogPeerClosed.text(lang).to_string());
                break;
            }
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                append_log(
                    &ui,
                    Key::ErrorGeneric.format(lang, &[("detail", &err.to_string())]),
                );
                break;
            }
        }
    }

    let _ = local.close("界面主动断开").await;
    set_state(&ui, Key::SessionStateClosed.text(lang));
    set_connected_flag(&ui, false);
    append_log(&ui, Key::LogDisconnected.text(lang).to_string());
    CONNECTED.store(false, Ordering::SeqCst);
    *OUTBOX.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
        // 确保 OUTBOX 为空时 send_text 返回 false。
        *OUTBOX.lock().unwrap() = None;
        CONNECTED.store(false, Ordering::SeqCst);
        assert!(!send_text("测试".into()));
    }
}
