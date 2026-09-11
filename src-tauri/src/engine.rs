//! 单一快照同步协调器：同一网络流程串行，所有完成结果按会话与操作代次提交。
//! 用户编辑仅取消旧防抖，不使正在返回的 GET 丢失仲裁，也不让旧 PUT 清掉新 dirty。
use doing_core::clock::SystemClock;
use doing_core::data::{AccountOwner, CloudData, ConflictReason, PendingConflict, UploadMarker};
use doing_core::Clock;
#[cfg(test)]
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::Emitter;
use uuid::Uuid;

use crate::error::CommandError;
use crate::events::*;
use crate::net::client::ApiClient;
use crate::net::dto::{ApiError, ItemDto, SnapshotDto, SnapshotPutRequest};
use crate::state::{AppState, AuthInner, ConflictCandidate, CoreInner, EngineInner, RetryAction};
use crate::SyncShared;

pub const DEBOUNCE_MS: u64 = 2000;
pub const RETRY_BASE_MS: u64 = 2000;
pub const SAVE_ERROR_MESSAGE: &str =
    "本地保存失败：请检查磁盘空间、目录权限或数据文件版本；原有数据未被覆盖";
fn now_str() -> String {
    SystemClock
        .now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
pub fn try_emit<T: serde::Serialize + Clone>(st: &AppState, event: &str, payload: T) -> bool {
    st.try_handle()
        .is_some_and(|h| h.emit(event, payload).is_ok())
}
fn report_save_failure(st: &AppState) {
    let _ = try_emit(st, EVT_SAVE_FAILED, SAVE_ERROR_MESSAGE);
}

#[derive(Clone)]
struct SessionContext {
    generation: u64,
    id: Uuid,
    owner: AccountOwner,
    username: String,
    client: ApiClient,
}
impl SessionContext {
    fn matches(&self, e: &EngineInner, auth: &AuthInner) -> bool {
        e.session_generation == self.generation
            && auth.logged_in
            && auth.client.as_ref().and_then(ApiClient::session_id) == Some(self.id)
            && self.client.lease().is_some_and(|lease| lease.is_active())
    }
    fn live(&self, e: &EngineInner, auth: &AuthInner, operation: u64) -> bool {
        self.matches(e, auth) && e.operation == operation
    }
}
async fn context(st: &SyncShared) -> Option<SessionContext> {
    let e = st.engine.read().await;
    let auth = st.auth.lock().await;
    let client = auth.client.as_ref()?;
    let lease = client.lease()?;
    if !auth.logged_in || !lease.is_active() {
        return None;
    }
    Some(SessionContext {
        generation: e.session_generation,
        id: lease.id,
        owner: lease.identity.owner.clone(),
        username: lease.identity.username.clone(),
        client: client.clone(),
    })
}
fn candidate(pending: &PendingConflict) -> Option<ConflictCandidate> {
    let data = pending.cloud.as_ref()?;
    Some(ConflictCandidate {
        id: pending.candidate_id,
        owner: pending.owner.clone(),
        reason: pending.reason,
        snapshot: SnapshotDto {
            items: data.items.iter().map(ItemDto::from).collect(),
            focus_id: data.focus_id,
            version: data.version,
            updated_at: data.updated_at.clone(),
        },
    })
}
pub(crate) fn refresh_view(e: &mut EngineInner, core: &CoreInner, owner: Option<&AccountOwner>) {
    e.dirty = core.meta.dirty;
    e.known_version = owner
        .filter(|o| core.meta.belongs_to(o))
        .and(core.meta.known_server_version);
    let pending = core
        .meta
        .pending_conflict
        .as_ref()
        .filter(|p| Some(&p.owner) == owner);
    e.conflict = pending.and_then(candidate);
    e.state = if pending.is_some() {
        SyncStateView::Conflict
    } else if core.save_failed {
        SyncStateView::Failed
    } else if owner.is_none() {
        SyncStateView::Idle
    } else if core.meta.dirty {
        SyncStateView::Pending
    } else if e.known_version.is_some() {
        SyncStateView::Synced
    } else {
        SyncStateView::Idle
    };
}
pub fn sync_view(e: &EngineInner, revision: u64) -> SyncStateViewPayload {
    SyncStateViewPayload {
        session_generation: e.session_generation,
        event_revision: revision,
        state: e.state,
        last_sync_at: e.last_sync_at.clone(),
        last_error: e.last_error.clone(),
        conflict_cloud_count: e.conflict.as_ref().map(|c| c.items.len()),
        conflict_cloud_version: e.conflict.as_ref().map(|c| c.version.to_string()),
        conflict_id: e.conflict.as_ref().map(|c| c.id),
    }
}
pub async fn emit_sync_state(st: &SyncShared) {
    let e = st.engine.read().await;
    let revision = st.next_event_revision();
    let payload = sync_view(&e, revision);
    let conflict = e
        .conflict
        .as_ref()
        .map(|c| ConflictView::from(c, e.session_generation, revision));
    drop(e);
    let _ = try_emit(st, EVT_SYNC_STATE, payload);
    if let Some(conflict) = conflict {
        let _ = try_emit(st, EVT_CONFLICT, conflict);
    }
}
pub async fn emit_snapshot(st: &SyncShared) {
    let e = st.engine.read().await;
    let core = st.core.lock().await;
    let payload = SnapshotView::from_store(
        &core.store,
        core.save_failed,
        e.session_generation,
        st.next_event_revision(),
    );
    drop(core);
    drop(e);
    let _ = try_emit(st, EVT_SNAPSHOT, payload);
}

pub(crate) fn commit_core(st: &AppState, current: &mut CoreInner, mut next: CoreInner) -> bool {
    if current.migration.blocks_writes() {
        return false;
    }
    if st.repo.save(&next.to_data_file()).is_ok() {
        next.save_failed = false;
        *current = next;
        true
    } else {
        current.save_failed = true;
        false
    }
}
pub async fn persist_all(st: &SyncShared) -> bool {
    let mut core = st.core.lock().await;
    let saved = st.repo.save(&core.to_data_file()).is_ok();
    core.save_failed = !saved;
    drop(core);
    if !saved {
        report_save_failure(st);
    }
    saved
}
pub async fn save_now(st: &SyncShared) -> bool {
    persist_all(st).await
}
pub async fn mutate_core<F, R>(
    st: &SyncShared,
    changes_tasks: bool,
    f: F,
) -> (Result<R, doing_core::CoreError>, bool)
where
    F: FnOnce(&mut CoreInner) -> Result<R, doing_core::CoreError>,
{
    let mut e = st.engine.write().await;
    let auth = st.auth.lock().await;
    let owner = auth
        .client
        .as_ref()
        .and_then(ApiClient::lease)
        .filter(|lease| lease.is_active())
        .map(|l| l.identity.owner.clone());
    if changes_tasks && (!auth.logged_in || owner.is_none()) {
        return (Err(doing_core::CoreError::AuthenticationRequired), true);
    }
    let mut core = st.core.lock().await;
    let mut next = core.clone();
    if changes_tasks
        && core.store.is_empty()
        && !core.meta.dirty
        && core.meta.pending_conflict.is_none()
        && core.meta.uncertain_upload.is_none()
    {
        let owner = owner.as_ref().unwrap();
        if !next.meta.belongs_to(owner) {
            next.meta
                .claim(owner, auth.username.as_deref().unwrap_or_default());
            next.meta.known_server_version = None;
        }
    }
    let result = match f(&mut next) {
        Ok(value) => value,
        Err(error) => return (Err(error), true),
    };
    if changes_tasks {
        next.meta.dirty = true;
        next.meta.local_edit_revision = next.store.revision();
    }
    let saved = commit_core(st, &mut core, next);
    if saved {
        if changes_tasks {
            e.bump_generation();
        }
        refresh_view(&mut e, &core, owner.as_ref());
    }
    drop(core);
    drop(auth);
    drop(e);
    if !saved {
        report_save_failure(st);
    }
    (Ok(result), saved)
}
pub async fn on_core_mutated(st: &SyncShared) {
    emit_sync_state(st).await;
    maybe_auto_sync(st).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Intent {
    Automatic,
    Manual,
    Bootstrap,
    Restore,
    ConfirmLocal,
}
impl Intent {
    fn can_upload(self, core: &CoreInner) -> bool {
        matches!(self, Self::Manual | Self::ConfirmLocal)
            || (self != Self::Restore && core.settings.automatic_sync)
    }
    fn retry(self) -> RetryAction {
        if self == Self::Bootstrap {
            RetryAction::Bootstrap
        } else {
            RetryAction::Sync
        }
    }
}
async fn invalidate_for_session(st: &SyncShared, ctx: &SessionContext) -> Result<(), CommandError> {
    let mut e = st.engine.write().await;
    let auth = st.auth.lock().await;
    if !ctx.matches(&e, &auth) {
        return Err(ApiError::SessionChanged.into());
    }
    e.invalidate();
    Ok(())
}
pub async fn flush(st: &SyncShared) -> Result<(), CommandError> {
    let ctx = context(st)
        .await
        .ok_or_else(CommandError::authentication_required)?;
    // 手动请求不被之后的输入防抖取消，只受会话与串行网络流程约束。
    invalidate_for_session(st, &ctx).await?;
    let st = st.clone();
    tokio::spawn(async move {
        let _ = run_flow(&st, ctx, Intent::Manual, None).await;
    });
    Ok(())
}
pub async fn auto_flush(st: &SyncShared) {
    auto_flush_after(st, 0).await;
}
async fn maybe_auto_sync(st: &SyncShared) {
    auto_flush_after(st, DEBOUNCE_MS).await;
}
async fn auto_flush_after(st: &SyncShared, delay_ms: u64) {
    let Some(ctx) = context(st).await else {
        return;
    };
    let mut e = st.engine.write().await;
    let auth = st.auth.lock().await;
    if !ctx.matches(&e, &auth) {
        return;
    }
    let core = st.core.lock().await;
    let ready_conflict = core
        .meta
        .pending_conflict
        .as_ref()
        .is_some_and(|p| p.owner == ctx.owner && p.cloud.is_some());
    if !core.settings.automatic_sync || core.save_failed || ready_conflict || !core.meta.dirty {
        return;
    }
    let retry_delay = e
        .retry_at
        .map(|at| at.saturating_duration_since(Instant::now()))
        .unwrap_or_default();
    let delay = Duration::from_millis(delay_ms).max(retry_delay);
    let epoch = e.invalidate();
    let cancel = e.cancel_tx.subscribe();
    drop(core);
    drop(auth);
    drop(e);
    let st = st.clone();
    tokio::spawn(async move {
        if wait_guarded(cancel, epoch, delay).await {
            let _ = run_flow(&st, ctx, Intent::Automatic, Some(epoch)).await;
        }
    });
}
async fn wait_guarded(
    mut cancel: tokio::sync::watch::Receiver<u64>,
    epoch: u64,
    delay: Duration,
) -> bool {
    if *cancel.borrow() != epoch {
        return false;
    }
    tokio::time::timeout(delay, cancel.changed()).await.is_err()
}
pub async fn bootstrap(st: &SyncShared, generation: u64) {
    let Some(ctx) = context(st).await else {
        return;
    };
    if ctx.generation == generation {
        let _ = run_flow(st, ctx, Intent::Bootstrap, None).await;
    }
}

pub fn retry_delay(attempt: u32) -> Duration {
    Duration::from_millis(
        (RETRY_BASE_MS.saturating_mul(1_u64 << attempt.saturating_sub(1).min(6))).min(60_000),
    )
}
pub async fn retry_tick(st: &SyncShared) {
    let Some(ctx) = context(st).await else {
        return;
    };
    let mut e = st.engine.write().await;
    let auth = st.auth.lock().await;
    if !ctx.matches(&e, &auth) {
        return;
    }
    let core = st.core.lock().await;
    if !core.settings.automatic_sync
        || core.save_failed
        || e.retry_at.is_none_or(|at| at > Instant::now())
    {
        return;
    }
    let intent = match e.retry_action {
        Some(RetryAction::Bootstrap) => Intent::Bootstrap,
        Some(RetryAction::Sync) => Intent::Automatic,
        None => return,
    };
    e.retry_at = None;
    let epoch = e.invalidate();
    drop(core);
    drop(auth);
    drop(e);
    let st = st.clone();
    tokio::spawn(async move {
        let _ = run_flow(&st, ctx, intent, Some(epoch)).await;
    });
}
pub fn start_retry_driver(st: SyncShared) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            retry_tick(&st).await;
        }
    });
}

