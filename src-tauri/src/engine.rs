//! 快照同步引擎（语义对齐旧 SyncEngine + 计划 §4.3 不变量）。
//! 约定：本模块所有公开函数接收 `&SyncShared`（Arc<AppState>）以便派生后台任务。
//! 网络请求全部在锁外执行；状态更新在锁内短临界区完成；
//! epoch 代次使旧任务失效；session 代次防护跨账号污染；push_lock 保证上传串行。

use std::sync::Arc;
use tokio::sync::watch;

use doing_core::clock::SystemClock;
use doing_core::Clock;

use crate::events::*;
use crate::net::client::ApiClient;
use crate::net::dto::{ApiError, ItemDto, SnapshotDto, SnapshotPutRequest};
use crate::state::{AppState, CoreInner};
use tauri::Emitter;
use crate::SyncShared;

pub const DEBOUNCE_MS: u64 = 2000;
pub const RETRY_BASE_MS: u64 = 2000;

// 事件发射：未注入 AppHandle（单元测试）时静默跳过。
pub fn try_emit<T: serde::Serialize + Clone>(st: &AppState, event: &str, payload: T) -> bool {
    match st.app_handle.read() {
        Ok(guard) => guard
            .as_ref()
            .map(|h| h.emit(event, payload.clone()).is_ok())
            .unwrap_or(false),
        Err(_) => false,
    }
}

