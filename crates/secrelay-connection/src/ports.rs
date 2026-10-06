//! 本机端口分配。只在测试里用来开假中继。

use std::net::TcpListener;

/// 要一个当前空闲的 TCP 端口。
///
/// 先绑再放开，因此端口在返回与调用方重新绑定之间可能被别人抢走 ——
/// 测试里这个窗口极小，比固定端口号稳。
pub fn free_tcp_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("应当能绑到本机随机端口");
    listener.local_addr().expect("应当能读到本地地址").port()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 分出来的端口能再绑上() {
        let port = free_tcp_port();
        assert_ne!(port, 0);
        TcpListener::bind(("127.0.0.1", port)).expect("分出来的端口应当立刻可绑");
    }
}