#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Restore,
    Mark,
    Upload,
    Conflict(ConflictReason),
}
fn same_snapshot(core: &CoreInner, cloud: &SnapshotDto) -> bool {
    let Ok((items, focus)) = cloud.to_local() else {
        return false;
    };
    core.store.focus_id() == focus
        && core.store.items().len() == items.len()
        && core.store.items().iter().zip(items).all(|(a, b)| {
            a.id == b.id
                && a.text == b.text
                && a.done == b.done
                && a.created_at.timestamp_millis() == b.created_at.timestamp_millis()
                && a.updated_at.timestamp_millis() == b.updated_at.timestamp_millis()
                && a.due_date.map(|d| d.timestamp_millis())
                    == b.due_date.map(|d| d.timestamp_millis())
        })
}
fn decide(
    core: &CoreInner,
    ctx: &SessionContext,
    cloud: &SnapshotDto,
    revision_before_get: u64,
    intent: Intent,
) -> Decision {
    if intent == Intent::Restore {
        return if core.store.revision() == revision_before_get {
            Decision::Restore
        } else {
            Decision::Conflict(ConflictReason::LocalChanged)
        };
    }
    if let Some(pending) = core
        .meta
        .pending_conflict
        .as_ref()
        .filter(|p| p.owner == ctx.owner)
    {
        return Decision::Conflict(pending.reason);
    }
    let empty = core.store.is_empty();
    if !core.meta.belongs_to(&ctx.owner) {
        return if empty && !core.meta.dirty {
            Decision::Restore
        } else {
            Decision::Conflict(ConflictReason::Ownership)
        };
    }
    if core.store.revision() != revision_before_get && !cloud.items.is_empty() {
        return Decision::Conflict(ConflictReason::LocalChanged);
    }
    if let Some(base) = core.meta.known_server_version {
        if cloud.version != base {
            if empty && !core.meta.dirty {
                return Decision::Restore;
            }
            return Decision::Conflict(if core.meta.uncertain_upload.is_some() {
                ConflictReason::UploadUncertain
            } else {
                ConflictReason::RemoteChanged
            });
        }
        if core.meta.dirty {
            Decision::Upload
        } else if same_snapshot(core, cloud) {
            Decision::Mark
        } else {
            Decision::Conflict(ConflictReason::Initial)
        }
    } else if empty && !core.meta.dirty {
        Decision::Restore
    } else if cloud.items.is_empty() {
        Decision::Upload
    } else {
        Decision::Conflict(ConflictReason::Initial)
    }
}
fn make_pending(
    ctx: &SessionContext,
    cloud: Option<CloudData>,
    reason: ConflictReason,
) -> PendingConflict {
    PendingConflict {
        candidate_id: Uuid::new_v4(),
        owner: ctx.owner.clone(),
        reason,
        cloud,
    }
}
fn cloud_data(cloud: &SnapshotDto) -> Result<CloudData, CommandError> {
    let (items, focus_id) = cloud.to_local().map_err(|_| "服务器快照格式无效")?;
    if cloud.version < 0 {
        return Err("服务器快照版本无效".into());
    }
    Ok(CloudData {
        items,
        focus_id,
        version: cloud.version,
        updated_at: cloud.updated_at.clone(),
    })
}
fn archive_if_foreign(
    st: &SyncShared,
    core: &CoreInner,
    owner: &AccountOwner,
) -> Result<(), CommandError> {
    if (!core.store.is_empty() || core.meta.dirty)
        && !core.meta.belongs_to(owner)
        && st.repo.archive_current().is_err()
    {
        return Err(CommandError::persistence(
            "原账号数据备份失败，未切换归属或替换事项",
        ));
    }
    Ok(())
}
fn success(e: &mut EngineInner) {
    e.retry_attempt = 0;
    e.retry_at = None;
    e.retry_action = None;
    e.last_error = None;
}

