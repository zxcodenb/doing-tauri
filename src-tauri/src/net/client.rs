//! 访问 Go 服务端 /api/v1 的 HTTP 客户端。
//! 规则（对应旧 Swift APIClient + 计划 §4.3）：
//! - 一个业务请求最多刷新一次 Token、重试原请求一次；
//! - 多个并发 401 共享一次刷新（single-flight）；
//! - 登出后的刷新结果必须丢弃（由上层 session 代次控制，这里只负责凭据）；
//! - 401 刷新失败 → 清凭据 → RefreshFailed（不再递归重试）。

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::Mutex;

use crate::creds::{api_error_from_cred, CredentialStore};
use crate::net::dto::*;

/// 刷新结果与其 single-flight 广播通道（复杂类型收敛别名）。
type RefreshResult = Result<AuthDto, ApiError>;
type RefreshBus = broadcast::Sender<RefreshResult>;

/// 单实例服务地址：开发用环回（显式 127.0.0.1 规避 localhost 双栈悬挂）；
/// 生产地址由受控配置（DOING_API_URL）决定（D06 未定前保持显式）。
pub fn default_base_url() -> String {
    if let Ok(url) = std::env::var("DOING_API_URL") {
        return url.trim_end_matches('/').to_string();
    }
    if cfg!(debug_assertions) {
        "http://127.0.0.1:8080".to_string()
    } else {
        // 正式发布前必须由发布环境注入；此处给出明确错误而不是静默指向本机。
        "https://api.invalid.invalid".to_string()
    }
}

#[derive(Clone)]
pub struct ApiClient {
    base_url: String,
    http: reqwest::Client,
    tokens: Arc<dyn CredentialStore>,
    refresh: Arc<Mutex<Option<RefreshBus>>>,
}

impl ApiClient {
    pub fn new(base_url: String, tokens: Arc<dyn CredentialStore>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(8))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client 构建失败");
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
            tokens,
            refresh: Arc::new(Mutex::new(None)),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn has_tokens(&self) -> bool {
        self.tokens.load().is_ok()
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    // MARK: - 认证端点

    pub async fn auth(
        &self,
        path: &str,
        username: &str,
        password: &str,
    ) -> ApiResult<AuthDto> {
        let body = serde_json::json!({ "username": username, "password": password });
        self.send("POST", path, Some(body), false).await
    }

    pub async fn logout(&self, refresh_token: &str) -> ApiResult<()> {
        let body = serde_json::json!({ "refresh": refresh_token });
        self.send::<serde_json::Value>("POST", "api/v1/auth/logout", Some(body), false)
            .await
            .map(|_| ())
    }

    pub async fn get_snapshot(&self) -> ApiResult<SnapshotDto> {
        self.send("GET", "api/v1/snapshot", None, true).await
    }

    pub async fn put_snapshot(
        &self,
        request: &SnapshotPutRequest,
    ) -> ApiResult<SnapshotPutResponse> {
        let body = serde_json::to_value(request).map_err(|_| ApiError::Decoding)?;
        self.send("PUT", "api/v1/snapshot", Some(body), true).await
    }

    // MARK: - 核心发送逻辑

    async fn send<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        authenticate: bool,
    ) -> ApiResult<T> {
        // 首次尝试（最多带一次自动刷新重试）。
        match self.perform::<T>(method, path, body.clone(), authenticate).await {
            Ok(v) => Ok(v),
            Err(ApiError::Unauthorized) if authenticate && self.has_tokens() => {
                // 401：刷新一次并重试原请求一次。
                let fresh = self.refresh_once().await?;
                // 只在实际取得新 Token 后重试（fresh 可能来自共享的并发刷新）。
                let _ = fresh;
                self.perform::<T>(method, path, body, true).await
            }
            Err(e) => Err(e),
        }
    }

