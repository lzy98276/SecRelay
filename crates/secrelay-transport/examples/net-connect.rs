//! 两个独立进程通过 WebRTC DataChannel 互连，交换一条消息。
//!
//! SDP 不走信令服务器，而是两个文件：
//! 发起方把 offer 写进 `out`，应答方读 `in`、把 answer 写回 `out`。
//!
//! 用法：
//!
//! ```text
//! # 终端 1（发起方）
//! cargo run -p secrelay-transport --example net-connect -- offer \
//!     --out %TEMP%\secrelay-offer.txt --in %TEMP%\secrelay-answer.txt
//!
//! # 终端 2（应答方）
//! cargo run -p secrelay-transport --example net-connect -- answer \
//!     --in %TEMP%\secrelay-offer.txt --out %TEMP%\secrelay-answer.txt
//! ```
//!
//! 环境变量 `SECRELAY_ICE_JSON`（可选）可以是中继发现接口返回的 JSON
//! （含 `ice` 数组），用来启用 STUN/TURN；`SECRELAY_ICE_STUN_ONLY=1` 时只用中继候选。
//! `SECRELAY_BIND`（可选）指定本机 UDP 绑定地址，默认 `0.0.0.0:0`。

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use secrelay_protocol::{ControlMessage, DeviceId, Frame, PROTOCOL_VERSION};
use secrelay_transport::ice::{parse_bind_addr, IceConfig};
use secrelay_transport::{Transport, WebRtcTransport, DEFAULT_CONNECT_TIMEOUT};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn main() -> Result<(), BoxError> {
    let args: Vec<String> = std::env::args().collect();
    let (role, options) = parse_args(&args)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run(role, options))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Offer,
    Answer,
}

#[derive(Debug, Clone)]
struct Options {
    in_path: PathBuf,
    out_path: PathBuf,
}

async fn run(role: Role, options: Options) -> Result<(), BoxError> {
    let config = ice_config_from_env()?;
    let bind = std::env::var("SECRELAY_BIND").unwrap_or_else(|_| "0.0.0.0:0".to_string());
    parse_bind_addr(&bind).map_err(|e| format!("SECRELAY_BIND 非法：{e}"))?;

    println!("== SecRelay WebRTC 传输示例 ==");
    println!("角色：{role:?}");
    println!("本机绑定：{bind}");
    println!(
        "ICE 服务器：{} 台，策略：{:?}",
        config.ice.len(),
        config.transport_policy()
    );
    for server in &config.ice {
        println!("  - {}（凭据{}）", server.urls.join(","), if server.credential.is_empty() { "无" } else { "有" });
    }

    let transport = WebRtcTransport::new_with_bind(&config, vec![bind]).await?;

    match role {
        Role::Offer => {
            let offer = transport.create_offer().await?;
            write_file(&options.out_path, &offer)?;
            println!("offer 已写入 {}", options.out_path.display());
            println!("等待对端 answer：{}", options.in_path.display());

            let answer = wait_for_peer(&options.in_path).await?;
            transport.accept_answer(answer).await?;
        }
        Role::Answer => {
            println!("等待对端 offer：{}", options.in_path.display());
            let offer = wait_for_peer(&options.in_path).await?;
            let answer = transport.accept_offer(offer).await?;
            write_file(&options.out_path, &answer)?;
            println!("answer 已写入 {}", options.out_path.display());
        }
    }

    transport.wait_connected(DEFAULT_CONNECT_TIMEOUT).await?;
    let pair = transport
        .selected_pair()
        .ok_or("连上了但读不到候选对")?;
    println!("已连通：本端 {}（{}）↔ 对端 {}（{}）", pair.local, pair.local_type, pair.remote, pair.remote_type);
    println!("是否经中继：{}", transport.is_relayed());

    match role {
        Role::Offer => {
            let hello = Frame::control(ControlMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                device_id: DeviceId::new("demo-offer")?,
                capabilities: vec!["text".into()],
            });
            transport.send(hello).await?;
            println!("已发送一条 Hello 帧");

            let reply = transport
                .recv()
                .await?
                .ok_or("对端在回消息之前关闭了连接")?;
            println!("收到对端回复：{:?}", reply.as_control());
            transport.close().await?;
        }
        Role::Answer => {
            let frame = transport
                .recv()
                .await?
                .ok_or("对端在发消息之前关闭了连接")?;
            println!("收到对端消息：{:?}", frame.as_control());

            let reply = Frame::control(ControlMessage::HelloAck {
                protocol_version: PROTOCOL_VERSION,
                device_id: DeviceId::new("demo-answer")?,
                capabilities: vec!["text".into()],
            });
            transport.send(reply).await?;
            println!("已回复一条 HelloAck 帧");
            tokio::time::sleep(Duration::from_millis(500)).await;
            transport.close().await?;
        }
    }

    println!("统计：{:?}", transport.stats());
    println!("完成");
    Ok(())
}

