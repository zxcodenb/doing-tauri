//! 认证/登出在 engine -> auth -> core 的同一临界区切换会话。
//! 网络位于锁外；每个完成结果必须仍属于发起它的认证代次。
use crate::creds::{CredError, SavedSession};
use crate::engine;
use crate::error::CommandError;
use crate::events::{AuthStateView, EVT_AUTH_STATE, EVT_SESSION_LOST};
use crate::net::client::{normalize_base_url, ApiClient};
use crate::net::dto::ApiError;
use crate::state::{AppState, AuthInner};
use crate::SyncShared;

pub fn view(auth: &AuthInner, generation: u64, revision: u64) -> AuthStateView {
    AuthStateView {
        session_generation: generation,
        event_revision: revision,
        logged_in: auth.logged_in,
        username: auth.username.clone(),
        server_url: auth.server_url.clone(),
        is_authenticating: auth.is_authenticating,
        error: auth.error.clone(),
    }
}
fn install(auth: &mut AuthInner, client: ApiClient) {
    let identity = &client.lease().expect("认证客户端必须有租约").identity;
    auth.username = Some(identity.username.clone());
    auth.server_url = Some(identity.owner.server_url.clone());
    auth.account_id = Some(identity.owner.account_id.clone());
    auth.client = Some(client);
    auth.logged_in = true;
    auth.is_authenticating = false;
    auth.error = None;
}
fn clear(auth: &mut AuthInner) {
    auth.client = None;
    auth.logged_in = false;
    auth.is_authenticating = false;
    auth.username = None;
    auth.server_url = None;
    auth.account_id = None;
}

pub(crate) fn reset_for_migration(
    e: &mut crate::state::EngineInner,
    auth: &mut AuthInner,
) -> Result<(), CommandError> {
    e.bump_session();
    let result = auth.vault.revoke().map_err(|_| {
        CommandError::new(
            "credentialsUnavailable",
            "无法清理新版凭据，请检查系统安全存储后重试迁移",
            true,
        )
    });
    clear(auth);
    auth.error = result.as_ref().err().map(|e| e.message.clone());
    result
}