    async fn perform<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        authenticate: bool,
    ) -> ApiResult<T> {
        let mut request = self
            .http
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET),
                self.url(path),
            )
            .header("Content-Type", "application/json");
        if authenticate {
            if let Ok(tokens) = self.tokens.load() {
                request = request.bearer_auth(tokens.access);
            } else {
                return Err(ApiError::Unauthorized);
            }
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|_| ApiError::Network)?;
        let status = response.status();
        let data = response.bytes().await.map_err(|_| ApiError::Network)?;

        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                if !authenticate {
                    // 刷新/登录端点本身 401：凭据已失效。
                    self.tokens.clear();
                    return Err(ApiError::Unauthorized);
                }
                return Err(ApiError::Unauthorized);
            }
            let body: Option<ApiErrorBody> = serde_json::from_slice(&data).ok();
            return Err(ApiError::Http {
                status: status.as_u16(),
                code: body
                    .as_ref()
                    .map(|b| b.code.clone())
                    .unwrap_or_else(|| fallback_code(status.as_u16())),
                message: body
                    .as_ref()
                    .map(|b| b.message.clone())
                    .unwrap_or_else(|| format!("HTTP {}", status.as_u16())),
                current_version: body.as_ref().and_then(|b| b.current_version),
            });
        }
        serde_json::from_slice(&data).map_err(|_| ApiError::Decoding)
    }

    /// 并发 401 共享一次刷新：leader 执行并广播结果，其余等待同一结果。
    async fn refresh_once(&self) -> ApiResult<AuthDto> {
        let subscribe = {
            let mut guard = self.refresh.lock().await;
            match guard.as_ref() {
                Some(sender) => Some(sender.subscribe()),
                None => {
                    let (tx, _rx) = broadcast::channel(1);
                    *guard = Some(tx);
                    None
                }
            }
        };
        let outcome = if let Some(mut rx) = subscribe {
            match rx.recv().await {
                Ok(result) => result,
                Err(_) => Err(ApiError::RefreshFailed),
            }
        } else {
            let result = self.do_refresh().await;
            let mut guard = self.refresh.lock().await;
            if let Some(sender) = guard.take() { let _ = sender.send(result.clone()); }
            result
        };
        match outcome {
            Ok(tokens) => {
                // 成功后凭据已由 do_refresh 保存（leader）或已由 leader 保存（等待者直接读取）。
                Ok(tokens)
            }
            Err(e) => Err(e),
        }
    }

    async fn do_refresh(&self) -> Result<AuthDto, ApiError> {
        let current = self.tokens.load().map_err(api_error_from_cred)?;
        let body = serde_json::json!({ "refresh": current.refresh });
        // 走 perform 而不是 send：避免 async 递归（send → refresh → do_refresh → send）。
        match self
            .perform::<AuthDto>("POST", "api/v1/auth/refresh", Some(body), false)
            .await
        {
            Ok(tokens) => {
                self.tokens
                    .save(&tokens)
                    .map_err(|_| ApiError::RefreshFailed)?;
                Ok(tokens)
            }
            Err(_) => {
                self.tokens.clear();
                Err(ApiError::RefreshFailed)
            }
        }
    }
}

fn fallback_code(status: u16) -> String {
    match status {
        400 => "bad_request",
        404 => "not_found",
        409 => "snapshot_conflict",
        429 => "rate_limited",
        503 => "service_unavailable",
        _ if status >= 500 => "internal_error",
        _ => "http_error",
    }
    .to_string()
}

