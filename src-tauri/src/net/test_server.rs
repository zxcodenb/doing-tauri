//! 隔离的并发 TCP 脚本服务器。每个请求独立执行，不持有 handler 锁等待响应；
//! Gate 让测试明确控制“请求已到达 → 切换会话 → 返回响应”，不用猜测 sleep 时序。
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Debug, Clone)]
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

#[derive(Default)]
pub struct Gate {
    released: Mutex<bool>,
    wake: Condvar,
}
impl Gate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn wait(&self) {
        let result = self
            .wake
            .wait_timeout_while(
                self.released.lock().unwrap(),
                Duration::from_secs(5),
                |released| !*released,
            )
            .unwrap();
        assert!(*result.0, "测试未在期限内释放响应屏障");
    }
    pub fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}

pub struct Scripted {
    pub url: String,
    stop: Arc<AtomicBool>,
    log: Arc<Mutex<Vec<Req>>>,
    handler: Arc<Mutex<Arc<Handler>>>,
    addr: SocketAddr,
    handle: Option<std::thread::JoinHandle<()>>,
}
impl Scripted {
    pub fn spawn(initial: Box<Handler>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定测试端口失败");
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Mutex<Arc<Handler>>> = Arc::new(Mutex::new(Arc::from(initial)));
        let (stop2, log2, handler2) = (stop.clone(), log.clone(), handler.clone());
        let handle = std::thread::spawn(move || {
            let mut workers = Vec::new();
            for stream in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else {
                    continue;
                };
                let log = log2.clone();
                let handler = handler2.lock().unwrap().clone();
                workers.push(std::thread::spawn(move || serve(stream, &log, &handler)));
            }
            for worker in workers {
                let _ = worker.join();
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
    pub fn set_handler(&self, handler: Box<Handler>) {
        *self.handler.lock().unwrap() = Arc::from(handler);
    }
    pub fn requests(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }
    pub fn requests_matching(&self, f: impl Fn(&Req) -> bool) -> usize {
        self.log.lock().unwrap().iter().filter(|r| f(r)).count()
    }
    pub async fn wait_for(&self, f: impl Fn(&Req) -> bool, count: usize) {
        for _ in 0..500 {
            if self.requests_matching(&f) >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("等待 {count} 个目标请求超时");
    }
}
impl Drop for Scripted {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
fn serve(mut stream: TcpStream, log: &Mutex<Vec<Req>>, handler: &Arc<Handler>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        let Ok(n) = stream.read(&mut chunk) else {
            return;
        };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 2_000_000 {
            return;
        }
        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buf[..end]);
        let length = head
            .lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if buf.len() >= end + 4 + length {
            break end;
        }
    };
    let head = String::from_utf8_lossy(&buf[..end]);
    let mut lines = head.lines();
    let mut parts = lines.next().unwrap_or_default().split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let bearer = lines.find_map(|l| {
        let (name, value) = l.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then(|| value.trim().strip_prefix("Bearer ").map(str::to_owned))
            .flatten()
    });
    let req = Req {
        method,
        path,
        bearer,
        body: String::from_utf8_lossy(&buf[end + 4..]).to_string(),
    };
    log.lock().unwrap().push(req.clone());
    let response = handler(&req);
    // status 0：请求可能已在服务器生效，但响应在返回前丢失。
    if response.status == 0 {
        return;
    }
    let header = format!("HTTP/1.1 {} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.status, response.body.len());
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(response.body.as_bytes());
}
pub fn json_response(status: u16, body: &str) -> Resp {
    Resp {
        status,
        body: body.to_owned(),
    }
}
pub const SNAPSHOT_EMPTY: &str = r#"{"items":[],"focusId":null,"version":0,"updatedAt":null}"#;
pub fn put_ok(version: i64) -> String {
    format!(r#"{{"version":{version},"updatedAt":"2026-09-09T00:00:00Z"}}"#)
}
pub fn snapshot(items: &str, version: i64, focus: Option<&str>) -> String {
    let focus = focus
        .map(|f| format!("\"{f}\""))
        .unwrap_or_else(|| "null".into());
    format!(r#"{{"items":{items},"focusId":{focus},"version":{version},"updatedAt":null}}"#)
}

/// 合成的 Go JWT 载荷（sub 为数值、username 为服务端规范用户名）；
/// 签名只用于测试，不做认证。本地仅解析它来隔离账号，授权仍由测试服务器决定。
pub fn tokens_for(user_id: i64, username: &str, tag: &str) -> super::dto::AuthDto {
    use base64::Engine;
    let header =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
        "sub": user_id, "username": username, "type": "access", "exp": 4102444800_i64, "testNonce": tag,
    })).unwrap());
    super::dto::AuthDto {
        access: format!("{header}.{payload}.test-only-signature"),
        refresh: format!("test-refresh-{user_id}-{tag}"),
        expires_in: 900,
    }
}
