//! 每个客户端绑定一个会话租约；401 共用互斥刷新，并按发出请求时的 access 判断是否已刷新。
//! HTTP 完成后再次验证租约。无凭据端点（登录/登出）永远没有清除仓库的副作用。
use crate::creds::{api_error_from_cred, SessionLease};
use crate::net::dto::*;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use tokio::sync::Mutex;

pub fn default_base_url() -> String {
    std::env::var("DOING_API_URL")
        .unwrap_or_else(|_| {
            if cfg!(debug_assertions) {
                "http://127.0.0.1:8080".into()
            } else {
                "https://api.invalid.invalid".into()
            }
        })
        .trim_end_matches('/')
        .to_owned()
}

pub fn normalize_base_url(value: &str) -> ApiResult<String> {
    let url = reqwest::Url::parse(value).map_err(|_| ApiError::InvalidConfiguration)?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    let secure = url.scheme() == "https"
        || (cfg!(any(test, debug_assertions)) && url.scheme() == "http" && loopback);
    if !secure
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ApiError::InvalidConfiguration);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

#[derive(Clone)]
pub struct ApiClient {
    base_url: String,
    http: reqwest::Client,
    session: Option<SessionLease>,
    refresh: Arc<Mutex<()>>,
}
impl ApiClient {
    pub fn anonymous(base_url: &str) -> ApiResult<Self> {
        Ok(Self::build(normalize_base_url(base_url)?, None))
    }
    pub fn authenticated(session: SessionLease) -> Self {
        Self::build(session.identity.owner.server_url.clone(), Some(session))
    }
    fn build(base_url: String, session: Option<SessionLease>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(8))
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client 构建失败");
        Self {
            base_url,
            http,
            session,
            refresh: Arc::new(Mutex::new(())),
        }
    }
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
    pub fn lease(&self) -> Option<&SessionLease> {
        self.session.as_ref()
    }
    pub fn has_tokens(&self) -> bool {
        self.session.as_ref().is_some_and(SessionLease::is_current)
    }
    pub fn session_id(&self) -> Option<uuid::Uuid> {
        self.session.as_ref().map(|s| s.id)
    }
    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    pub async fn auth(&self, path: &str, username: &str, password: &str) -> ApiResult<AuthDto> {
        if !matches!(path, "api/v1/auth/login" | "api/v1/auth/register") {
            return Err(ApiError::InvalidConfiguration);
        }
        self.perform(
            "POST",
            path,
            Some(serde_json::json!({"username":username, "password":password})),
            None,
        )
        .await
    }
    pub async fn logout(&self, refresh_token: &str) -> ApiResult<()> {
        self.perform::<serde_json::Value>(
            "POST",
            "api/v1/auth/logout",
            Some(serde_json::json!({"refresh":refresh_token})),
            None,
        )
        .await
        .map(|_| ())
    }
    pub async fn get_snapshot(&self) -> ApiResult<SnapshotDto> {
        let snapshot: SnapshotDto = self.send("GET", "api/v1/snapshot", None).await?;
        if snapshot.version < 0 || snapshot.to_local().is_err() {
            return Err(ApiError::InvalidResponse);
        }
        Ok(snapshot)
    }
    pub async fn put_snapshot(
        &self,
        request: &SnapshotPutRequest,
    ) -> ApiResult<SnapshotPutResponse> {
        let response: SnapshotPutResponse = self
            .send(
                "PUT",
                "api/v1/snapshot",
                Some(serde_json::to_value(request).map_err(|_| ApiError::Decoding)?),
            )
            .await?;
        if response.version <= request.base_version {
            return Err(ApiError::InvalidResponse);
        }
        Ok(response)
    }
    async fn send<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> ApiResult<T> {
        let session = self.session.as_ref().ok_or(ApiError::Unauthorized)?;
        let sent = session.load().map_err(api_error_from_cred)?;
        let first = self
            .perform(method, path, body.clone(), Some(&sent.access))
            .await;
        session.load().map_err(api_error_from_cred)?;
        let result = match first {
            Err(ApiError::Unauthorized) => {
                let fresh = self.refresh_after_401(&sent.access).await?;
                self.perform(method, path, body, Some(&fresh.access)).await
            }
            other => other,
        };
        session.load().map_err(api_error_from_cred)?;
        result
    }
    async fn refresh_after_401(&self, used_access: &str) -> ApiResult<AuthDto> {
        // 取消中的 leader 自动释放互斥锁，不留下无人广播的 single-flight 槽位。
        let _single = self.refresh.lock().await;
        let session = self.session.as_ref().ok_or(ApiError::Unauthorized)?;
        let current = session.load().map_err(api_error_from_cred)?;
        if current.access != used_access {
            return Ok(current);
        }
        let result = self
            .perform::<AuthDto>(
                "POST",
                "api/v1/auth/refresh",
                Some(serde_json::json!({"refresh":current.refresh})),
                None,
            )
            .await;
        session.load().map_err(api_error_from_cred)?;
        match result {
            Ok(tokens) => session
                .replace_tokens(&current.refresh, tokens)
                .map_err(api_error_from_cred),
            Err(ApiError::Unauthorized) => match session
                .reject_refresh(&current.refresh)
                .map_err(api_error_from_cred)?
            {
                Some(tokens) => Ok(tokens),
                None => Err(ApiError::RefreshFailed),
            },
            // 断网/5xx 不是 Refresh Token 被撤销的证据，不清除仍可恢复的凭据。
            Err(error) => Err(error),
        }
    }
    async fn perform<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        bearer: Option<&str>,
    ) -> ApiResult<T> {
        let mut request = self.http.request(
            reqwest::Method::from_bytes(method.as_bytes())
                .map_err(|_| ApiError::InvalidConfiguration)?,
            self.url(path),
        );
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|_| ApiError::Network)?;
        let status = response.status();
        let data = response.bytes().await.map_err(|_| ApiError::Network)?;
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            let body = serde_json::from_slice::<ApiErrorBody>(&data).ok();
            return Err(ApiError::Http {
                status: status.as_u16(),
                code: body
                    .as_ref()
                    .map(|b| b.code.clone())
                    .unwrap_or_else(|| "http_error".into()),
                message: body
                    .as_ref()
                    .map(|b| b.message.clone())
                    .unwrap_or_else(|| format!("HTTP {}", status.as_u16())),
                current_version: body.and_then(|b| b.current_version),
            });
        }
        serde_json::from_slice(if data.is_empty() { b"null" } else { &data })
            .map_err(|_| ApiError::Decoding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creds::{memory::MemoryStore, CredentialStore, CredentialVault, SavedSession};
    use crate::net::test_server::{json_response, tokens_for, Gate, Scripted, SNAPSHOT_EMPTY};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture {
        client: ApiClient,
        vault: Arc<CredentialVault>,
        store: Arc<MemoryStore>,
    }
    fn fixture(server: &Scripted) -> Fixture {
        let store = Arc::new(MemoryStore::default());
        let vault = CredentialVault::new(store.clone());
        let lease = vault
            .activate(SavedSession::new(&server.url, tokens_for(1, "alice", "old")).unwrap())
            .unwrap();
        Fixture {
            client: ApiClient::authenticated(lease),
            vault,
            store,
        }
    }
    fn token_json(tokens: &AuthDto) -> String {
        serde_json::to_string(tokens).unwrap()
    }

    #[tokio::test]
    async fn expired_access_refreshes_once_saves_and_retries() {
        let fresh = tokens_for(1, "alice", "new");
        let response = fresh.clone();
        let server = Scripted::spawn(Box::new(move |req| {
            if req.path.ends_with("/refresh") {
                json_response(200, &token_json(&response))
            } else if req.bearer.as_deref() == Some(&response.access) {
                json_response(200, SNAPSHOT_EMPTY)
            } else {
                json_response(401, r#"{"code":"unauthorized","message":"expired"}"#)
            }
        }));
        let f = fixture(&server);
        let id = f.store.load().unwrap().session_id;
        assert_eq!(f.client.get_snapshot().await.unwrap().version, 0);
        assert_eq!(
            server.requests_matching(|r| r.path.ends_with("/refresh")),
            1
        );
        let gets: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|r| r.method == "GET")
            .collect();
        assert_eq!(gets.len(), 2);
        assert_eq!(
            gets[0].bearer.as_deref(),
            Some(tokens_for(1, "alice", "old").access.as_str())
        );
        assert_eq!(gets[1].bearer.as_deref(), Some(fresh.access.as_str()));
        let stored = f.store.load().unwrap();
        assert_eq!(stored.tokens, fresh);
        assert_eq!(stored.session_id, id, "刷新不改变已绑定的账号会话身份");
    }

    #[tokio::test]
    async fn refresh_failure_clears_credentials_without_recursion() {
        let server = Scripted::spawn(Box::new(|_| {
            json_response(401, r#"{"code":"unauthorized","message":"expired"}"#)
        }));
        let f = fixture(&server);
        assert_eq!(
            f.client.get_snapshot().await.unwrap_err(),
            ApiError::RefreshFailed
        );
        assert!(!f.client.has_tokens());
        assert!(f.store.load().is_err());
        assert_eq!(
            server.requests_matching(|r| r.path.ends_with("/refresh")),
            1
        );
        assert_eq!(server.requests_matching(|r| r.method == "GET"), 1);
    }

    #[tokio::test]
    async fn concurrent_refreshes_share_single_flight() {
        let gate = Gate::new();
        let response_gate = gate.clone();
        let server = Scripted::spawn(Box::new(move |_| {
            response_gate.wait();
            json_response(200, &token_json(&tokens_for(1, "alice", "new")))
        }));
        let f = fixture(&server);
        let a = {
            let client = f.client.clone();
            tokio::spawn(async move {
                client
                    .refresh_after_401(&tokens_for(1, "alice", "old").access)
                    .await
            })
        };
        server.wait_for(|r| r.path.ends_with("/refresh"), 1).await;
        let b = {
            let client = f.client.clone();
            tokio::spawn(async move {
                client
                    .refresh_after_401(&tokens_for(1, "alice", "old").access)
                    .await
            })
        };
        gate.release();
        assert!(a.await.unwrap().is_ok());
        assert!(b.await.unwrap().is_ok());
        assert_eq!(
            server.requests_matching(|r| r.path.ends_with("/refresh")),
            1
        );
    }

    #[tokio::test]
    async fn late_logout_401_cannot_clear_new_credentials() {
        let gate = Gate::new();
        let response_gate = gate.clone();
        let server = Scripted::spawn(Box::new(move |_| {
            response_gate.wait();
            json_response(
                401,
                r#"{"code":"unauthorized","message":"old logout rejected"}"#,
            )
        }));
        let f = fixture(&server);
        let task = {
            let client = f.client.clone();
            tokio::spawn(async move { client.logout("old-refresh").await })
        };
        server.wait_for(|r| r.path.ends_with("/logout"), 1).await;
        f.vault.revoke().unwrap();
        let bob = tokens_for(2, "bob", "b");
        f.vault
            .activate(SavedSession::new(&server.url, bob.clone()).unwrap())
            .unwrap();
        gate.release();
        let _ = task.await.unwrap();
        assert!(f.store.load().is_ok(), "旧登出请求不得清掉新账户凭据");
        assert_eq!(f.store.load().unwrap().tokens, bob);
    }

    #[tokio::test]
    async fn concurrent_business_401s_share_refresh_even_when_second_is_late() {
        let gate = Gate::new();
        let response_gate = gate.clone();
        let counter = AtomicUsize::new(0);
        let old = tokens_for(1, "alice", "old");
        let server = Scripted::spawn(Box::new(move |req| {
            if req.path.ends_with("/refresh") {
                return json_response(200, &token_json(&tokens_for(1, "alice", "new")));
            }
            if req.bearer.as_deref() == Some(&old.access) {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    response_gate.wait();
                }
                json_response(401, r#"{"code":"unauthorized","message":"expired"}"#)
            } else {
                json_response(200, SNAPSHOT_EMPTY)
            }
        }));
        let f = fixture(&server);
        let first = {
            let client = f.client.clone();
            tokio::spawn(async move { client.get_snapshot().await })
        };
        server.wait_for(|r| r.method == "GET", 1).await;
        f.client.get_snapshot().await.unwrap();
        gate.release();
        first.await.unwrap().unwrap();
        assert_eq!(
            server.requests_matching(|r| r.path.ends_with("/refresh")),
            1,
            "迟到旧 access 的 401 应复用完成的刷新"
        );
    }

    #[tokio::test]
    async fn late_refresh_success_failure_or_network_error_cannot_touch_new_account() {
        for status in [200, 401, 503] {
            let gate = Gate::new();
            let response_gate = gate.clone();
            let server = Scripted::spawn(Box::new(move |req| {
                if req.path.ends_with("/refresh") {
                    response_gate.wait();
                    if status == 200 {
                        json_response(200, &token_json(&tokens_for(1, "alice", "late")))
                    } else {
                        json_response(
                            status,
                            r#"{"code":"unauthorized","message":"old refresh result"}"#,
                        )
                    }
                } else {
                    json_response(401, r#"{"code":"unauthorized","message":"expired"}"#)
                }
            }));
            let f = fixture(&server);
            let task = {
                let client = f.client.clone();
                tokio::spawn(async move { client.get_snapshot().await })
            };
            server.wait_for(|r| r.path.ends_with("/refresh"), 1).await;
            f.vault.revoke().unwrap();
            let bob = tokens_for(2, "bob", "new-account");
            f.vault
                .activate(SavedSession::new(&server.url, bob.clone()).unwrap())
                .unwrap();
            gate.release();
            assert_eq!(task.await.unwrap().unwrap_err(), ApiError::SessionChanged);
            assert_eq!(f.store.load().unwrap().tokens, bob);
            assert_eq!(
                server.requests_matching(|r| r.bearer.as_deref() == Some(&bob.access)),
                0,
                "旧业务请求也不能拿新账户 Token 重试"
            );
        }
    }

    #[tokio::test]
    async fn transient_refresh_failure_keeps_credentials_for_offline_recovery() {
        let server = Scripted::spawn(Box::new(|req| {
            if req.path.ends_with("/refresh") {
                json_response(
                    503,
                    r#"{"code":"service_unavailable","message":"try later"}"#,
                )
            } else {
                json_response(401, r#"{"code":"unauthorized","message":"expired"}"#)
            }
        }));
        let f = fixture(&server);
        assert!(matches!(
            f.client.get_snapshot().await,
            Err(ApiError::Http { status: 503, .. })
        ));
        assert!(f.client.has_tokens());
        assert_eq!(
            f.store.load().unwrap().tokens,
            tokens_for(1, "alice", "old")
        );
    }

    #[tokio::test]
    async fn business_retry_401_still_has_only_one_refresh_budget() {
        let server = Scripted::spawn(Box::new(|req| {
            if req.path.ends_with("/refresh") {
                json_response(200, &token_json(&tokens_for(1, "alice", "new")))
            } else {
                json_response(
                    401,
                    r#"{"code":"unauthorized","message":"still unauthorized"}"#,
                )
            }
        }));
        let f = fixture(&server);
        assert_eq!(
            f.client.get_snapshot().await.unwrap_err(),
            ApiError::Unauthorized
        );
        assert_eq!(
            server.requests_matching(|r| r.path.ends_with("/refresh")),
            1
        );
        assert_eq!(server.requests_matching(|r| r.method == "GET"), 2);
    }

    #[tokio::test]
    async fn refresh_cannot_change_the_account_bound_to_the_session() {
        let server = Scripted::spawn(Box::new(|req| {
            if req.path.ends_with("/refresh") {
                json_response(200, &token_json(&tokens_for(2, "bob", "wrong-account")))
            } else {
                json_response(401, r#"{"code":"unauthorized","message":"expired"}"#)
            }
        }));
        let f = fixture(&server);
        assert_eq!(
            f.client.get_snapshot().await.unwrap_err(),
            ApiError::Unauthorized
        );
        assert_eq!(f.store.load().unwrap().identity.owner.account_id, "1");
        assert_eq!(server.requests_matching(|r| r.method == "GET"), 1);
    }

    #[test]
    fn controlled_server_urls_reject_credentials_queries_and_non_loopback_http() {
        for url in [
            "http://example.com",
            "https://alice:secret@example.com",
            "https://example.com?access=secret",
            "https://example.com#fragment",
            "file:///tmp/secret",
        ] {
            assert!(normalize_base_url(url).is_err());
        }
        assert_eq!(
            normalize_base_url("HTTPS://EXAMPLE.COM:443/").unwrap(),
            "https://example.com"
        );
        assert!(normalize_base_url("http://127.0.0.1:8080").is_ok());
        assert!(normalize_base_url("http://[::1]:8080").is_ok());
    }
}
