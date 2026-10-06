//! 本机两块网卡之间跑一次真实建连，用来验证候选收集与打洞。
//!
//! SDP 在进程内直接交换，不经过信令。
//!
//! 用法：
//!
//! ```text
//! cargo run -p secrelay-transport --example lan-connect -- stun:stun.example.com:3478
//! ```
//!
//! `BIND_A` / `BIND_B`（可选）指定两侧绑定的本机地址，默认 `192.168.0.100:0` 与
//! `192.168.0.102:0`。

use std::time::Instant;

use secrelay_protocol::{Channel, Frame};
use secrelay_transport::ice::{IceConfig, IceServerConfig};
use secrelay_transport::{Transport, WebRtcTransport};

#[tokio::main]
async fn main() {
    let mut servers = Vec::new();
    for raw in std::env::args().skip(1) {
        servers.push(IceServerConfig {
            urls: vec![raw],
            username: String::new(),
            credential: String::new(),
        });
    }
    let config = IceConfig::from_servers(servers);

    let a_bind = std::env::var("BIND_A").unwrap_or_else(|_| "192.168.0.100:0".to_string());
    let b_bind = std::env::var("BIND_B").unwrap_or_else(|_| "192.168.0.102:0".to_string());

    let started = Instant::now();
    let a = WebRtcTransport::new_with_bind(&config, vec![a_bind.clone()])
        .await
        .unwrap();
    let b = WebRtcTransport::new_with_bind(&config, vec![b_bind.clone()])
        .await
        .unwrap();
    println!("建两条：{:?}", started.elapsed());

    let started = Instant::now();
    let offer = a.create_offer().await.unwrap();
    println!("a 收集候选：{:?}", started.elapsed());
    for line in offer.lines().filter(|l| l.contains("candidate:")) {
        println!("  A {line}");
    }
    let started = Instant::now();
    let answer = b.accept_offer(offer).await.unwrap();
    println!("b 收集候选：{:?}", started.elapsed());
    for line in answer.lines().filter(|l| l.contains("candidate:")) {
        println!("  B {line}");
    }
    a.accept_answer(answer).await.unwrap();

    let started = Instant::now();
    let (ra, rb) = tokio::join!(
        a.wait_connected(std::time::Duration::from_secs(20)),
        b.wait_connected(std::time::Duration::from_secs(20)),
    );
    println!("两端就绪：{:?} / {:?}，耗时 {:?}", ra.is_ok(), rb.is_ok(), started.elapsed());
    println!("A 候选对：{:?}", a.selected_pair());
    println!("B 候选对：{:?}", b.selected_pair());
    println!("是否经中继：A={} B={}", a.is_relayed(), b.is_relayed());

    a.send(Frame::raw(Channel::Media, vec![1, 2, 3]).unwrap())
        .await
        .unwrap();
    let frame = b.recv().await.unwrap().expect("B 应当收到帧");
    println!("B 收到：{:?}", frame.as_raw());
    a.close().await.unwrap();
}