async fn run_flow(
    st: &SyncShared,
    ctx: SessionContext,
    intent: Intent,
    epoch: Option<u64>,
) -> Result<(), CommandError> {
    let serial = st.engine.read().await.push_lock.clone();
    let _serial = serial.lock().await;
    let (operation, revision, get_first) = {
        let mut e = st.engine.write().await;
        let auth = st.auth.lock().await;
        let core = st.core.lock().await;
        if !ctx.matches(&e, &auth) || epoch.is_some_and(|v| v != e.epoch) {
            return Ok(());
        }
        if core.save_failed {
            return Err(CommandError::persistence(SAVE_ERROR_MESSAGE));
        }
        if intent == Intent::Automatic && !core.settings.automatic_sync {
            return Ok(());
        }
        let pending = core
            .meta
            .pending_conflict
            .as_ref()
            .filter(|p| p.owner == ctx.owner);
        if pending.is_some_and(|p| p.cloud.is_some()) && intent != Intent::Restore {
            refresh_view(&mut e, &core, Some(&ctx.owner));
            drop(core);
            drop(auth);
            drop(e);
            emit_sync_state(st).await;
            return Ok(());
        }
        e.operation += 1;
        e.state = SyncStateView::Syncing;
        e.last_error = None;
        e.retry_at = None;
        let get_first = matches!(intent, Intent::Bootstrap | Intent::Restore)
            || !core.meta.belongs_to(&ctx.owner)
            || core.meta.known_server_version.is_none()
            || core.meta.pending_conflict.is_some()
            || core.meta.uncertain_upload.is_some()
            || (intent == Intent::Manual && !core.meta.dirty);
        (e.operation, core.store.revision(), get_first)
    };
    emit_sync_state(st).await;
    if get_first {
        let cloud = match ctx.client.get_snapshot().await {
            Ok(cloud) => cloud,
            Err(error) => {
                if intent == Intent::Restore {
                    mark_needs_candidate(st, &ctx, operation, ConflictReason::Restore).await;
                }
                fail(st, &ctx, operation, error.clone(), intent.retry()).await;
                return Err(error.into());
            }
        };
        let decision = {
            let mut e = st.engine.write().await;
            let auth = st.auth.lock().await;
            let mut core = st.core.lock().await;
            if !ctx.live(&e, &auth, operation) {
                return Ok(());
            }
            let decision = decide(&core, &ctx, &cloud, revision, intent);
            let mut next = core.clone();
            match decision {
                Decision::Conflict(reason) => {
                    next.meta.pending_conflict =
                        Some(make_pending(&ctx, Some(cloud_data(&cloud)?), reason));
                    next.meta.uncertain_upload = None;
                    // 保留最后确认基线，候选版本只在明确选择本地时才能用于 PUT。
                }
                Decision::Restore => {
                    if let Err(message) = archive_if_foreign(st, &core, &ctx.owner) {
                        e.state = SyncStateView::Failed;
                        e.last_error = Some(message.message.clone());
                        drop(core);
                        drop(auth);
                        drop(e);
                        emit_sync_state(st).await;
                        return Err(message);
                    }
                    let data = cloud_data(&cloud)?;
                    next.store.replace_all(data.items, data.focus_id);
                    next.meta.claim(&ctx.owner, &ctx.username);
                    next.meta.known_server_version = Some(cloud.version);
                    next.meta.dirty = false;
                    next.meta.pending_conflict = None;
                    next.meta.uncertain_upload = None;
                }
                Decision::Mark => {
                    next.meta.known_server_version = Some(cloud.version);
                    next.meta.uncertain_upload = None;
                }
                Decision::Upload => {
                    next.meta.known_server_version = Some(cloud.version);
                    next.meta.dirty = true;
                    next.meta.uncertain_upload = None;
                }
            }
            let saved = commit_core(st, &mut core, next);
            refresh_view(&mut e, &core, Some(&ctx.owner));
            if !saved {
                e.last_error = Some(SAVE_ERROR_MESSAGE.into());
                drop(core);
                drop(auth);
                drop(e);
                report_save_failure(st);
                emit_sync_state(st).await;
                return Err(CommandError::persistence(SAVE_ERROR_MESSAGE));
            }
            if decision != Decision::Upload {
                success(&mut e);
            }
            if matches!(decision, Decision::Mark | Decision::Restore) {
                e.last_sync_at = Some(now_str());
            }
            decision
        };
        emit_snapshot(st).await;
        emit_sync_state(st).await;
        if decision != Decision::Upload {
            return Ok(());
        }
    }
    upload(st, &ctx, operation, intent).await
}

