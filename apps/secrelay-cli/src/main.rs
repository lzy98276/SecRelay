//! SecRelay 命令行工具。
//!
//! 当前是 M0 阶段的骨架：**传输层只有回环实现**，真实的 ICE 打洞 / TURN 中继 /
//! QUIC 文件通道尚未接入。`selftest` 验证的是协议、能力协商与会话模型本身 ——
//! 也就是需求分析 §3 那句"一条加密通道 + N 种频道"到底成不成立。

use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use secrelay_media::{ScreenSource, SyntheticScreenSource, VideoFrame};
use secrelay_protocol::{Channel, ControlMessage, DeviceId};
use secrelay_session::{capabilities, Session, SessionConfig, SessionEvent};
use secrelay_transport::loopback_pair;

const HELP: &str = r#"SecRelay —— 跨设备连接，让看、传、说归于一处

用法：
  secrelay selftest [媒体帧数]   在回环传输上跑通「一条通道 + 三个频道」（默认 32 帧）
  secrelay capture [选项]        屏幕采集探针：量化帧率、抖动与「相邻帧变化比例」
  secrelay channels              打印频道模型
  secrelay version               打印版本
  secrelay help                  显示本帮助

capture 选项：
  --synthetic          用合成画面源（不采集真实屏幕，任何平台都能跑）
  --seconds <秒>       采集时长，默认 5
  --size <宽x高>       合成源的分辨率，默认 1920x1080
  --fps <帧率>         合成源的目标帧率，默认 60

说明：当前是 M0 骨架，传输层只有回环实现。selftest 验证协议、协商与会话模型，
      不代表真实的 P2P 连通性已经跑通。
"#;

/// 采集探针里每帧的等待上限。
const CAPTURE_FRAME_TIMEOUT: Duration = Duration::from_millis(250);

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
        "capture" => capture(&args[1..]),
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

// ─────────────────────────────────────────────────────────── 采集探针

