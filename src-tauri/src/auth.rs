//! 认证会话：登录/注册/登出、凭据保存、账号归属仲裁触发。
//! 规则（计划 §4.2/§4.3）：Token 只在系统安全存储；账号或服务地址改变时
//! 先备份本地数据，未经明确确认不上传、不覆盖。

use std::path::PathBuf;

use crate::engine;
use crate::events::EVT_AUTH_STATE;
use crate::net::client::ApiClient;
use crate::net::dto::ApiError;
use crate::state::AppState;
use crate::SyncShared;

/// 登录/注册共用流程。`path` 区分 login/register；服务地址由调用方注入（测试可控）。
pub async fn authenticate(
    st: SyncShared,
    path: &str,
    server_url: String,
    username: &str,
    password: &str,
) -> Result<(), String> {
    {
        let mut auth = st.auth.lock().await;
        if auth.is_authenticating {
            return Err("正在连接，请稍候".into());
        }
        auth.is_authenticating = true;
    }
    let username = username.trim().to_string();
    let creds = st.auth.lock().await.creds.clone();
    let client = ApiClient::new(server_url.clone(), creds);
    let result = client.auth(path, &username, password).await;

    let mut auth = st.auth.lock().await;
    auth.is_authenticating = false;
    match result {
        Ok(tokens) => {
            // 凭据只进系统安全存储；不可用时可见报错，绝不回退明文。
            if auth.creds.save(&tokens).is_err() {
                auth.creds.clear();
                return Err("系统安全存储不可用，无法保存登录状态".into());
            }
            let (prior_owner, local_nonempty) = {
                let core = st.core.lock().await;
                (core.meta.owner(), !core.store.is_empty())
            };
            let new_owner = format!("{server_url}#{username}");
            let owner_changed = prior_owner.as_deref() != Some(new_owner.as_str());

            // 账号归属变化且本地有数据：先做可回滚备份，再进入仲裁（不自动上传）。
            if local_nonempty && owner_changed {
                if let Some(prior) = &prior_owner {
                    archive_current_data(&st, prior).await;
                }
            }

            auth.client = Some(client);
            auth.logged_in = true;
            auth.username = Some(username.clone());
            auth.server_url = Some(server_url.clone());
            drop(auth);

            // 引擎进入全新会话代次。
            st.engine.write().await.bump_session();
            emit_auth(&st).await;
            engine::emit_snapshot(&st).await;

            // 本地为空：直接声明归属；否则由仲裁对话框决定归属。
            if !local_nonempty {
                claim_owner(&st).await;
            }
            let st2 = st.clone();
            tokio::spawn(async move { resolve_initial(&st2, new_owner).await });
            Ok(())
        }
        Err(ApiError::Unauthorized) => {
            auth.logged_in = false;
            Err("用户名或密码不正确".into())
        }
        Err(error) => {
            auth.logged_in = false;
            Err(error.display_message().to_string())
        }
    }
}

/// 把当前数据文件复制为带归属说明的归档（可回滚，不删除）。
async fn archive_current_data(st: &AppState, prior_owner: &str) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let n = st
        .owner_archive_counter
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let tag = prior_owner.replace(['/', '#', ':'], "_");
    let source = st.repo.path();
    let target = PathBuf::from(format!(
        "{}.owner-backup-{stamp}-{n}.{tag}",
        source.display()
    ));
    if source.exists() {
        let _ = std::fs::copy(source, target);
    }
}

/// 登录后的首次仲裁：沿用“四组合”；本地非空且归属变化/无归属时不自动上传。
async fn resolve_initial(st: &SyncShared, new_owner: String) {
    let Some(client) = st.auth.lock().await.client.clone() else {
        return;
    };
    let (owner_server, owner_user) = new_owner
        .split_once('#')
        .unwrap_or((new_owner.as_str(), ""));
    let epoch = {
        let mut e = st.engine.write().await;
        e.bump_generation();
        e.epoch
    };
    let local_empty = st.core.lock().await.store.is_empty();
    let local_owned_by = st.core.lock().await.meta.owner();

    match client.get_snapshot().await {
        Err(ApiError::Unauthorized | ApiError::RefreshFailed) => {
            engine::handle_auth_lost(st).await;
        }
        Err(error) => {
            let mut e = st.engine.write().await;
            e.state = crate::events::SyncStateView::Failed;
            e.last_error = Some(error.display_message().to_string());
            drop(e);
            engine::emit_sync_state(st).await;
        }
        Ok(cloud) => {
            if engine::is_stale_public(st, epoch).await {
                return;
            }
            let cloud_empty = cloud.items.is_empty();
            if local_empty {
                engine::claim_owner_public(st).await;
                if cloud_empty {
                    engine::mark_synced(st, cloud.version).await;
                } else {
                    engine::choose_cloud(st, &cloud).await;
                }
                return;
            }
            let _ = (owner_server, owner_user);
            // 本地非空：归属一致且云端为空 → 自动上传（四组合之三）。
            let safe_auto_push =
                cloud_empty && local_owned_by.as_deref() == Some(new_owner.as_str());
            if safe_auto_push {
                engine::set_known_version(st, cloud.version).await;
                engine::flush(st).await;
                return;
            }
            // 其余（云端有数据 / 归属变化 / 无归属数据）：交用户选择。
            engine::prepare_conflict(st, cloud).await;
        }
    }
}

