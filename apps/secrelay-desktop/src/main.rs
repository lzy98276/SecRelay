//! SecRelay 桌面客户端。
//!
//! # 分层
//!
//! ```text
//! ui/app.slint   界面（不含任何面向用户的字面量，全部由 i18n 注入）
//! src/bridge.rs  UI ↔ 核心 的桥接：只显示状态、发出意图
//! secrelay-*     核心（协议 / 传输 / 会话 / 采集），不依赖任何 UI 框架
//! ```
//!
//! 这条边界是需求分析 §6.1 的核心建议：**UI 框架是可替换的一层壳**。
//! 所以这个 crate 是唯一允许依赖 Slint 的地方，`crates/*` 里没有一处 UI 依赖。

slint::include_modules!();

mod bridge;

use std::time::Duration;

use secrelay_i18n::{Key, Lang};
use slint::ComponentHandle;

fn main() -> anyhow::Result<()> {
    init_tracing();

    // 语言协商：目前只有简体中文，所以直接用默认值。
    // 将来在这里接"系统语言 / 用户配置"，Lang::negotiate 已经准备好了。
    let lang = Lang::default();

    let ui = AppWindow::new()?;
    bridge::apply_language(&ui, lang);

    // 连接 / 断开
    let weak = ui.as_weak();
    ui.on_connect_clicked(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if bridge::is_connected() {
            // 断开：状态与日志由会话线程收尾，避免这里和后台线程抢着写界面。
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

    // --demo：自动连接并发两条消息，方便截图、录屏与给别人演示。
    if std::env::args().any(|arg| arg == "--demo") {
        schedule_demo(&ui, lang);
    }

    ui.run()?;
    Ok(())
}

/// `--demo` 的自动播放脚本。
fn schedule_demo(ui: &AppWindow, lang: Lang) {
    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_millis(700), move || {
        if let Some(ui) = weak.upgrade() {
            ui.invoke_connect_clicked();
        }
    });

    // 握手需要一点时间，稍后再发消息；两条消息能看出「已发送 / 已收到」都通了。
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

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