pub async fn authenticate(
    st: SyncShared,
    path: &str,
    server_url: String,
    username: &str,
    password: &str,
) -> Result<(), CommandError> {
    let username = username.trim();
    if username.is_empty() || username.len() > 64 {
        return Err(CommandError::input("用户名必须为 1–64 字节"));
    }
    if password.is_empty() || (path.ends_with("/register") && password.len() < 8) {
        return Err(CommandError::input("请输入密码；注册密码至少 8 字节"));
    }
    let client = ApiClient::anonymous(&server_url).map_err(CommandError::from)?;
    let server_url = client.base_url().to_owned();
    let generation = {
        let mut e = st.engine.write().await;
        let mut auth = st.auth.lock().await;
        if st.core.lock().await.migration.blocks_writes() {
            return Err(crate::migrate::problem(
                "migrationPending",
                "请先完成、撤销或处理旧版导入，再登录",
            ));
        }
        if auth.is_authenticating {
            return Err("正在连接，请稍候".into());
        }
        if auth.logged_in {
            return Err("请先退出当前账号".into());
        }
        e.bump_session();
        clear(&mut auth);
        auth.error = None;
        if auth.vault.revoke().is_err() {
            auth.error = Some("系统安全存储不可用，无法安全开始新会话".into());
            let error =
                CommandError::new("credentialsUnavailable", auth.error.clone().unwrap(), true);
            let payload = view(&auth, e.session_generation, st.next_event_revision());
            drop(auth);
            drop(e);
            let _ = engine::try_emit(&st, EVT_AUTH_STATE, payload);
            return Err(error);
        }
        auth.is_authenticating = true;
        st.core.lock().await.store.clear_history();
        e.session_generation
    };
    emit_auth(&st).await;
    let response = client.auth(path, username, password).await;
    let mut e = st.engine.write().await;
    let mut auth = st.auth.lock().await;
    if e.session_generation != generation || !auth.is_authenticating {
        // 返回的 Token 不进入任何仓库，也不能撤销现在登录的账户。
        if let Ok(tokens) = response {
            tokio::spawn(async move {
                let _ = client.logout(&tokens.refresh).await;
            });
        }
        return Err("登录请求已取消".into());
    }
    e.bump_session();
    auth.is_authenticating = false;
    let outcome: Result<(), CommandError> = match response {
        Ok(tokens) => match SavedSession::new(&server_url, tokens) {
            Ok(saved) => {
                let mut core = st.core.lock().await;
                let foreign = (!core.store.is_empty() || core.meta.dirty)
                    && !core.meta.belongs_to(&saved.identity.owner);
                if foreign && st.repo.archive_current().is_err() {
                    Err(CommandError::persistence(
                        "无法备份原账号本地数据，尚未切换账号",
                    ))
                } else if let Err(error) = crate::migrate::acknowledge_new_login(&st, &mut core) {
                    Err(error)
                } else {
                    match auth.vault.activate(saved) {
                        Ok(lease) => {
                            install(&mut auth, ApiClient::authenticated(lease));
                            engine::refresh_view(
                                &mut e,
                                &core,
                                auth.client
                                    .as_ref()
                                    .and_then(|c| c.lease())
                                    .map(|l| &l.identity.owner),
                            );
                            Ok(())
                        }
                        Err(_) => Err(CommandError::new(
                            "credentialsUnavailable",
                            "系统安全存储不可用，无法保存登录状态",
                            true,
                        )),
                    }
                }
            }
            Err(_) => Err(CommandError::new(
                "invalidResponse",
                "服务器返回的账号凭据无效",
                false,
            )),
        },
        Err(ApiError::Unauthorized) => Err(CommandError::new(
            "unauthorized",
            "用户名或密码不正确",
            false,
        )),
        Err(error) => Err(error.into()),
    };
    if let Err(message) = &outcome {
        clear(&mut auth);
        auth.error = Some(message.message.clone());
    }
    let accepted_generation = e.session_generation;
    drop(auth);
    drop(e);
    emit_auth(&st).await;
    engine::emit_snapshot(&st).await;
    engine::emit_sync_state(&st).await;
    if outcome.is_ok() {
        crate::migrate::probe_and_emit(&st).await;
        tokio::spawn(async move {
            engine::bootstrap(&st, accepted_generation).await;
        });
    }
    outcome
}

/// expected 用于拒绝迟到的鉴权失败；None 是用户明确登出。
async fn reset(
    st: &SyncShared,
    expected: Option<(u64, uuid::Uuid)>,
    message: Option<String>,
    remote_logout: bool,
) -> Result<(), CommandError> {
    let mut e = st.engine.write().await;
    let mut auth = st.auth.lock().await;
    if let Some((generation, id)) = expected {
        if e.session_generation != generation
            || auth.client.as_ref().and_then(ApiClient::session_id) != Some(id)
        {
            return Ok(());
        }
    }
    let remote = auth
        .client
        .clone()
        .and_then(|c| c.lease()?.load().ok().map(|t| (c, t.refresh)));
    e.bump_session();
    let clear_error = auth
        .vault
        .revoke()
        .err()
        .map(|_| "当前会话已退出，但系统凭据删除失败；请检查权限后重试".to_owned());
    clear(&mut auth);
    auth.error = clear_error.clone().or_else(|| message.clone());
    let mut core = st.core.lock().await;
    // 数据归属、dirty、已确认基线及未决冲突保留；只清除会话内历史。
    core.store.clear_history();
    e.dirty = core.meta.dirty;
    e.state = if message.is_some() {
        crate::events::SyncStateView::Unauthorized
    } else {
        crate::events::SyncStateView::Idle
    };
    e.last_error = auth.error.clone();
    let payload = view(&auth, e.session_generation, st.next_event_revision());
    drop(core);
    drop(auth);
    drop(e);
    let _ = engine::try_emit(st, EVT_AUTH_STATE, payload.clone());
    if message.is_some() {
        let _ = engine::try_emit(st, EVT_SESSION_LOST, payload);
    }
    engine::emit_snapshot(st).await;
    engine::emit_sync_state(st).await;
    if remote_logout {
        if let Some((client, refresh)) = remote {
            tokio::spawn(async move {
                let _ = client.logout(&refresh).await;
            });
        }
    }
    clear_error.map_or(Ok(()), |message| {
        Err(CommandError::new("credentialsUnavailable", message, true))
    })
}
pub async fn logout(st: &SyncShared) -> Result<(), CommandError> {
    reset(st, None, None, true).await
}
pub async fn lose_session(st: &SyncShared, generation: u64, id: uuid::Uuid, error: &ApiError) {
    let _ = reset(
        st,
        Some((generation, id)),
        Some(error.display_message().to_owned()),
        false,
    )
    .await;
}
#[cfg(test)]
pub async fn reset_for_test(st: &SyncShared) {
    reset(st, None, None, false).await.unwrap();
}