fn now_str() -> String {
    SystemClock
        .now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// MARK: - 事件发射

pub async fn emit_sync_state(st: &SyncShared) {
    let payload = {
        let e = st.engine.read().await;
        SyncStateViewPayload {
            state: e.state,
            last_sync_at: e.last_sync_at.clone(),
            last_error: e.last_error.clone(),
            conflict_cloud_count: e.conflict.as_ref().map(|c| c.items.len()),
            conflict_cloud_version: e.conflict.as_ref().map(|c| c.version),
        }
    };
    let _ = try_emit(st, EVT_SYNC_STATE, payload);
}

pub async fn emit_snapshot(st: &SyncShared) {
    let core = st.core.lock().await;
    let payload = SnapshotView::from_store(&core.store, core.save_failed);
    drop(core);
    let _ = try_emit(st, EVT_SNAPSHOT, payload);
}

// MARK: - 持久化

/// 原子保存当前数据文件（任务与同步元数据同一提交边界）。
/// 锁内短临界区构建快照，锁外执行文件写入；失败时更新 save_failed。
pub async fn persist_all(st: &SyncShared) -> bool {
    let data = st.core.lock().await.to_data_file();
    match st.repo.save(&data) {
        Ok(()) => {
            st.core.lock().await.save_failed = false;
            true
        }
        Err(e) => {
            eprintln!("[persist] 保存失败: {e}");
            st.core.lock().await.save_failed = true;
            let _ = try_emit(st, EVT_SAVE_FAILED, e.to_string());
            false
        }
    }
}

/// 在锁内执行变更并立即原子落盘；返回 (变更结果, 是否成功保存)。
pub async fn mutate_core<F, R>(st: &SyncShared, f: F) -> (R, bool)
where
    F: FnOnce(&mut CoreInner) -> R,
{
    let out = {
        let mut core = st.core.lock().await;
        f(&mut core)
    };
    let saved = persist_all(st).await;
    (out, saved)
}

// MARK: - 变更入口

/// 本地变更入口（workspace 命令成功落盘后调用）：置 dirty → 自动同步防抖。
pub async fn on_core_mutated(st: &SyncShared) {
    {
        let mut core = st.core.lock().await;
        core.meta.dirty = true;
    }
    persist_all(st).await;
    {
        let mut e = st.engine.write().await;
        e.dirty = true;
        e.state = SyncStateView::Pending;
    }
    emit_sync_state(st).await;
    maybe_auto_sync(st).await;
}

/// 手动同步（冲突态除外）。
pub async fn flush(st: &SyncShared) {
    if st.engine.read().await.state == SyncStateView::Conflict {
        return;
    }
    schedule_push(st, 0).await;
}

async fn maybe_auto_sync(st: &SyncShared) {
    // 注意锁序：全局约定“先 auth 后 core”或分次取；此处分两次短锁，避免 core→auth 反转。
    let auto = st.core.lock().await.settings.automatic_sync;
    let logged_in = st.auth.lock().await.logged_in;
    let e = st.engine.read().await;
    let blocked = !logged_in
        || !auto
        || e.state == SyncStateView::Conflict
        || e.state == SyncStateView::Syncing;
    drop(e);
    if blocked {
        return;
    }
    schedule_push(st, DEBOUNCE_MS).await;
}

async fn schedule_push(st: &SyncShared, delay_ms: u64) {
    let epoch = {
        let mut e = st.engine.write().await;
        e.bump_generation();
        e.epoch
    };
    let st = Arc::clone(st);
    let cancel_rx = st.engine.read().await.cancel_tx.subscribe();
    tokio::spawn(async move {
        let ok = wait_guarded(cancel_rx, epoch, std::time::Duration::from_millis(delay_ms)).await;
        if ok {
            push(&st, epoch).await;
        }
    });
}

async fn wait_guarded(
    mut cancel: watch::Receiver<u64>,
    _epoch: u64,
    duration: std::time::Duration,
) -> bool {
    // 超时返回 Err → 到期继续执行；changed 返回 Ok → 任务已被取代。
    tokio::time::timeout(duration, cancel.changed())
        .await
        .map(|_| false)
        .unwrap_or(true)
}

async fn is_stale(st: &SyncShared, epoch: u64) -> bool {
    st.engine.read().await.epoch != epoch
}

/// 供 auth 等外部模块判断任务是否已被取代。
pub async fn is_stale_public(st: &SyncShared, epoch: u64) -> bool {
    is_stale(st, epoch).await
}

async fn client(st: &SyncShared) -> Option<ApiClient> {
    st.auth.lock().await.client.clone()
}

// MARK: - 完整上传流程

async fn push(st: &SyncShared, epoch: u64) {
    // 串行化：等待在途上传完成后，本代仍有效则继续。
    let push_lock = st.engine.read().await.push_lock.clone();
    let _serial = push_lock.lock().await;
    if is_stale(st, epoch).await {
        return;
    }
    let Some(client) = client(st).await else { return };
    let session = st.engine.read().await.session_generation;
    if st.engine.read().await.state == SyncStateView::Conflict {
        return;
    }
    {
        let mut e = st.engine.write().await;
        if e.epoch != epoch {
            return;
        }
        e.state = SyncStateView::Syncing;
        e.last_error = None;
    }
    // 1) 无基线先 GET（绝不盲目 PUT）。
    // 注意：读取锁必须先独立取值再 match——match 的 scrutinee 临时会把读锁
    // 存活到整段分支（含内部 await 里的 engine.write），造成自死锁。
    let known = st.engine.read().await.known_version;
    let base = match known {
        Some(v) => Some(v),
        None => match client.get_snapshot().await {
            Ok(snapshot) => {
                if is_stale(st, epoch).await {
                    return;
                }
                set_known_version(st, snapshot.version).await;
                Some(snapshot.version)
            }
            Err(error) => {
                handle_push_failure(st, epoch, error, RETRY_BASE_MS).await;
                return;
            }
        },
    };
    let Some(base) = base else { return };
    if is_stale(st, epoch).await {
        return;
    }

    // 2) 锁内抓取本地快照，锁外执行 PUT。
    let request = {
        let core = st.core.lock().await;
        SnapshotPutRequest {
            items: core.store.items().iter().map(ItemDto::from).collect(),
            focus_id: core.store.focus_id(),
            base_version: base,
        }
    };
    let result = client.put_snapshot(&request).await;

    // 会话已切换：旧结果一律丢弃（版本也不采纳）。
    if st.engine.read().await.session_generation != session {
        return;
    }
    if is_stale(st, epoch).await {
        // 版本是服务端客观事实，仍采纳；状态推进由新代处理。
        if let Ok(response) = &result {
            set_known_version(st, response.version).await;
        }
        return;
    }
    match result {
        Ok(response) => {
            mark_synced(st, response.version).await;
        }
        Err(error) => {
            handle_push_failure(st, epoch, error, RETRY_BASE_MS).await;
        }
    }
}

// MARK: - 状态推进

pub async fn set_known_version(st: &SyncShared, version: i64) {
    st.core.lock().await.meta.known_server_version = Some(version);
    persist_all(st).await;
    st.engine.write().await.known_version = Some(version);
}

pub async fn mark_synced(st: &SyncShared, version: i64) {
    {
        let mut core = st.core.lock().await;
        core.meta.known_server_version = Some(version);
        core.meta.dirty = false;
    }
    persist_all(st).await;
    {
        let mut e = st.engine.write().await;
        e.known_version = Some(version);
        e.dirty = false;
        e.state = SyncStateView::Synced;
        e.last_sync_at = Some(now_str());
        e.last_error = None;
    }
    emit_sync_state(st).await;
}

// MARK: - 失败处置

/// 失败处置：登录失效走登出；409 拉最新云端转冲突；其余置 failed，
/// 由全局 retry_driver 周期重试（不在此处 spawn 递归，避免栈式任务堆积）。
async fn handle_push_failure(st: &SyncShared, _epoch: u64, error: ApiError, _retry_delay_ms: u64) {
    if error.is_auth_failure() {
        handle_auth_lost(st).await;
    } else if error.is_conflict() {
        resolve_conflict(st).await;
    } else {
        let mut e = st.engine.write().await;
        e.state = SyncStateView::Failed;
        e.last_error = Some(error.display_message().to_string());
        drop(e);
        emit_sync_state(st).await;
    }
}

/// 409 处置：拉取最新云端进入冲突流程（拉取失败则置 failed 等驱动重试）。
async fn resolve_conflict(st: &SyncShared) {
    let Some(client) = client(st).await else { return };
    match client.get_snapshot().await {
        Ok(cloud) => {
            prepare_conflict(st, cloud).await;
        }
        Err(error) => {
            if error.is_auth_failure() {
                handle_auth_lost(st).await;
                return;
            }
            let mut e = st.engine.write().await;
            e.state = SyncStateView::Failed;
            e.last_error = Some(error.display_message().to_string());
            drop(e);
            emit_sync_state(st).await;
        }
    }
}

/// 全局重试驱动：failed + dirty 且有会话时周期重新上传（指数退避上限 60s）。
pub async fn retry_tick(st: &SyncShared) {
    let should = {
        let e = st.engine.read().await;
        e.state == SyncStateView::Failed && e.dirty
    };
    if !should {
        return;
    }
    let logged_in = st.auth.lock().await.logged_in;
    if logged_in {
        schedule_push(st, 0).await;
    }
}

/// 失败后的等待间隔：交给 retry_driver 固定周期触发，无需此处调度。
pub fn start_retry_driver(state: SyncShared) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            retry_tick(&state).await;
        }
    });
}