/// 登出：本地先清（凭据/会话），服务端撤销尽力而为。
pub async fn logout(st: &SyncShared) {
    let (refresh_token, client) = {
        let auth = st.auth.lock().await;
        (auth.creds.load().ok().map(|t| t.refresh), auth.client.clone())
    };
    engine::session_reset(st).await;
    if let (Some(token), Some(client)) = (refresh_token, client) {
        let _ = client.logout(&token).await;
    }
    emit_auth(st).await;
}

pub async fn emit_auth(st: &SyncShared) {
    let payload = {
        let auth = st.auth.lock().await;
        crate::events::AuthStateView {
            logged_in: auth.logged_in,
            username: auth.username.clone(),
            server_url: auth.server_url.clone(),
            is_authenticating: auth.is_authenticating,
        }
    };
    let _ = crate::engine::try_emit(st, EVT_AUTH_STATE, payload);
}

/// 声明数据归属为当前账号（本地为空或用户确认保留本地后调用）。
pub async fn claim_owner(st: &SyncShared) {
    engine::claim_owner_public(st).await;
}

/// 重启恢复：凭据已在（logged_in）时按归属与状态决定自动续传/云端恢复/冲突挂起。
/// 规则（计划 §4.3）：同归属才允许自动上传；无归属或归属不同的本地数据必须先经用户选择。
pub async fn resume_after_restart(st: &SyncShared) {
    if !st.auth.lock().await.logged_in {
        return;
    }
    if st.engine.read().await.state == crate::events::SyncStateView::Conflict {
        return; // 保持未决冲突挂起，绝不自动覆盖。
    }
    let Some(client) = st.auth.lock().await.client.clone() else {
        return;
    };
    let (local_empty, dirty, owner_match) = {
        // 全局锁序约定：先 auth 后 core（与 authenticate 保持一致，避免 ABBA）。
        let auth = st.auth.lock().await;
        let core = st.core.lock().await;
        let owner_match = auth.username.is_some()
            && core.meta.username == auth.username
            && core.meta.server_url == auth.server_url;
        (core.store.is_empty(), core.meta.dirty, owner_match)
    };
    let epoch = {
        let mut e = st.engine.write().await;
        e.bump_generation();
        e.epoch
    };
    match client.get_snapshot().await {
        Err(ApiError::Unauthorized | ApiError::RefreshFailed) => {
            engine::handle_auth_lost(st).await;
        }
        Err(error) => {
            let mut e = st.engine.write().await;
            e.state = crate::events::SyncStateView::Failed;
            e.last_error = Some(error.display_message().to_string());
            drop(e);
            engine::emit_sync_state(st).await;
            // dirty 场景由 retry_driver 继续重试。
        }
        Ok(cloud) => {
            if engine::is_stale_public(st, epoch).await {
                return;
            }
            let cloud_empty = cloud.items.is_empty();
            if local_empty {
                if cloud_empty {
                    engine::mark_synced(st, cloud.version).await;
                } else {
                    engine::choose_cloud(st, &cloud).await;
                }
                return;
            }
            if owner_match {
                if cloud_empty && !dirty {
                    engine::mark_synced(st, cloud.version).await;
                    return;
                }
                engine::set_known_version(st, cloud.version).await;
                engine::flush(st).await;
                return;
            }
            // 无归属 / 归属不同的本地数据：交用户选择（不自动上传、不自动覆盖）。
            engine::prepare_conflict(st, cloud).await;
        }
    }
}