pub async fn emit_auth(st: &SyncShared) {
    let e = st.engine.read().await;
    let auth = st.auth.lock().await;
    let payload = view(&auth, e.session_generation, st.next_event_revision());
    drop(auth);
    drop(e);
    let _ = engine::try_emit(st, EVT_AUTH_STATE, payload);
}
pub async fn resume_after_restart(st: &SyncShared) {
    let generation = st.engine.read().await.session_generation;
    engine::bootstrap(st, generation).await;
}

/// setup / 测试的真实重载路径：身份只从受保护的凭据记录恢复，绝不从任务归属猜测。
pub fn restore_credentials(st: &AppState, server_url: &str) {
    let mut e = st.engine.blocking_write();
    let mut auth = st.auth.blocking_lock();
    e.bump_session();
    clear(&mut auth);
    if st.core.blocking_lock().migration.requires_login() {
        auth.error = Some("旧版导入后需重新登录；迁移前会话不会恢复".into());
        return;
    }
    let server_url = match normalize_base_url(server_url) {
        Ok(url) => url,
        Err(error) => {
            auth.error = Some(error.display_message().to_owned());
            return;
        }
    };
    match auth.vault.restore(&server_url) {
        Ok(lease) => install(&mut auth, ApiClient::authenticated(lease)),
        Err(CredError::Unavailable) => {
            auth.error = Some("系统安全存储不可用，请检查权限后重新登录".into())
        }
        Err(_) => {}
    }
    let core = st.core.blocking_lock();
    engine::refresh_view(
        &mut e,
        &core,
        auth.client
            .as_ref()
            .and_then(|c| c.lease())
            .map(|l| &l.identity.owner),
    );
}

#[cfg(test)]
pub async fn install_test_session(
    st: &AppState,
    server_url: &str,
    tokens: crate::net::dto::AuthDto,
) {
    let mut e = st.engine.write().await;
    let mut auth = st.auth.lock().await;
    let lease = auth
        .vault
        .activate(SavedSession::new(server_url, tokens).unwrap())
        .unwrap();
    e.bump_session();
    install(&mut auth, ApiClient::authenticated(lease));
    let core = st.core.lock().await;
    engine::refresh_view(
        &mut e,
        &core,
        auth.client
            .as_ref()
            .and_then(ApiClient::lease)
            .map(|l| &l.identity.owner),
    );
}

#[cfg(test)]
mod race_tests {
    use super::*;
    use crate::creds::{memory::MemoryStore, CredentialStore};
    use crate::net::test_server::{json_response, Gate, Scripted, SNAPSHOT_EMPTY};
    use std::sync::Arc;

    fn state() -> (SyncShared, Arc<MemoryStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let creds = Arc::new(MemoryStore::default());
        let st = Arc::new(AppState::new(dir.path().into(), None, creds.clone()));
        (st, creds, dir)
    }