// MARK: - 冲突

/// 冲突/仲裁的公共挂起态：保留候选、采纳其版本、停止自动上传。
pub async fn prepare_conflict(st: &SyncShared, cloud: SnapshotDto) {
    {
        let mut e = st.engine.write().await;
        e.bump_generation();
        e.known_version = Some(cloud.version);
        e.conflict = Some(cloud.clone());
        e.last_error = Some("本地与云端都有事项，请选择保留的版本。".into());
        e.state = SyncStateView::Conflict;
    }
    st.core.lock().await.meta.known_server_version = Some(cloud.version);
    persist_all(st).await;
    emit_sync_state(st).await;
    let _ = try_emit(st, EVT_CONFLICT, ConflictView::from(&cloud));
}

/// 选择“本地”：以云端最新版本为基线上传本地。
pub async fn choose_local(st: &SyncShared) {
    {
        let mut e = st.engine.write().await;
        e.conflict = None;
        e.state = SyncStateView::Idle;
    }
    emit_sync_state(st).await;
    flush(st).await;
}

/// 选择“云端”：本地与基线都采用云端快照。
pub async fn choose_cloud(st: &SyncShared, cloud: &SnapshotDto) {
    let Some((items, focus)) = cloud.to_local().ok() else {
        return;
    };
    {
        let mut core = st.core.lock().await;
        core.store.replace_all(items, focus);
        core.meta.known_server_version = Some(cloud.version);
        core.meta.dirty = false;
    }
    persist_all(st).await;
    {
        let mut e = st.engine.write().await;
        e.conflict = None;
        e.known_version = Some(cloud.version);
        e.dirty = false;
        e.state = SyncStateView::Synced;
        e.last_sync_at = Some(now_str());
        e.last_error = None;
    }
    emit_snapshot(st).await;
    emit_sync_state(st).await;
    crate::reminder::refresh(st).await;
}

/// 从云端恢复（用户显式触发）：恢复期间本地变化则转入冲突，不覆盖新工作。
pub async fn restore_from_cloud(st: &SyncShared) {
    let Some(client) = client(st).await else { return };
    let epoch = {
        let mut e = st.engine.write().await;
        if e.state == SyncStateView::Syncing {
            return;
        }
        e.bump_generation();
        e.epoch
    };
    {
        let mut e = st.engine.write().await;
        e.state = SyncStateView::Syncing;
        e.last_error = None;
    }
    emit_sync_state(st).await;
    let (original_items, original_focus) = {
        let core = st.core.lock().await;
        (core.store.items().to_vec(), core.store.focus_id())
    };
    match client.get_snapshot().await {
        Ok(cloud) => {
            if is_stale(st, epoch).await {
                return;
            }
            let changed = {
                let core = st.core.lock().await;
                core.store.items() != original_items || core.store.focus_id() != original_focus
            };
            if changed {
                prepare_conflict(st, cloud).await;
                return;
            }
            choose_cloud(st, &cloud).await;
        }
        Err(error) => {
            if is_stale(st, epoch).await {
                return;
            }
            if error.is_auth_failure() {
                handle_auth_lost(st).await;
            } else {
                let mut e = st.engine.write().await;
                e.state = SyncStateView::Failed;
                e.last_error = Some(error.display_message().to_string());
            }
            emit_sync_state(st).await;
        }
    }
}