/// 采集探针：回答三个 M0 必须回答的问题。
///
/// 1. **能不能稳定采到目标帧率**（帧间隔分布）？
/// 2. **桌面静止时有多少帧其实是空转**（没有新画面）？
/// 3. **相邻帧的变化区域有多大** —— 这直接决定脏矩形差分的收益上限。
///    需求分析 §6.3 说它的收益（5~50 倍）比"换编解码器"（1.5~2 倍）大一个数量级，
///    这里就是把这个说法量化成我们自己环境下的数字。
fn capture(args: &[String]) -> Result<()> {
    let mut seconds = 5.0f64;
    let mut synthetic = false;
    let mut size = (1920u32, 1080u32);
    let mut fps = 60u32;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--synthetic" => {
                synthetic = true;
                index += 1;
            }
            "--seconds" => {
                seconds = args
                    .get(index + 1)
                    .context("--seconds 需要一个数值")?
                    .parse()
                    .context("--seconds 必须是数字")?;
                index += 2;
            }
            "--size" => {
                size = parse_size(args.get(index + 1).context("--size 需要 宽x高")?)?;
                index += 2;
            }
            "--fps" => {
                fps = args
                    .get(index + 1)
                    .context("--fps 需要一个数值")?
                    .parse()
                    .context("--fps 必须是数字")?;
                index += 2;
            }
            other => bail!("capture 的未知参数：{other}"),
        }
    }

    if seconds <= 0.0 {
        bail!("--seconds 必须大于 0");
    }

    let mut source = build_source(synthetic, size, fps)?;

    println!("SecRelay 采集探针");
    println!("{}", "=".repeat(64));
    println!("采集源：{}", source.description());
    println!("时长：{seconds}s");
    println!();

    let started = Instant::now();
    let deadline = started + Duration::from_secs_f64(seconds);

    let mut frames: u64 = 0;
    let mut idle: u64 = 0; // 没有新画面的轮次
    let mut bytes: u64 = 0;
    let mut intervals: Vec<Duration> = Vec::new();
    let mut diff_sum = 0.0f64;
    let mut diff_samples: u64 = 0;
    let mut diff_min = f32::MAX;
    let mut diff_max = 0.0f32;
    let mut previous: Option<VideoFrame> = None;
    let mut last_capture: Option<Instant> = None;
    let mut platform_errors: Vec<String> = Vec::new();

    while Instant::now() < deadline {
        match source.next_frame(CAPTURE_FRAME_TIMEOUT) {
            Ok(Some(frame)) => {
                let now = Instant::now();
                if let Some(prev_time) = last_capture {
                    intervals.push(now - prev_time);
                }
                last_capture = Some(now);

                if let Some(prev) = &previous {
                    let ratio = prev.diff_ratio(&frame);
                    diff_sum += f64::from(ratio);
                    diff_samples += 1;
                    diff_min = diff_min.min(ratio);
                    diff_max = diff_max.max(ratio);
                }

                bytes += frame.data.len() as u64;
                frames += 1;
                previous = Some(frame);
            }
            Ok(None) => {
                // 桌面自上次采集以来没有变化 —— 这是重要信号，不是错误。
                idle += 1;
            }
            Err(secrelay_media::CaptureError::Timeout(_)) => {
                idle += 1;
            }
            Err(err) => {
                // 采集错误不立即中断：先把已经采到的数据统计出来。
                let text = err.to_string();
                if !platform_errors.contains(&text) {
                    platform_errors.push(text);
                }
                if frames == 0 {
                    // 一帧都没采到，继续等没有意义。
                    bail!("采集失败，且尚未取到任何帧：{err}");
                }
                break;
            }
        }
    }

    let elapsed = started.elapsed();
    println!("结果");
    println!("   有效帧：{frames}    空转（画面无变化）：{idle}");
    println!(
        "   实测帧率：{:.1} fps（目标 {} fps）",
        frames as f64 / elapsed.as_secs_f64(),
        if synthetic { fps.to_string() } else { "跟随屏幕刷新".into() }
    );

    if !intervals.is_empty() {
        let mut sorted = intervals.clone();
        sorted.sort();
        let pick = |q: f64| -> f64 {
            let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
            sorted[idx].as_secs_f64() * 1000.0
        };
        println!(
            "   帧间隔：p50 {:.2}ms   p95 {:.2}ms   max {:.2}ms",
            pick(0.50),
            pick(0.95),
            pick(1.0)
        );
    }

    if let Some(frame) = &previous {
        let frame_bytes = frame.data.len();
        println!(
            "   单帧未压缩：{}x{} BGRA = {:.2} MB",
            frame.width,
            frame.height,
            frame_bytes as f64 / (1024.0 * 1024.0)
        );
        println!(
            "   累计原始数据：{:.2} GB（{:.1} Mbps 未压缩）",
            bytes as f64 / (1024.0 * 1024.0 * 1024.0),
            (bytes as f64 * 8.0) / elapsed.as_secs_f64() / 1_000_000.0
        );
    }

    if diff_samples > 0 {
        let average = diff_sum / diff_samples as f64;
        println!();
        println!("脏矩形差分的依据");
        println!(
            "   相邻帧变化比例：平均 {:.2}%   最小 {:.2}%   最大 {:.2}%",
            average * 100.0,
            f64::from(diff_min) * 100.0,
            f64::from(diff_max) * 100.0
        );
        if average > 0.0 {
            println!(
                "   理论收益上限：约 {:.0}×（只传变化区域 vs 整帧）",
                1.0 / average
            );
        }
        println!("   注：这是变化像素的**比例**，还不是脏矩形面积 —— 真正实现时按矩形合并会更大。");
    } else if frames > 0 {
        println!("\n   样本不足，无法给出相邻帧变化比例（多采一会儿）。");
    }

    if !platform_errors.is_empty() {
        println!();
        println!("采集期间出现的错误：");
        for err in &platform_errors {
            println!("   ! {err}");
        }
    }

    println!("\n合计 {frames} 帧 / {:.2}s", elapsed.as_secs_f64());
    Ok(())
}

fn build_source(
    synthetic: bool,
    size: (u32, u32),
    fps: u32,
) -> Result<Box<dyn ScreenSource>> {
    if synthetic {
        return Ok(Box::new(SyntheticScreenSource::new(size.0, size.1, fps)));
    }

    #[cfg(target_os = "windows")]
    {
        let source = secrelay_media::windows_dxgi::DxgiScreenSource::new(0)
            .context("打开 DXGI 桌面复制失败；可加 --synthetic 先验证其余链路")?;
        Ok(Box::new(source))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (size, fps);
        bail!("该平台的采集后端尚未实现（目前只有 Windows DXGI 与合成源），请加 --synthetic")
    }
}

fn parse_size(raw: &str) -> Result<(u32, u32)> {
    let (width, height) = raw
        .split_once(['x', 'X'])
        .context("分辨率格式应为 宽x高，例如 1920x1080")?;
    let width: u32 = width.trim().parse().context("宽度必须是整数")?;
    let height: u32 = height.trim().parse().context("高度必须是整数")?;
    if width == 0 || height == 0 {
        bail!("分辨率必须大于 0");
    }
    Ok((width, height))
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