async fn upload(
    st: &SyncShared,
    ctx: &SessionContext,
    operation: u64,
    intent: Intent,
) -> Result<(), CommandError> {
    let (request, marker) = {
        let mut e = st.engine.write().await;
        let auth = st.auth.lock().await;
        let mut core = st.core.lock().await;
        if !ctx.live(&e, &auth, operation) {
            return Ok(());
        }
        if !core.meta.belongs_to(&ctx.owner)
            || core.meta.pending_conflict.is_some()
            || core.meta.uncertain_upload.is_some()
        {
            return Err("同步基线或数据归属尚未确认".into());
        }
        if core.save_failed {
            return Err(CommandError::persistence(SAVE_ERROR_MESSAGE));
        }
        if !intent.can_upload(&core) || !core.meta.dirty {
            refresh_view(&mut e, &core, Some(&ctx.owner));
            drop(core);
            drop(auth);
            drop(e);
            emit_sync_state(st).await;
            return Ok(());
        }
        let base = core
            .meta
            .known_server_version
            .ok_or("尚未取得云端版本基线")?;
        let request = SnapshotPutRequest {
            items: core.store.items().iter().map(ItemDto::from).collect(),
            focus_id: core.store.focus_id(),
            base_version: base,
        };
        let marker = UploadMarker {
            id: Uuid::new_v4(),
            owner: ctx.owner.clone(),
            base_version: base,
            revision: core.store.revision(),
        };
        let mut next = core.clone();
        next.meta.uncertain_upload = Some(marker.clone());
        if !commit_core(st, &mut core, next) {
            e.state = SyncStateView::Failed;
            e.last_error = Some(SAVE_ERROR_MESSAGE.into());
            drop(core);
            drop(auth);
            drop(e);
            report_save_failure(st);
            emit_sync_state(st).await;
            return Err(CommandError::persistence(SAVE_ERROR_MESSAGE));
        }
        e.state = SyncStateView::Syncing;
        (request, marker)
    };
    match ctx.client.put_snapshot(&request).await {
        Ok(response) => acknowledge(st, ctx, operation, &marker, response.version).await,
        Err(error) if error.is_conflict() => {
            if mark_needs_candidate(st, ctx, operation, ConflictReason::RemoteChanged).await {
                match ctx.client.get_snapshot().await {
                    Ok(cloud) => {
                        store_candidate(st, ctx, operation, cloud, ConflictReason::RemoteChanged)
                            .await?;
                    }
                    Err(error) => {
                        fail(st, ctx, operation, error.clone(), RetryAction::Sync).await;
                        return Err(error.into());
                    }
                }
            }
            Ok(())
        }
        Err(error) => {
            fail(st, ctx, operation, error.clone(), RetryAction::Sync).await;
            Err(error.into())
        }
    }
}
async fn acknowledge(
    st: &SyncShared,
    ctx: &SessionContext,
    operation: u64,
    marker: &UploadMarker,
    version: i64,
) -> Result<(), CommandError> {
    let mut e = st.engine.write().await;
    let auth = st.auth.lock().await;
    let mut core = st.core.lock().await;
    if !ctx.live(&e, &auth, operation)
        || !core.meta.belongs_to(&ctx.owner)
        || core.meta.uncertain_upload.as_ref().map(|m| m.id) != Some(marker.id)
    {
        return Ok(());
    }
    let mut next = core.clone();
    next.meta.known_server_version = Some(version);
    next.meta.uncertain_upload = None;
    if core.meta.local_edit_revision <= marker.revision {
        next.meta.dirty = false;
    }
    let saved = commit_core(st, &mut core, next);
    refresh_view(&mut e, &core, Some(&ctx.owner));
    if saved {
        success(&mut e);
        e.last_sync_at = Some(now_str());
    } else {
        e.last_error = Some(SAVE_ERROR_MESSAGE.into());
    }
    drop(core);
    drop(auth);
    drop(e);
    if !saved {
        report_save_failure(st);
    }
    emit_sync_state(st).await;
    if saved {
        Ok(())
    } else {
        Err(CommandError::persistence(SAVE_ERROR_MESSAGE))
    }
}
async fn mark_needs_candidate(
    st: &SyncShared,
    ctx: &SessionContext,
    operation: u64,
    reason: ConflictReason,
) -> bool {
    let mut e = st.engine.write().await;
    let auth = st.auth.lock().await;
    let mut core = st.core.lock().await;
    if !ctx.live(&e, &auth, operation) {
        return false;
    }
    let mut next = core.clone();
    next.meta.pending_conflict = Some(make_pending(ctx, None, reason));
    next.meta.uncertain_upload = None;
    let saved = commit_core(st, &mut core, next);
    refresh_view(&mut e, &core, Some(&ctx.owner));
    if !saved {
        e.last_error = Some(SAVE_ERROR_MESSAGE.into());
    }
    drop(core);
    drop(auth);
    drop(e);
    if !saved {
        report_save_failure(st);
    }
    emit_sync_state(st).await;
    saved
}
async fn store_candidate(
    st: &SyncShared,
    ctx: &SessionContext,
    operation: u64,
    cloud: SnapshotDto,
    reason: ConflictReason,
) -> Result<(), CommandError> {
    let data = cloud_data(&cloud)?;
    let mut e = st.engine.write().await;
    let auth = st.auth.lock().await;
    let mut core = st.core.lock().await;
    if !ctx.live(&e, &auth, operation) {
        return Ok(());
    }
    let mut next = core.clone();
    next.meta.pending_conflict = Some(make_pending(ctx, Some(data), reason));
    next.meta.uncertain_upload = None;
    let saved = commit_core(st, &mut core, next);
    refresh_view(&mut e, &core, Some(&ctx.owner));
    if saved {
        success(&mut e);
    } else {
        e.last_error = Some(SAVE_ERROR_MESSAGE.into());
    }
    drop(core);
    drop(auth);
    drop(e);
    if !saved {
        report_save_failure(st);
    }
    emit_sync_state(st).await;
    if saved {
        Ok(())
    } else {
        Err(CommandError::persistence(SAVE_ERROR_MESSAGE))
    }
}
async fn fail(
    st: &SyncShared,
    ctx: &SessionContext,
    operation: u64,
    error: ApiError,
    retry: RetryAction,
) {
    if error == ApiError::SessionChanged {
        return;
    }
    if error.is_auth_failure() {
        crate::auth::lose_session(st, ctx.generation, ctx.id, &error).await;
        return;
    }
    let mut e = st.engine.write().await;
    let auth = st.auth.lock().await;
    let core = st.core.lock().await;
    if !ctx.live(&e, &auth, operation) {
        return;
    }
    e.state = if core
        .meta
        .pending_conflict
        .as_ref()
        .is_some_and(|p| p.owner == ctx.owner)
    {
        SyncStateView::Conflict
    } else {
        SyncStateView::Failed
    };
    e.last_error = Some(error.display_message().to_owned());
    e.retry_attempt = e.retry_attempt.saturating_add(1);
    e.retry_at = Some(Instant::now() + retry_delay(e.retry_attempt));
    e.retry_action = Some(retry);
    drop(core);
    drop(auth);
    drop(e);
    emit_sync_state(st).await;
}

