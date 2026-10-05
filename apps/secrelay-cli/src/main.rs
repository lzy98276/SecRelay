//! SecRelay 命令行工具。
//!
//! 当前是 M0 阶段的骨架：**传输层只有回环实现**，真实的 ICE 打洞 / TURN 中继 /
//! QUIC 文件通道尚未接入。`selftest` 验证的是协议、能力协商与会话模型本身 ——
//! 也就是需求分析 §3 那句"一条加密通道 + N 种频道"到底成不成立。

use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use secrelay_protocol::{Channel, ControlMessage, DeviceId};
use secrelay_session::{capabilities, Session, SessionConfig, SessionEvent};
use secrelay_transport::loopback_pair;

const HELP: &str = r#"SecRelay —— 跨设备连接，让看、传、说归于一处

用法：
  secrelay selftest [媒体帧数]   在回环传输上跑通「一条通道 + 三个频道」（默认 32 帧）
  secrelay channels              打印频道模型
  secrelay version               打印版本
  secrelay help                  显示本帮助

说明：当前是 M0 骨架，传输层只有回环实现。selftest 验证协议、协商与会话模型，
      不代表真实的 P2P 连通性已经跑通。
"#;

/// 自检用的文件大小（256 KiB）。
const SELFTEST_FILE_BYTES: usize = 256 * 1024;
/// 自检用的文件分块大小（16 KiB）。
const SELFTEST_CHUNK_BYTES: usize = 16 * 1024;

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");

    let outcome = match command {
        "selftest" => {
            let frames = args
                .get(1)
                .map(|raw| raw.parse::<usize>())
                .transpose()
                .unwrap_or_else(|_| {
                    eprintln!("媒体帧数必须是正整数，已使用默认值");
                    Some(32)
                })
                .unwrap_or(32);
            selftest(frames).await
        }
        "channels" => {
            print_channels();
            Ok(())
        }
        "version" | "--version" | "-V" => {
            println!("secrelay {}（协议版本 v{}）", env!("CARGO_PKG_VERSION"), secrelay_protocol::PROTOCOL_VERSION);
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!("{HELP}");
            Ok(())
        }
        other => {
            eprintln!("未知命令：{other}\n");
            println!("{HELP}");
            return ExitCode::from(2);
        }
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("\n✗ 失败：{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn print_channels() {
    println!("SecRelay 频道模型（需求分析 §3）\n");
    println!("{:<10} {:<8} {:<10} 用途", "频道", "编号", "可丢帧");
    println!("{}", "-".repeat(64));
    let purposes = [
        "屏幕 / 摄像头 / 麦克风：低延迟优先",
        "文件 / 剪贴板大对象：必达、可分块续传",
        "文字消息 / 桌面提示 / 会话控制：必达、有序",
    ];
    for (channel, purpose) in Channel::ALL.iter().zip(purposes) {
        println!(
            "{:<10} {:<8} {:<10} {}",
            format!("{channel:?}"),
            channel.as_u8(),
            if channel.is_lossy() { "是" } else { "否" },
            purpose
        );
    }
    println!(
        "\n默认能力清单：{}",
        capabilities::default_all().join(", ")
    );
    println!("注：Control 频道永远存在 —— 没有它连「关闭会话」都发不出去。");
}

async fn selftest(media_frames: usize) -> Result<()> {
    let started = Instant::now();

    println!("SecRelay M0 自检：在回环传输上验证「一条通道 + 三个频道」");
    println!("{}", "=".repeat(64));

    // ── 1. 建立连接（当前是回环；将来这里换成 ICE 打洞 / 中继）
    let (transport_a, transport_b) = loopback_pair();
    let mut a = Session::new(transport_a, SessionConfig::new(DeviceId::new("dev-alpha")?));
    let mut b = Session::new(transport_b, SessionConfig::new(DeviceId::new("dev-beta")?));

    let (hello_a, hello_b) = tokio::join!(a.connect(), b.accept());
    let info_a = hello_a?;
    let info_b = hello_b?;

    println!("✓ 握手完成");
    println!("    A 看到的对端：{}", info_a.device_id);
    println!("    B 看到的对端：{}", info_b.device_id);
    println!("    协商频道：{:?}", info_a.channels);
    println!("    共同能力：{}", info_a.capabilities.join(", "));

    if info_a.channels != info_b.channels {
        bail!("双方协商出的频道不一致：{:?} vs {:?}", info_a.channels, info_b.channels);
    }
    if !info_a.channels.contains(&Channel::Control) {
        bail!("控制频道必须始终可用");
    }

    // ── 2. 媒体频道：发 N 帧，校验顺序与内容
    for seq in 0..media_frames {
        let mut payload = Vec::with_capacity(1024);
        payload.extend_from_slice(&(seq as u64).to_be_bytes());
        payload.extend(std::iter::repeat_n((seq % 251) as u8, 1016));
        a.send_raw(Channel::Media, payload).await?;

        match b.next_event().await? {
            SessionEvent::Frame(frame) => {
                if frame.channel != Channel::Media {
                    bail!("期望媒体帧，收到 {:?}", frame.channel);
                }
                let raw = frame.as_raw().expect("媒体帧应当是原始字节");
                let got = u64::from_be_bytes(raw[..8].try_into().expect("序号应当是 8 字节"));
                if got != seq as u64 {
                    bail!("媒体帧乱序：期望序号 {seq}，收到 {got}");
                }
            }
            other => bail!("期望媒体帧，收到 {other:?}"),
        }
    }
    println!("✓ 媒体频道：{media_frames} 帧可达且保序（每帧 1024 字节）");

    // ── 3. 文件频道：分块发送 256 KiB，校验重组结果
    let source: Vec<u8> = (0..SELFTEST_FILE_BYTES).map(|i| (i % 253) as u8).collect();
    let chunk_count = SELFTEST_FILE_BYTES.div_ceil(SELFTEST_CHUNK_BYTES);

    let sender = {
        let source = source.clone();
        let session = &a;
        async move {
            for (index, chunk) in source.chunks(SELFTEST_CHUNK_BYTES).enumerate() {
                let mut payload = Vec::with_capacity(4 + chunk.len());
                payload.extend_from_slice(&(index as u32).to_be_bytes());
                payload.extend_from_slice(chunk);
                session.send_raw(Channel::File, payload).await?;
            }
            Ok::<usize, anyhow::Error>(chunk_count)
        }
    };

    let receiver = async {
        let mut assembled = Vec::with_capacity(SELFTEST_FILE_BYTES);
        let mut expected_index: u32 = 0;
        while assembled.len() < SELFTEST_FILE_BYTES {
            match b.next_event().await? {
                SessionEvent::Frame(frame) => {
                    if frame.channel != Channel::File {
                        bail!("期望文件块，收到 {:?}", frame.channel);
                    }
                    let raw = frame.as_raw().expect("文件块应当是原始字节");
                    let index = u32::from_be_bytes(raw[..4].try_into().expect("块序号 4 字节"));
                    if index != expected_index {
                        bail!("文件块乱序：期望 {expected_index}，收到 {index}");
                    }
                    expected_index += 1;
                    assembled.extend_from_slice(&raw[4..]);
                }
                other => bail!("期望文件块，收到 {other:?}"),
            }
        }
        Ok::<Vec<u8>, anyhow::Error>(assembled)
    };

    let (sent_chunks, assembled) = tokio::try_join!(sender, receiver)?;
    if assembled != source {
        bail!("文件内容校验失败：重组后的字节与源不一致");
    }
    println!(
        "✓ 文件频道：{sent_chunks} 块 / {} KiB 完整重组，校验和 0x{:016x}",
        SELFTEST_FILE_BYTES / 1024,
        fnv1a(&assembled)
    );

    // ── 4. 控制频道：文字消息
    let message = "跨设备连接，让看、传、说归于一处";
    a.send_control(ControlMessage::Text {
        body: message.to_string(),
    })
    .await?;
    match b.next_event().await? {
        SessionEvent::Control(ControlMessage::Text { body }) => {
            if body != message {
                bail!("文字消息内容不一致：{body}");
            }
            println!("✓ 控制频道：文字消息送达 —— 「{body}」");
        }
        other => bail!("期望文字消息，收到 {other:?}"),
    }

    // ── 5. 心跳：会话层自动应答，不应作为事件上抛
    a.ping(1).await?;
    expect_no_event(&mut b, "B 侧消化 Ping 并自动应答").await?;
    expect_no_event(&mut a, "A 侧消化 Pong").await?;
    println!("✓ 心跳：由会话层自动应答，未上抛给上层");

    // ── 6. 有序关闭
    let stats_a = a.stats();
    let stats_b = b.stats();
    a.close("自检结束").await?;
    match b.next_event().await? {
        SessionEvent::PeerClosed => println!("✓ 关闭：对端收到 PeerClosed，状态机收敛"),
        other => bail!("期望 PeerClosed，收到 {other:?}"),
    }

    // ── 7. 报告
    println!("{}", "=".repeat(64));
    println!("统计（也验证了中继占比的埋点口径）");
    println!(
        "    A → B  帧 {:<6} 字节 {:<9}   中继：{}",
        stats_a.frames_sent,
        stats_a.bytes_sent,
        yes_no(a.is_relayed())
    );
    println!(
        "    B → A  帧 {:<6} 字节 {:<9}   中继：{}",
        stats_b.frames_sent,
        stats_b.bytes_sent,
        yes_no(b.is_relayed())
    );
    println!(
        "    单向字节一致：{}",
        yes_no(stats_a.bytes_sent == stats_b.bytes_received)
    );
    println!("\n耗时 {:?}", started.elapsed());
    println!("\n注意：这次跑的是回环传输，只证明协议与会话模型成立。");
    println!("下一步要验证的是真实的 P2P 连通性（ICE 打洞成功率）与屏幕采集管线。");
    Ok(())
}

/// 断言在给定时间内**没有**事件上抛（用于验证心跳被内部消化）。
async fn expect_no_event(session: &mut Session, what: &str) -> Result<()> {
    match tokio::time::timeout(Duration::from_millis(300), session.next_event()).await {
        Err(_) => Ok(()),
        Ok(Ok(SessionEvent::PeerClosed)) => bail!("{what}：连接意外关闭"),
        Ok(Ok(other)) => bail!("{what}：不应上抛事件，却收到 {other:?}"),
        Ok(Err(err)) => Err(err.into()),
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "是"
    } else {
        "否"
    }
}

/// FNV-1a 64 位校验和。刻意不引第三方依赖 —— 这里只需要一个稳定的内容指纹。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