    #[tokio::test]
    async fn late_login_after_logout_does_not_resurrect_session() {
        let (st, creds, _dir) = state();
        let gate = Gate::new();
        let response_gate = gate.clone();
        let server = Scripted::spawn(Box::new(move |req| {
            if req.path.ends_with("/login") {
                response_gate.wait();
                json_response(
                    200,
                    &serde_json::to_string(&crate::net::test_server::tokens_for(
                        1, "alice", "late",
                    ))
                    .unwrap(),
                )
            } else {
                json_response(200, SNAPSHOT_EMPTY)
            }
        }));
        let task = {
            let st = st.clone();
            let url = server.url.clone();
            tokio::spawn(async move {
                authenticate(st, "api/v1/auth/login", url, "alice", "password123").await
            })
        };
        server.wait_for(|r| r.path.ends_with("/login"), 1).await;
        engine::session_reset(&st).await;
        gate.release();
        let _ = task.await.unwrap();
        assert!(!st.auth.lock().await.logged_in, "迟到登录响应不能重新登录");
        assert!(creds.load().is_err(), "登出后不得保存迟到的凭据");
    }

    #[tokio::test]
    async fn late_refresh_after_logout_does_not_recreate_credentials() {
        let (st, creds, _dir) = state();
        let gate = Gate::new();
        let response_gate = gate.clone();
        let server = Scripted::spawn(Box::new(move |req| {
            if req.path.ends_with("/refresh") {
                response_gate.wait();
                json_response(
                    200,
                    &serde_json::to_string(&crate::net::test_server::tokens_for(
                        1, "alice", "late",
                    ))
                    .unwrap(),
                )
            } else if req.bearer.as_deref()
                == Some(
                    crate::net::test_server::tokens_for(1, "alice", "old")
                        .access
                        .as_str(),
                )
            {
                json_response(401, r#"{"code":"unauthorized","message":"expired"}"#)
            } else {
                json_response(200, SNAPSHOT_EMPTY)
            }
        }));
        install_test_session(
            &st,
            &server.url,
            crate::net::test_server::tokens_for(1, "alice", "old"),
        )
        .await;
        let client = st.auth.lock().await.client.clone().unwrap();
        let task = tokio::spawn(async move { client.get_snapshot().await });
        server.wait_for(|r| r.path.ends_with("/refresh"), 1).await;
        engine::session_reset(&st).await;
        gate.release();
        let _ = task.await.unwrap();
        assert!(creds.load().is_err(), "迟到刷新不能重建已登出的凭据");
    }
    #[tokio::test]
    async fn late_account_a_login_cannot_replace_canonical_account_b_credentials() {
        let (st, creds, _dir) = state();
        st.core.lock().await.settings.automatic_sync = false;
        let gate = Gate::new();
        let response_gate = gate.clone();
        let server = Scripted::spawn(Box::new(move |req| {
            if req.path.ends_with("/login") {
                if req.body.contains("alice") {
                    response_gate.wait();
                    json_response(
                        200,
                        &serde_json::to_string(&crate::net::test_server::tokens_for(
                            1, "Alice", "a",
                        ))
                        .unwrap(),
                    )
                } else {
                    json_response(
                        200,
                        &serde_json::to_string(&crate::net::test_server::tokens_for(
                            2,
                            "CanonicalBob",
                            "b",
                        ))
                        .unwrap(),
                    )
                }
            } else {
                json_response(200, SNAPSHOT_EMPTY)
            }
        }));
        let login_a = {
            let st = st.clone();
            let url = server.url.clone();
            tokio::spawn(async move {
                authenticate(st, "api/v1/auth/login", url, "alice", "password123").await
            })
        };
        server.wait_for(|r| r.path.ends_with("/login"), 1).await;
        reset_for_test(&st).await;
        authenticate(
            st.clone(),
            "api/v1/auth/login",
            server.url.clone(),
            " bob ",
            "password456",
        )
        .await
        .unwrap();
        let session_b = creds.load().unwrap().session_id;
        gate.release();
        assert!(login_a.await.unwrap().is_err());
        assert!(st.auth.lock().await.logged_in);
        assert_eq!(
            st.auth.lock().await.username.as_deref(),
            Some("CanonicalBob")
        );
        assert_eq!(creds.load().unwrap().session_id, session_b);
        assert_eq!(creds.load().unwrap().identity.owner.account_id, "2");
    }
}