// MARK: - 会话生命周期

pub async fn handle_auth_lost(st: &SyncShared) {
    session_reset(st).await;
    {
        let mut e = st.engine.write().await;
        e.state = SyncStateView::Unauthorized;
        e.last_error = Some(ApiError::Unauthorized.display_message().to_string());
    }
    emit_sync_state(st).await;
    let _ = try_emit(st, EVT_SESSION_LOST, ());
}

/// 登出/鉴权丢失：清空引擎会话态与凭据；本地事项保留、历史清除。
pub async fn session_reset(st: &SyncShared) {
    {
        let mut e = st.engine.write().await;
        e.bump_session();
    }
    {
        let mut auth = st.auth.lock().await;
        auth.logged_in = false;
        auth.is_authenticating = false;
        auth.client = None;
        auth.username = None;
        auth.server_url = None;
        auth.creds.clear();
    }
    {
        let mut core = st.core.lock().await;
        core.store.clear_history();
        core.meta.known_server_version = None;
        core.meta.dirty = false;
    }
    persist_all(st).await;
    emit_snapshot(st).await;
    emit_sync_state(st).await;
}

/// 登录成功后把会话内变更者身份写入 meta（owner 供归属判断）。
pub async fn claim_owner_public(st: &SyncShared) {
    let (server_url, username) = {
        let auth = st.auth.lock().await;
        (auth.server_url.clone(), auth.username.clone())
    };
    if let (Some(server_url), Some(username)) = (server_url, username) {
        let mut core = st.core.lock().await;
        core.meta.server_url = Some(server_url);
        core.meta.username = Some(username);
        drop(core);
        persist_all(st).await;
    }
}