#[allow(dead_code)]
fn _assert_serialize(_: impl Serialize) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creds::memory::MemoryStore;
    use crate::net::test_server::{json_response, Scripted, SNAPSHOT_EMPTY};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn seeded_store() -> Arc<MemoryStore> {
        let store = Arc::new(MemoryStore::default());
        store
            .save(&AuthDto {
                access: "old-access".into(),
                refresh: "old-refresh".into(),
                expires_in: 900,
            })
            .unwrap();
        store
    }

    fn client_for(server: &Scripted, store: Arc<MemoryStore>) -> ApiClient {
        ApiClient::new(server.url.clone(), store as Arc<dyn CredentialStore>)
    }

    /// 计划 §4.3：一个业务请求最多刷新一次 Token、重试原请求一次。
    #[tokio::test]
    async fn expired_access_refreshes_once_saves_and_retries() {
        let refreshes = Arc::new(AtomicUsize::new(0));
        let counter = refreshes.clone();
        let server = Scripted::spawn(Box::new(move |req| {
            if req.path.ends_with("/auth/refresh") {
                counter.fetch_add(1, Ordering::SeqCst);
                return json_response(
                    200,
                    r#"{"access":"new-access","refresh":"new-refresh","expiresIn":900}"#,
                );
            }
            match req.bearer.as_deref() {
                Some("new-access") => json_response(200, SNAPSHOT_EMPTY),
                _ => json_response(401, r#"{"code":"unauthorized","message":"expired"}"#),
            }
        }));
        let store = seeded_store();
        let client = client_for(&server, store.clone());

        let snap = client.get_snapshot().await.expect("刷新后重试应成功");
        assert_eq!(snap.version, 0);

        let reqs = server.requests();
        assert_eq!(
            reqs.iter()
                .filter(|r| r.path.ends_with("/auth/refresh"))
                .count(),
            1,
            "恰好一次刷新请求"
        );
        let snapshot_reqs: Vec<_> = reqs
            .iter()
            .filter(|r| r.path.ends_with("/snapshot"))
            .collect();
        assert_eq!(snapshot_reqs.len(), 2, "原请求重试一次");
        assert_eq!(snapshot_reqs[0].bearer.as_deref(), Some("old-access"));
        assert_eq!(snapshot_reqs[1].bearer.as_deref(), Some("new-access"));
        assert_eq!(store.load().unwrap().access, "new-access", "新 Token 已保存");
    }

    /// 计划 §4.3：401 刷新失败 → 清凭据 → RefreshFailed（不再递归重试）。
    #[tokio::test]
    async fn refresh_failure_clears_credentials_without_recursion() {
        let refreshes = Arc::new(AtomicUsize::new(0));
        let counter = refreshes.clone();
        let server = Scripted::spawn(Box::new(move |req| {
            if req.path.ends_with("/auth/refresh") {
                counter.fetch_add(1, Ordering::SeqCst);
                return json_response(
                    401,
                    r#"{"code":"unauthorized","message":"refresh expired"}"#,
                );
            }
            json_response(401, r#"{"code":"unauthorized","message":"expired"}"#)
        }));
        let store = seeded_store();
        let client = client_for(&server, store.clone());

        let err = client.get_snapshot().await.expect_err("应失败");
        assert_eq!(err, ApiError::RefreshFailed);
        assert!(!client.has_tokens(), "刷新失败必须清除凭据（无明文回退）");
        assert_eq!(refreshes.load(Ordering::SeqCst), 1, "不得递归刷新");
        assert_eq!(
            server
                .requests()
                .iter()
                .filter(|r| r.path.ends_with("/snapshot"))
                .count(),
            1,
            "刷新失败不再重试原请求"
        );
    }

    /// 计划 §4.3：多个并发 401 共享一次刷新（single-flight）。
    #[tokio::test]
    async fn concurrent_refreshes_share_single_flight() {
        let refreshes = Arc::new(AtomicUsize::new(0));
        let counter = refreshes.clone();
        let server = Scripted::spawn(Box::new(move |req| {
            if req.path.ends_with("/auth/refresh") {
                counter.fetch_add(1, Ordering::SeqCst);
                // 慢刷新：制造并发窗口，验证后来者等待同一次结果。
                std::thread::sleep(Duration::from_millis(200));
                return json_response(
                    200,
                    r#"{"access":"new-access","refresh":"new-refresh","expiresIn":900}"#,
                );
            }
            json_response(401, r#"{"code":"unauthorized","message":"expired"}"#)
        }));
        let store = seeded_store();
        let client = client_for(&server, store.clone());

        let (a, b) = tokio::join!(client.refresh_once(), client.refresh_once());
        assert!(a.is_ok() && b.is_ok(), "两者都应拿到刷新结果");
        assert_eq!(a.unwrap().access, "new-access");
        assert_eq!(b.unwrap().access, "new-access");
        assert_eq!(refreshes.load(Ordering::SeqCst), 1, "并发 401 只刷新一次");
    }
}