fn parse_args(args: &[String]) -> Result<(Role, Options), BoxError> {
    let role = match args.get(1).map(|s| s.as_str()) {
        Some("offer") => Role::Offer,
        Some("answer") => Role::Answer,
        other => {
            return Err(format!("第一个参数必须是 offer 或 answer，收到 {other:?}").into());
        }
    };

    let mut in_path = None;
    let mut out_path = None;
    let mut index = 2;
    while index < args.len() {
        let key = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{key} 缺少参数值"))?;
        match key {
            "--in" => in_path = Some(PathBuf::from(value)),
            "--out" => out_path = Some(PathBuf::from(value)),
            other => return Err(format!("未知参数 {other}").into()),
        }
        index += 2;
    }

    Ok((
        role,
        Options {
            in_path: in_path.ok_or("缺少 --in")?,
            out_path: out_path.ok_or("缺少 --out")?,
        },
    ))
}

fn ice_config_from_env() -> Result<IceConfig, BoxError> {
    let mut config = match std::env::var("SECRELAY_ICE_JSON") {
        Ok(json) if !json.trim().is_empty() => serde_json::from_str(&json)?,
        _ => IceConfig::host_only(),
    };
    config.stun_only = matches!(
        std::env::var("SECRELAY_ICE_STUN_ONLY").as_deref(),
        Ok("1") | Ok("true")
    );
    Ok(config)
}

/// 轮询等对端把文件写出来；先写临时文件再改名，避免读到写了一半的内容。
async fn wait_for_peer(path: &PathBuf) -> Result<String, BoxError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        match std::fs::read_to_string(path) {
            Ok(content) if !content.trim().is_empty() => return Ok(content),
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("等待 {} 超时", path.display()).into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn write_file(path: &PathBuf, content: &str) -> Result<(), BoxError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(rest: &[&str]) -> Vec<String> {
        let mut all = vec!["net-connect".to_string()];
        all.extend(rest.iter().map(|s| s.to_string()));
        all
    }

    #[test]
    fn 解析角色与文件路径() {
        let (role, options) = parse_args(&args(&[
            "offer", "--in", "a.txt", "--out", "b.txt",
        ]))
        .unwrap();
        assert_eq!(role, Role::Offer);
        assert_eq!(options.in_path, PathBuf::from("a.txt"));
        assert_eq!(options.out_path, PathBuf::from("b.txt"));

        let (role, _) = parse_args(&args(&["answer", "--in", "a", "--out", "b"])).unwrap();
        assert_eq!(role, Role::Answer);
    }

    #[test]
    fn 缺少参数时报错() {
        assert!(parse_args(&args(&["offer"])).is_err());
        assert!(parse_args(&args(&["offer", "--in", "a"])).is_err());
        assert!(parse_args(&args(&["both", "--in", "a", "--out", "b"])).is_err());
        assert!(parse_args(&args(&["offer", "--in", "a", "--out"])).is_err());
    }
}
