//! 本地假中继：`/api/v1/relay` 与 `/healthz`。
//!
//! 给设置页的截图与手工验证用，不参与构建产物。
//! 用法：`fake-relay <端口>[=<上报的短 ID>] ...`，不给 ID 就按真实基址算。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use serde_json::json;
use sha1::{Digest, Sha1};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法：fake-relay 18081[=ABCDEFGHIJ] 18082 ...");
        return;
    }

    let mut handles = Vec::new();
    for spec in args {
        let (port, advertised) = match spec.split_once('=') {
            Some((port, id)) => (port.to_string(), Some(id.to_string())),
            None => (spec.clone(), None),
        };
        let port: u16 = match port.parse() {
            Ok(port) => port,
            Err(_) => {
                eprintln!("端口不合法：{spec}");
                continue;
            }
        };
        let origin = format!("http://127.0.0.1:{port}");
        let id = advertised.unwrap_or_else(|| relay_id(&origin));
        println!(":{port} 上报 ID {id}（真实 {}）", relay_id(&origin));

        handles.push(thread::spawn(move || serve(port, id)));
    }

    for handle in handles {
        let _ = handle.join();
    }
}

fn relay_id(origin: &str) -> String {
    let digest = Sha1::digest(origin.as_bytes());
    base32_upper(&digest)[..10].to_string()
}

fn base32_upper(data: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for byte in data {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 0x1F) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1F) as usize] as char);
    }
    out
}

fn serve(port: u16, id: String) {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!(":{port} 绑定失败：{err}");
            return;
        }
    };

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let id = id.clone();
        thread::spawn(move || handle(stream, port, &id));
    }
}

fn handle(mut stream: TcpStream, port: u16, id: &str) {
    let mut buffer = [0u8; 2048];
    let Ok(read) = stream.read(&mut buffer) else {
        return;
    };
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();

    let body = match path.as_str() {
        "/api/v1/relay" => json!({
            "id": id,
            "version": "0.1.0",
            "protocol_version": 1,
            "signaling": format!("ws://127.0.0.1:{port}/ws/signal"),
            "ice": [
                {"urls": ["turn:relay.example.com:3478?transport=udp"], "username": "1:secrelay", "credential": "abc"},
                {"urls": ["stun:stun.example.com:3478"]}
            ],
            "credential_ttl": 600,
            "realm": "secrelay.relay",
            "turn_configured": true
        })
        .to_string(),
        "/healthz" => json!({
            "status": "ok",
            "version": "0.1.0",
            "protocol_version": 1,
            "turn_configured": true,
            "credential_mode": "ephemeral",
            "active_signaling_connections": 0,
            "active_sessions": 0,
            "peers_over_quota": 0
        })
        .to_string(),
        _ => {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        }
    };

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}