pub async fn restore_from_cloud(st: &SyncShared) -> Result<(), CommandError> {
    let ctx = context(st)
        .await
        .ok_or_else(CommandError::authentication_required)?;
    invalidate_for_session(st, &ctx).await?;
    run_flow(st, ctx, Intent::Restore, None).await
}
fn verify_choice<'a>(
    core: &'a CoreInner,
    ctx: &SessionContext,
    id: Uuid,
    version: i64,
) -> Result<&'a CloudData, CommandError> {
    let pending = core
        .meta
        .pending_conflict
        .as_ref()
        .ok_or("没有待处理的冲突")?;
    if pending.owner != ctx.owner || pending.candidate_id != id {
        return Err(CommandError::new(
            "conflictChanged",
            "冲突候选或账号已变化，请重新查看后确认",
            false,
        ));
    }
    let cloud = pending
        .cloud
        .as_ref()
        .ok_or("尚未取得云端候选，请重试同步")?;
    if cloud.version != version {
        return Err(CommandError::new(
            "conflictChanged",
            "云端候选版本已变化，请重新确认",
            false,
        ));
    }
    Ok(cloud)
}
pub async fn choose_local(st: &SyncShared, id: Uuid, version: i64) -> Result<(), CommandError> {
    let ctx = context(st)
        .await
        .ok_or_else(CommandError::authentication_required)?;
    let mut e = st.engine.write().await;
    let auth = st.auth.lock().await;
    let mut core = st.core.lock().await;
    if !ctx.matches(&e, &auth) {
        return Err(ApiError::SessionChanged.into());
    }
    verify_choice(&core, &ctx, id, version)?;
    archive_if_foreign(st, &core, &ctx.owner)?;
    let mut next = core.clone();
    next.meta.claim(&ctx.owner, &ctx.username);
    next.meta.known_server_version = Some(version);
    next.meta.dirty = true;
    next.meta.pending_conflict = None;
    next.meta.uncertain_upload = None;
    if !commit_core(st, &mut core, next) {
        drop(core);
        drop(auth);
        drop(e);
        report_save_failure(st);
        return Err(CommandError::persistence(SAVE_ERROR_MESSAGE));
    }
    e.operation += 1;
    e.invalidate();
    success(&mut e);
    refresh_view(&mut e, &core, Some(&ctx.owner));
    drop(core);
    drop(auth);
    drop(e);
    emit_sync_state(st).await;
    let st = st.clone();
    tokio::spawn(async move {
        let _ = run_flow(&st, ctx, Intent::ConfirmLocal, None).await;
    });
    Ok(())
}
pub async fn choose_cloud(st: &SyncShared, id: Uuid, version: i64) -> Result<(), CommandError> {
    let ctx = context(st)
        .await
        .ok_or_else(CommandError::authentication_required)?;
    let mut e = st.engine.write().await;
    let auth = st.auth.lock().await;
    let mut core = st.core.lock().await;
    if !ctx.matches(&e, &auth) {
        return Err(ApiError::SessionChanged.into());
    }
    let cloud = verify_choice(&core, &ctx, id, version)?.clone();
    archive_if_foreign(st, &core, &ctx.owner)?;
    let mut next = core.clone();
    next.store.replace_all(cloud.items, cloud.focus_id);
    next.meta.claim(&ctx.owner, &ctx.username);
    next.meta.known_server_version = Some(version);
    next.meta.dirty = false;
    next.meta.pending_conflict = None;
    next.meta.uncertain_upload = None;
    if !commit_core(st, &mut core, next) {
        drop(core);
        drop(auth);
        drop(e);
        report_save_failure(st);
        return Err(CommandError::persistence(SAVE_ERROR_MESSAGE));
    }
    e.operation += 1;
    e.invalidate();
    success(&mut e);
    refresh_view(&mut e, &core, Some(&ctx.owner));
    e.last_sync_at = Some(now_str());
    drop(core);
    drop(auth);
    drop(e);
    emit_snapshot(st).await;
    emit_sync_state(st).await;
    crate::reminder::refresh(st).await;
    Ok(())
}
pub async fn defer_conflict(st: &SyncShared) -> Result<(), CommandError> {
    let ctx = context(st)
        .await
        .ok_or_else(CommandError::authentication_required)?;
    let mut e = st.engine.write().await;
    let auth = st.auth.lock().await;
    let core = st.core.lock().await;
    if !ctx.matches(&e, &auth)
        || !core
            .meta
            .pending_conflict
            .as_ref()
            .is_some_and(|p| p.owner == ctx.owner)
    {
        return Err("没有待处理的冲突".into());
    }
    refresh_view(&mut e, &core, Some(&ctx.owner));
    drop(core);
    drop(auth);
    drop(e);
    emit_sync_state(st).await;
    Ok(())
}

#[cfg(test)]
async fn prepare_conflict(st: &SyncShared, cloud: SnapshotDto) {
    let ctx = context(st).await.expect("测试必须建立真实会话");
    let operation = st.engine.read().await.operation;
    store_candidate(st, &ctx, operation, cloud, ConflictReason::RemoteChanged)
        .await
        .unwrap();
}
#[cfg(test)]
pub async fn session_reset(st: &SyncShared) {
    crate::auth::reset_for_test(st).await;
}

#[cfg(test)]
mod tests;