/// 供 commands 单条落盘用：任何变更后的统一保存。
pub async fn save_now(st: &SyncShared) -> bool {
    persist_all(st).await
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::creds::CredentialStore;
    use crate::net::dto::AuthDto;
    use crate::net::test_server::{json_response, put_ok, snapshot, Resp, Scripted, SNAPSHOT_EMPTY};
    use std::future::Future;
    use std::time::Duration;

    async fn state_of(st: &AppState) -> SyncStateView {
        st.engine.read().await.state
    }
    async fn known_of(st: &AppState) -> Option<i64> {
        st.engine.read().await.known_version
    }

    async fn wait_until<F, Fut>(mut pred: F) -> bool
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = bool>,
    {
        for _ in 0..500 {
            if pred().await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        pred().await
    }

    fn temp_state() -> (Arc<AppState>, Arc<crate::creds::memory::MemoryStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let creds = Arc::new(crate::creds::memory::MemoryStore::default());
        let st = Arc::new(crate::state::AppState::new(
            dir.path().to_path_buf(),
            None,
            creds.clone() as Arc<dyn CredentialStore>,
        ));
        (st, creds, dir)
    }

    fn seed_tokens(creds: &Arc<crate::creds::memory::MemoryStore>) {
        creds
            .save(&AuthDto {
                access: "access-token".into(),
                refresh: "refresh-token".into(),
                expires_in: 900,
            })
            .unwrap();
    }

    async fn seed_item(st: &AppState, text: &str) {
        let mut core = st.core.lock().await;
        core.store.add(text, None, SystemClock.now()).unwrap();
    }

    async fn login_like(st: &AppState, url: &str, creds: Arc<crate::creds::memory::MemoryStore>) {
        let mut auth = st.auth.lock().await;
        auth.client = Some(ApiClient::new(url.to_string(), creds));
        auth.logged_in = true;
        auth.username = Some("tester".into());
        auth.server_url = Some(url.to_string());
        drop(auth);
        st.engine.write().await.bump_session();
    }

    #[tokio::test]
    async fn no_baseline_gets_then_puts_with_base0() {
        let (st, creds, _dir) = temp_state();
        seed_tokens(&creds);
        let server = Scripted::spawn(Box::new(|req| {
            if req.method == "GET" {
                json_response(200, SNAPSHOT_EMPTY)
            } else if req.method == "PUT" {
                json_response(200, &put_ok(1))
            } else {
                json_response(404, r#"{"code":"not_found","message":"no"}"#)
            }
        }));
        login_like(&st, &server.url, creds).await;
        seed_item(&st, "本地事项").await;

        flush(&st).await;
        assert!(
            wait_until(|| async { state_of(&st).await == SyncStateView::Synced }).await,
            "未进入 synced"
        );
        let reqs = server.requests();
        assert_eq!(reqs.iter().filter(|r| r.method == "GET").count(), 1);
        let puts: Vec<_> = reqs.iter().filter(|r| r.method == "PUT").collect();
        assert_eq!(puts.len(), 1);
        assert!(puts[0].body.contains("\"baseVersion\":0"), "{}", puts[0].body);
        assert!(puts[0].body.contains("本地事项"));
        assert_eq!(
            puts[0].bearer.as_deref(),
            Some("access-token"),
            "业务请求必须携带访问令牌"
        );
        assert_eq!(known_of(&st).await, Some(1));
        let core = st.core.lock().await;
        assert!(!core.meta.dirty);
        assert_eq!(core.meta.known_server_version, Some(1));
    }

    #[tokio::test]
    async fn put_409_enters_conflict_blocks_auto_then_choose_local_uses_latest_base() {
        let (st, creds, _dir) = temp_state();
        seed_tokens(&creds);
        let server = Scripted::spawn(Box::new(|req| {
            if req.method == "GET" {
                json_response(
                    200,
                    &snapshot(
                        r#"[{"id":"33333333-3333-3333-3333-333333333333","text":"cloud","done":false,"createdAt":"2026-09-08T00:00:00Z","dueDate":null,"updatedAt":"2026-09-08T00:00:00Z"}]"#,
                        2,
                        None,
                    ),
                )
            } else {
                json_response(
                    409,
                    r#"{"code":"snapshot_conflict","message":"snapshot changed on another device","currentVersion":2}"#,
                )
            }
        }));
        login_like(&st, &server.url, creds).await;
        seed_item(&st, "本地").await;

        flush(&st).await;
        assert!(
            wait_until(|| async { state_of(&st).await == SyncStateView::Conflict }).await,
            "应进入冲突态"
        );
        assert_eq!(known_of(&st).await, Some(2));
        {
            let e = st.engine.read().await;
            assert!(e.conflict.is_some());
            assert_eq!(e.conflict.as_ref().unwrap().version, 2);
        }

        // 冲突未决时自动上传被阻断：本地再改也不会新增 PUT。
        let puts_before = server.requests_matching(|r| r.method == "PUT");
        assert_eq!(puts_before, 1, "首次上传应已发生（409）");
        seed_item(&st, "冲突期间的本地记录").await;
        on_core_mutated(&st).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            server.requests_matching(|r| r.method == "PUT"),
            puts_before,
            "冲突未决不得自动上传"
        );

        // 选择本地：以云端最新版本 2 为基线上传，成功后版本 3。
        server.set_handler(Box::new(|req| {
            if req.method == "GET" {
                json_response(200, &snapshot("[]", 2, None))
            } else if req.method == "PUT" {
                json_response(200, &put_ok(3))
            } else {
                json_response(404, r#"{"code":"not_found","message":"no"}"#)
            }
        }));
        choose_local(&st).await;
        assert!(
            wait_until(|| async { state_of(&st).await == SyncStateView::Synced }).await,
            "选择本地后应 synced"
        );
        let reqs = server.requests();
        let puts: Vec<_> = reqs.iter().filter(|r| r.method == "PUT").collect();
        assert_eq!(puts.len(), puts_before + 1, "只应新增一次上传");
        let last_put = puts.last().unwrap();
        assert!(
            last_put.body.contains("\"baseVersion\":2"),
            "{}",
            last_put.body
        );
        assert_eq!(known_of(&st).await, Some(3));
        assert!(!st.core.lock().await.meta.dirty);
    }

    /// 计划 P3 门禁：结果未知的 PUT（响应丢失）不得被当作成功，也不得盲目覆盖。
    /// 服务器已应用首个 PUT（v2→v3）但断开连接；客户端置 failed、保留 dirty 与基线；
    /// 重试沿用原基线 → 409 → 冲突挂起，本地数据完整。
    #[tokio::test]
    async fn put_result_unknown_retries_same_base_then_conflicts_without_overwrite() {
        use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};
        let (st, creds, _dir) = temp_state();
        seed_tokens(&creds);

        let version = Arc::new(AtomicI64::new(2));
        let v_get = version.clone();
        let v_put = version.clone();
        let server = Scripted::spawn(Box::new(move |req| {
            if req.method == "GET" {
                let v = v_get.load(AtomicOrdering::SeqCst);
                if v >= 3 {
                    // 首个 PUT 已在服务端生效：云端出现“本地”内容，版本推进。
                    json_response(
                        200,
                        &snapshot(
                            r#"[{"id":"44444444-4444-4444-4444-444444444444","text":"已应用的本地事项","done":false,"createdAt":"2026-09-08T00:00:00Z","dueDate":null,"updatedAt":"2026-09-08T00:00:00Z"}]"#,
                            v,
                            None,
                        ),
                    )
                } else {
                    json_response(200, &snapshot("[]", v, None))
                }
            } else if req.method == "PUT" {
                let cur = v_put.load(AtomicOrdering::SeqCst);
                if cur == 2 {
                    v_put.store(3, AtomicOrdering::SeqCst); // 服务器已应用……
                    Resp { status: 0, body: String::new() } // ……但响应丢失
                } else {
                    json_response(
                        409,
                        &format!(
                            r#"{{"code":"snapshot_conflict","message":"changed","currentVersion":{cur}}}"#
                        ),
                    )
                }
            } else {
                json_response(404, r#"{"code":"not_found","message":"no"}"#)
            }
        }));
        login_like(&st, &server.url, creds).await;
        seed_item(&st, "本地-结果未知").await;
        // 模拟真实本地变更入口（seed 直插 store 不置脏；retry driver 依赖 dirty）。
        st.engine.write().await.dirty = true;
        st.core.lock().await.meta.dirty = true;

        flush(&st).await;
        assert!(
            wait_until(|| async { state_of(&st).await == SyncStateView::Failed }).await,
            "响应丢失应进入 failed 等待重试"
        );
        {
            let core = st.core.lock().await;
            assert!(core.meta.dirty, "结果未知不得清除 dirty");
            assert_eq!(core.meta.known_server_version, Some(2), "不得假设版本推进");
            assert!(
                core.store.items().iter().any(|i| i.text == "本地-结果未知"),
                "本地数据不得丢失"
            );
        }
        assert_eq!(known_of(&st).await, Some(2), "内存基线保持 2");

        // 重试：沿原基线重发 → 服务端 409 → 拉最新云端 → 冲突挂起。
        retry_tick(&st).await;
        assert!(
            wait_until(|| async { state_of(&st).await == SyncStateView::Conflict }).await,
            "重试应转入冲突（不盲写）"
        );
        let reqs = server.requests();
        let put_bodies: Vec<&String> = reqs
            .iter()
            .filter(|r| r.method == "PUT")
            .map(|r| &r.body)
            .collect();
        assert_eq!(put_bodies.len(), 2, "重试恰好一次 PUT");
        assert!(
            put_bodies[1].contains("\"baseVersion\":2"),
            "重试必须沿用原基线：{}",
            put_bodies[1]
        );
        {
            let e = st.engine.read().await;
            assert_eq!(e.conflict.as_ref().map(|c| c.version), Some(3));
        }
        let core = st.core.lock().await;
        assert!(
            core.store.items().iter().any(|i| i.text == "本地-结果未知"),
            "冲突挂起后本地数据仍完整"
        );
    }

    #[tokio::test]
    async fn choose_cloud_replaces_local_and_adopts_version() {
        let (st, creds, _dir) = temp_state();
        seed_tokens(&creds);
        // GET 返回云端候选（version 9，含 1 条）；PUT 因基线过期返回 409。
        let server = Scripted::spawn(Box::new(|req| {
            if req.method == "GET" {
                json_response(
                    200,
                    &snapshot(
                        r#"[{"id":"44444444-4444-4444-4444-444444444444","text":"cloud","done":false,"createdAt":"2026-09-08T00:00:00Z","dueDate":null,"updatedAt":"2026-09-08T00:00:00Z"}]"#,
                        9,
                        None,
                    ),
                )
            } else {
                json_response(
                    409,
                    r#"{"code":"snapshot_conflict","message":"conflict","currentVersion":9}"#,
                )
            }
        }));
        login_like(&st, &server.url, creds).await;
        seed_item(&st, "本地要丢的").await;
        flush(&st).await;
        let conflicted =
            wait_until(|| async { state_of(&st).await == SyncStateView::Conflict }).await;
        assert!(conflicted, "应进入冲突态");
        let cloud = { st.engine.read().await.conflict.clone().unwrap() };
        assert_eq!(cloud.version, 9);
        choose_cloud(&st, &cloud).await;
        let core = st.core.lock().await;
        assert_eq!(core.store.items().len(), 1, "本地已被云端替换");
        assert_eq!(core.store.items()[0].text, "cloud");
        assert_eq!(core.meta.known_server_version, Some(9));
        assert_eq!(core.store.undo_title(), None, "云端替换清空历史");
    }

    #[tokio::test]
    async fn auth_failure_clears_credentials_and_enters_unauthorized() {
        let (st, creds, _dir) = temp_state();
        seed_tokens(&creds);
        let server = Scripted::spawn(Box::new(|_req| {
            json_response(
                401,
                r#"{"code":"invalid_token","message":"invalid or expired token"}"#,
            )
        }));
        login_like(&st, &server.url, creds.clone()).await;
        seed_item(&st, "x").await;
        flush(&st).await;
        assert!(
            wait_until(|| async { state_of(&st).await == SyncStateView::Unauthorized }).await,
            "鉴权失败应进入 unauthorized"
        );
        assert!(creds.load().is_err(), "凭据应被清除");
        assert!(!st.auth.lock().await.logged_in);
    }

    #[tokio::test]
    async fn failure_enters_failed_and_retry_driver_recovers() {
        let (st, creds, _dir) = temp_state();
        seed_tokens(&creds);
        let server = Scripted::spawn(Box::new(|_req| {
            json_response(503, r#"{"code":"service_unavailable","message":"down"}"#)
        }));
        login_like(&st, &server.url, creds).await;
        seed_item(&st, "x").await;
        // 模拟“本地有未上传修改”的真实入口：置 dirty（retry driver 依赖它）。
        st.engine.write().await.dirty = true;
        st.core.lock().await.meta.dirty = true;
        st.engine.write().await.known_version = Some(0);
        flush(&st).await;
        assert!(
            wait_until(|| async { state_of(&st).await == SyncStateView::Failed }).await,
            "应进入 failed"
        );
        server.set_handler(Box::new(|_req| json_response(200, &put_ok(5))));
        retry_tick(&st).await;
        assert!(
            wait_until(|| async { state_of(&st).await == SyncStateView::Synced }).await,
            "重试驱动应恢复 synced"
        );
        assert_eq!(known_of(&st).await, Some(5));
    }

    #[tokio::test]
    async fn automatic_sync_off_keeps_local_then_manual_flush_works() {
        let (st, creds, _dir) = temp_state();
        seed_tokens(&creds);
        let server = Scripted::spawn(Box::new(|req| {
            if req.method == "GET" {
                json_response(200, SNAPSHOT_EMPTY)
            } else {
                json_response(200, &put_ok(7))
            }
        }));
        login_like(&st, &server.url, creds).await;
        st.core.lock().await.settings.automatic_sync = false;
        seed_item(&st, "仅本地").await;
        on_core_mutated(&st).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(server.requests_matching(|r| r.method == "PUT"), 0, "自动同步关闭不应上传");
        flush(&st).await;
        assert!(
            wait_until(|| async { state_of(&st).await == SyncStateView::Synced }).await,
            "手动同步应成功"
        );
        assert_eq!(server.requests_matching(|r| r.method == "PUT"), 1);
    }

    #[tokio::test]
    async fn session_reset_discards_inflight_result() {
        let (st, creds, _dir) = temp_state();
        seed_tokens(&creds);
        let server = Scripted::spawn(Box::new(|_req| {
            std::thread::sleep(Duration::from_millis(600));
            json_response(200, &put_ok(11))
        }));
        login_like(&st, &server.url, creds.clone()).await;
        seed_item(&st, "x").await;
        st.engine.write().await.known_version = Some(0);
        flush(&st).await;
        let started = wait_until(|| async { server.requests_matching(|r| r.method == "PUT") == 1 })
            .await;
        assert!(started, "PUT 应已发出");
        session_reset(&st).await;
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert!(creds.load().is_err());
        let e = st.engine.read().await;
        assert_eq!(e.state, SyncStateView::Idle);
        assert_eq!(e.known_version, None);
        assert_eq!(e.conflict, None);
        drop(e);
        assert_eq!(st.core.lock().await.meta.known_server_version, None);
    }

    #[tokio::test]
    async fn restart_resume_same_owner_dirty_uploads() {
        let (st, creds, _dir) = temp_state();
        seed_tokens(&creds);
        let server = Scripted::spawn(Box::new(|req| {
            if req.method == "GET" {
                json_response(200, &snapshot("[]", 4, None))
            } else {
                json_response(200, &put_ok(5))
            }
        }));
        login_like(&st, &server.url, creds).await;
        // 模拟“重启前”的持久化状态：同归属、有未上传修改、已知基线 4。
        {
            let mut core = st.core.lock().await;
            core.store.add("重启前的修改", None, SystemClock.now()).unwrap();
            core.meta.server_url = Some(server.url.clone());
            core.meta.username = Some("tester".into());
            core.meta.dirty = true;
            core.meta.known_server_version = Some(4);
        }
        st.engine.write().await.dirty = true;
        st.engine.write().await.known_version = Some(4);
        crate::auth::resume_after_restart(&st).await;
        assert!(
            wait_until(|| async { state_of(&st).await == SyncStateView::Synced }).await,
            "同归属重启应自动续传"
        );
        assert_eq!(server.requests_matching(|r| r.method == "PUT"), 1);
        assert_eq!(known_of(&st).await, Some(5));
    }

    #[tokio::test]
    async fn restart_resume_unowned_local_enters_conflict_without_upload() {
        let (st, creds, _dir) = temp_state();
        seed_tokens(&creds);
        let server = Scripted::spawn(Box::new(|req| {
            if req.method == "GET" {
                json_response(
                    200,
                    &snapshot(
                        r#"[{"id":"55555555-5555-5555-5555-555555555555","text":"cloud","done":false,"createdAt":"2026-09-08T00:00:00Z","dueDate":null,"updatedAt":"2026-09-08T00:00:00Z"}]"#,
                        6,
                        None,
                    ),
                )
            } else {
                json_response(200, &put_ok(7))
            }
        }));
        login_like(&st, &server.url, creds).await;
        seed_item(&st, "无归属本地数据").await;
        // meta 无归属（username/server_url 为空）→ 必须交用户选择。
        crate::auth::resume_after_restart(&st).await;
        assert!(
            wait_until(|| async { state_of(&st).await == SyncStateView::Conflict }).await,
            "无归属恢复应转冲突"
        );
        assert_eq!(
            server.requests_matching(|r| r.method == "PUT"),
            0,
            "无归属数据不得自动上传"
        );
    }

    #[tokio::test]
    async fn restore_with_midflight_edits_becomes_conflict_candidate() {
        let (st, creds, _dir) = temp_state();
        seed_tokens(&creds);
        // 云端恢复的 GET 故意放慢，期间本地发生编辑。
        let server = Scripted::spawn(Box::new(|req| {
            if req.method == "GET" {
                std::thread::sleep(Duration::from_millis(350));
                json_response(
                    200,
                    &snapshot(
                        r#"[{"id":"66666666-6666-6666-6666-666666666666","text":"cloud","done":false,"createdAt":"2026-09-08T00:00:00Z","dueDate":null,"updatedAt":"2026-09-08T00:00:00Z"}]"#,
                        9,
                        None,
                    ),
                )
            } else {
                json_response(200, &put_ok(10))
            }
        }));
        login_like(&st, &server.url, creds).await;
        seed_item(&st, "本地原始").await;
        let st2 = st.clone();
        let restore = tokio::spawn(async move { restore_from_cloud(&st2).await });
        // 等待 GET 已发出（恢复进行中）。
        assert!(
            wait_until(|| async { server.requests_matching(|r| r.method == "GET") >= 1 }).await,
            "恢复 GET 应已发出"
        );
        seed_item(&st, "恢复期间的本地编辑").await;
        let _ = restore.await;
        let e = st.engine.read().await;
        assert_eq!(e.state, SyncStateView::Conflict, "恢复期间编辑应转入冲突");
        assert_eq!(e.conflict.as_ref().map(|c| c.version), Some(9));
        drop(e);
        let core = st.core.lock().await;
        assert_eq!(core.store.items().len(), 2, "本地新工作必须被保留");
    }

    #[tokio::test]
    async fn resume_keeps_pending_conflict_without_any_request() {
        let (st, creds, _dir) = temp_state();
        seed_tokens(&creds);
        let server = Scripted::spawn(Box::new(|_req| {
            json_response(500, r#"{"code":"internal_error","message":"should not be called"}"#)
        }));
        login_like(&st, &server.url, creds).await;
        seed_item(&st, "本地").await;
        // 模拟“重启时仍有未决冲突”的持久化情景（状态为内存态，本用例直接置位）。
        st.engine.write().await.state = SyncStateView::Conflict;
        crate::auth::resume_after_restart(&st).await;
        assert_eq!(
            server.requests().len(),
            0,
            "未决冲突在重启后不得发起任何同步请求（更不得自动覆盖）"
        );
        assert_eq!(st.engine.read().await.state, SyncStateView::Conflict);
    }
}
