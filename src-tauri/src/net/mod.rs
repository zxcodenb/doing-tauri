pub mod client;
pub mod dto;

#[cfg(test)]
pub mod test_server {
    //! 极简脚本化 HTTP 服务器：解析单请求（HTTP/1.1），返回预设响应。
    //! 用于在单元测试中验证客户端/同步引擎的完整网络行为（真实 TCP）。

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone)]
    #[allow(dead_code)] // path/bearer 供后续用例做请求断言保留。
    pub struct Req {
        pub method: String,
        pub path: String,
        pub bearer: Option<String>,
        pub body: String,
    }

    pub struct Resp {
        pub status: u16,
        pub body: String,
    }

    pub type Handler = dyn Fn(&Req) -> Resp + Send + Sync + 'static;

    pub struct Scripted {
        pub url: String,
        pub stop: Arc<AtomicBool>,
        pub log: Arc<Mutex<Vec<Req>>>,
        handler: Arc<Mutex<Box<Handler>>>,
        addr: std::net::SocketAddr,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl Scripted {
        pub fn spawn(initial: Box<Handler>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("绑定测试端口失败");
            let addr = listener.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let log = Arc::new(Mutex::new(Vec::new()));
            let handler: Arc<Mutex<Box<Handler>>> = Arc::new(Mutex::new(initial));
            let stop2 = stop.clone();
            let log2 = log.clone();
            let handler2 = handler.clone();
            let handle = std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop2.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut stream) = stream else { continue };
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let mut header_done = false;
                    let mut content_length = 0usize;
                    // 先读头，再按 Content-Length 读 body。
                    while let Ok(n) = stream.read(&mut chunk) {
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if !header_done {
                            if let Some(pos) = find_sub(&buf, b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                                content_length = head
                                    .lines()
                                    .find_map(|l| {
                                        l.to_ascii_lowercase()
                                            .strip_prefix("content-length:")
                                            .and_then(|v| v.trim().parse().ok())
                                    })
                                    .unwrap_or(0);
                                header_done = true;
                            }
                        }
                        if header_done && buf.len() >= head_len(&buf) + content_length {
                            break;
                        }
                    }
                    let head_end = find_sub(&buf, b"\r\n\r\n").unwrap_or(buf.len());
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let mut lines = head.lines();
                    let request_line = lines.next().unwrap_or_default();
                    let mut parts = request_line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let path = parts.next().unwrap_or("").to_string();
                    let bearer = lines
                        .find_map(|l| {
                            let (name, value) = l.split_once(':')?;
                            name.trim()
                                .eq_ignore_ascii_case("authorization")
                                .then(|| value.trim().to_string())
                        })
                        .and_then(|v| v.strip_prefix("Bearer ").map(str::to_string));
                    let body =
                        String::from_utf8_lossy(&buf[head_end + 4..]).to_string();
                    let req = Req {
                        method,
                        path,
                        bearer,
                        body,
                    };
                    log2.lock().unwrap().push(req.clone());
                    let resp = (handler2.lock().unwrap())(&req);
                    if resp.status == 0 {
                        // 脚本约定：status 0 = 服务器收到请求但响应丢失（直接断开），
                        // 用于验证“结果未知”路径（如 PUT 已应用但客户端看不到响应）。
                        drop(stream);
                        continue;
                    }
                    let body_bytes = resp.body.as_bytes();
                    let head = format!(
                        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        resp.status,
                        status_text(resp.status),
                        body_bytes.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body_bytes);
                }
            });
            Self {
                url: format!("http://{addr}"),
                stop,
                log,
                handler,
                addr,
                handle: Some(handle),
            }
        }

        pub fn set_handler(&self, h: Box<Handler>) {
            *self.handler.lock().unwrap() = h;
        }

        pub fn requests(&self) -> Vec<Req> {
            self.log.lock().unwrap().clone()
        }

        #[allow(dead_code)] // 测试工具 API：供后续用例按需使用。
        pub fn count(&self) -> usize {
            self.log.lock().unwrap().len()
        }

        pub fn requests_matching(&self, f: impl Fn(&Req) -> bool) -> usize {
            self.log.lock().unwrap().iter().filter(|r| f(r)).count()
        }

        #[allow(dead_code)] // 显式回收（Drop 已兜底，保留给需要提前停止的用例）。
        pub fn shutdown(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // 解除 accept 阻塞后回收线程。
            let _ = std::net::TcpStream::connect(self.addr);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    impl Drop for Scripted {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = std::net::TcpStream::connect(self.addr);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|w| w == needle)
    }

    fn head_len(buf: &[u8]) -> usize {
        find_sub(buf, b"\r\n\r\n").map(|p| p + 4).unwrap_or(buf.len())
    }

    fn status_text(status: u16) -> &'static str {
        match status {
            200 => "OK",
            401 => "Unauthorized",
            409 => "Conflict",
            503 => "Service Unavailable",
            _ => "Error",
        }
    }

    // 便捷 JSON 构造
    pub fn json_response(status: u16, body: &str) -> Resp {
        Resp {
            status,
            body: body.to_string(),
        }
    }

    pub const SNAPSHOT_EMPTY: &str = r#"{"items":[],"focusId":null,"version":0,"updatedAt":null}"#;
    pub fn put_ok(version: i64) -> String {
        format!(r#"{{"version":{version},"updatedAt":"2026-09-09T00:00:00Z"}}"#)
    }
    pub fn snapshot(items: &str, version: i64, focus: Option<&str>) -> String {
        let focus = focus.map(|f| format!("\"{f}\"")).unwrap_or_else(|| "null".into());
        format!(r#"{{"items":{items},"focusId":{focus},"version":{version},"updatedAt":null}}"#)
    }
}
