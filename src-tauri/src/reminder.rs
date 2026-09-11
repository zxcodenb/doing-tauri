//! 到期提醒：先由系统接受提交，再记录已提醒；失败保留资格，稳定 ID 降低重复。
//! OS 投递与文件提交无法跨崩溃严格 exactly-once；文件提交失败后可能以同 ID 再次提交。
use crate::error::CommandError;
use crate::{desktop, engine, SyncShared};
use doing_core::clock::SystemClock;
use doing_core::data::AccountOwner;
use doing_core::Clock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use ts_rs::TS;
use uuid::Uuid;
pub(crate) mod routes;

const TICK_SECONDS: u64 = 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationTarget {
    pub item_id: Uuid,
    pub owner: AccountOwner,
    due_millis: i64,
}
#[derive(Clone)]
struct DueNotification {
    target: NotificationTarget,
    id: Uuid,
    text: String,
    sound: bool,
    session_generation: u64,
    session_id: Uuid,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeliveryError {
    Unavailable,
    PermissionDenied,
}
impl DeliveryError {
    fn message(self) -> &'static str {
        match self {
            Self::Unavailable => "系统通知暂时不可用，提醒资格已保留",
            Self::PermissionDenied => {
                "尚未获得系统通知权限，提醒资格已保留；可在系统设置中允许通知"
            }
        }
    }
}
trait NotificationSink: Send + Sync {
    /// Ok 只表示 OS API 接受提交，不声称通知一定已展示给用户。
    fn submit(&self, notification: &DueNotification) -> Result<(), DeliveryError>;
}
struct NativeSink {
    native: Option<Arc<doing_notifications::Native>>,
}
impl NotificationSink for NativeSink {
    fn submit(&self, notification: &DueNotification) -> Result<(), DeliveryError> {
        self.native
            .as_ref()
            .ok_or(DeliveryError::Unavailable)?
            .submit(doing_notifications::Request {
                id: notification.id,
                title: "已截止",
                body: &notification.text,
                sound: notification.sound,
            })
            .map_err(|error| {
                if error == doing_notifications::Error::PermissionDenied {
                    DeliveryError::PermissionDenied
                } else {
                    DeliveryError::Unavailable
                }
            })
    }
}

pub const ACTIVATION_SCHEME: &str = "doing-dev-notification";
pub fn install_native(st: &SyncShared, application_id: String) {
    let weak = Arc::downgrade(st);
    let activated = Arc::new(move |id| {
        if let Some(st) = weak.upgrade() {
            receive_activation(&st, id);
        }
    });
    match doing_notifications::Native::new(
        doing_notifications::Identity {
            application_id,
            activation_scheme: ACTIVATION_SCHEME.into(),
        },
        activated,
    ) {
        Ok(native) => *st.native_notifications.write().unwrap() = Some(Arc::new(native)),
        Err(error) => *st.notification_setup_error.lock().unwrap() = Some(error),
    }
}

/// Native callbacks synchronously persist first, before completing the OS callback. The
/// main workspace fetches the queue after its listeners/rendering are ready and then ACKs.
pub fn receive_activation(st: &SyncShared, activation: doing_notifications::Activation) {
    drop(dispatch_activation(st, activation, |st| async move {
        desktop::present_main(&st).await;
    }));
}
fn dispatch_activation<F, Fut>(
    st: &SyncShared,
    activation: doing_notifications::Activation,
    present: F,
) -> tauri::async_runtime::JoinHandle<()>
where
    F: FnOnce(SyncShared) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    // This write is synchronous: completing the OS callback cannot race durable enqueueing.
    let result = match activation {
        doing_notifications::Activation::Route(id) => st.notification_routes.activate(id),
        doing_notifications::Activation::OpenWorkspace => Ok(false),
    };
    let st = st.clone();
    tauri::async_runtime::spawn(async move {
        // Consumed/unknown/legacy clicks still open the login-gated workspace, but never
        // fabricate a target. Missing routes must not make a valid OS click disappear.
        present(st.clone()).await;
        if let Err(error) = result {
            let _ = engine::try_emit(&st, crate::events::EVT_SAVE_FAILED, error.message);
        }
    })
}

#[derive(Clone, Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub enum NotificationPermission {
    Granted,
    Denied,
    NotDetermined,
    Unavailable,
}
#[derive(Clone, Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct NotificationPermissionView {
    pub status: NotificationPermission,
    pub error: Option<String>,
}
pub async fn permission_view(
    st: &SyncShared,
    request: bool,
) -> Result<NotificationPermissionView, CommandError> {
    let native = st.native_notifications.read().unwrap().clone();
    let setup_error = *st.notification_setup_error.lock().unwrap();
    let result = match native {
        Some(native) => tokio::task::spawn_blocking(move || native.permission(request))
            .await
            .unwrap_or(Err(doing_notifications::Error::Unavailable)),
        None => Err(setup_error.unwrap_or(doing_notifications::Error::Unavailable)),
    };
    match result {
        Ok(value) => Ok(NotificationPermissionView {
            status: match value {
                doing_notifications::Permission::Granted => NotificationPermission::Granted,
                doing_notifications::Permission::Denied => NotificationPermission::Denied,
                doing_notifications::Permission::NotDetermined => {
                    NotificationPermission::NotDetermined
                }
            },
            error: None,
        }),
        Err(error) => {
            let message = match error {
                doing_notifications::Error::BundleRequired => "系统通知需要以本应用身份运行的 macOS 应用包；未打包开发进程不会借用 Terminal 身份",
                doing_notifications::Error::Timeout => "系统通知操作尚未确认，请稍后重新查询权限；未把超时当作拒绝",
                _ => "无法查询系统通知状态，请检查应用安装身份和系统通知设置",
            };
            if request {
                Err(CommandError::new("notificationUnavailable", message, true))
            } else {
                Ok(NotificationPermissionView {
                    status: NotificationPermission::Unavailable,
                    error: Some(message.into()),
                })
            }
        }
    }
}

pub fn start_ticker(state: SyncShared) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(TICK_SECONDS)).await;
            refresh(&state).await;
        }
    });
}
pub async fn refresh(st: &SyncShared) {
    let native = st.native_notifications.read().unwrap().clone();
    refresh_with(st, Arc::new(NativeSink { native })).await;
}

fn reserve_id(
    st: &crate::state::AppState,
    target: &NotificationTarget,
) -> Result<Uuid, CommandError> {
    st.notification_routes.reserve(target)
}
async fn refresh_with(st: &SyncShared, sink: Arc<dyn NotificationSink>) {
    // 启动、分钟 tick、任务与设置触发可并发到达，但不能重复提交同一轮资格。
    let _serial = st.reminder_lock.lock().await;
    let due = {
        let e = st.engine.read().await;
        let auth = st.auth.lock().await;
        let core = st.core.lock().await;
        let Some(lease) = auth.client.as_ref().and_then(|client| client.lease()) else {
            return;
        };
        if !auth.logged_in
            || core.migration.blocks_writes()
            || !lease.is_active()
            || !core.settings.notifications_enabled
            || !core.meta.belongs_to(&lease.identity.owner)
        {
            return;
        }
        let now = SystemClock.now();
        let notified = core.store.notified_set();
        core.store
            .items()
            .iter()
            .filter(|item| {
                !item.done
                    && item.due_date.is_some_and(|date| date <= now)
                    && !notified.contains(&item.id)
            })
            .map(|item| {
                let target = NotificationTarget {
                    item_id: item.id,
                    owner: lease.identity.owner.clone(),
                    due_millis: item.due_date.unwrap().timestamp_millis(),
                };
                DueNotification {
                    id: Uuid::nil(),
                    target,
                    text: item.text.clone(),
                    sound: core.settings.notification_sound,
                    session_generation: e.session_generation,
                    session_id: lease.id,
                }
            })
            .collect::<Vec<_>>()
    };
    for mut notification in due {
        // 每次提交前再检查当前资格，列表前面的慢投递不能让后面已删除/延期的任务仍被发送。
        if !still_eligible(st, &notification).await {
            continue;
        }
        notification.id = match reserve_id(st, &notification.target) {
            Ok(id) => id,
            Err(error) => {
                let _ = engine::try_emit(st, crate::events::EVT_SAVE_FAILED, error.message);
                continue;
            }
        };
        if !still_eligible(st, &notification).await {
            continue;
        }
        let sender = sink.clone();
        let request = notification.clone();
        let delivery = tokio::task::spawn_blocking(move || sender.submit(&request))
            .await
            .unwrap_or(Err(DeliveryError::Unavailable));
        if let Err(error) = delivery {
            if !still_eligible(st, &notification).await {
                continue;
            }
            let message = error.message();
            let changed = {
                let mut last = st.last_notification_error.lock().unwrap();
                let changed = *last != Some(message);
                *last = Some(message);
                changed
            };
            if changed {
                let _ = engine::try_emit(st, crate::events::EVT_SAVE_FAILED, message);
            }
            continue;
        }
        *st.last_notification_error.lock().unwrap() = None;
        let e = st.engine.read().await;
        let auth = st.auth.lock().await;
        let mut core = st.core.lock().await;
        if !eligible(&notification, &e, &auth, &core) {
            continue;
        }
        let mut next = core.clone();
        next.store
            .mark_notified(notification.target.item_id, SystemClock.now());
        let saved = engine::commit_core(st, &mut core, next);
        drop(core);
        drop(auth);
        drop(e);
        if !saved {
            let _ = engine::try_emit(
                st,
                crate::events::EVT_SAVE_FAILED,
                engine::SAVE_ERROR_MESSAGE,
            );
        }
        engine::emit_snapshot(st).await;
    }
    desktop::refresh_tray(st).await;
}
fn eligible(
    notification: &DueNotification,
    e: &crate::state::EngineInner,
    auth: &crate::state::AuthInner,
    core: &crate::state::CoreInner,
) -> bool {
    auth.logged_in
        && !core.migration.blocks_writes()
        && e.session_generation == notification.session_generation
        && auth
            .client
            .as_ref()
            .and_then(|client| client.lease())
            .is_some_and(|lease| lease.id == notification.session_id && lease.is_active())
        && core.settings.notifications_enabled
        && core.meta.belongs_to(&notification.target.owner)
        && !core
            .store
            .notified_due_ids()
            .contains(&notification.target.item_id)
        && core
            .store
            .find(notification.target.item_id)
            .is_some_and(|item| {
                !item.done
                    && item.due_date.is_some_and(|date| {
                        date.timestamp_millis() == notification.target.due_millis
                            && date <= SystemClock.now()
                    })
            })
}
async fn still_eligible(st: &SyncShared, notification: &DueNotification) -> bool {
    let e = st.engine.read().await;
    let auth = st.auth.lock().await;
    let core = st.core.lock().await;
    eligible(notification, &e, &auth, &core)
}

/// A pending activation survives login and renderer startup. It does not grant authority:
/// every read and ACK checks the live lease, data owner and (for ACK) the UI session generation.
pub async fn next_notification(
    st: &SyncShared,
) -> Result<Option<crate::events::ScrollTargetView>, CommandError> {
    let e = st.engine.read().await;
    let auth = st.auth.lock().await;
    let core = st.core.lock().await;
    let Some(lease) = auth.client.as_ref().and_then(|c| c.lease()) else {
        return Ok(None);
    };
    if !auth.logged_in
        || !lease.is_active()
        || core.migration.blocks_writes()
        || !core.meta.belongs_to(&lease.identity.owner)
    {
        return Ok(None);
    }
    for route in st.notification_routes.pending()? {
        if route.target.owner != lease.identity.owner {
            continue;
        }
        if !core.store.has_item(route.target.item_id) {
            // The click has already opened the panel. Deleted items need no phantom selection.
            st.notification_routes.acknowledge(route.id)?;
            continue;
        }
        return Ok(Some(crate::events::ScrollTargetView {
            notification_id: route.id,
            item_id: route.target.item_id,
            session_generation: e.session_generation,
            event_revision: st.next_event_revision(),
        }));
    }
    Ok(None)
}
pub async fn acknowledge_notification(
    st: &SyncShared,
    id: Uuid,
    generation: u64,
) -> Result<(), CommandError> {
    let e = st.engine.read().await;
    let auth = st.auth.lock().await;
    let core = st.core.lock().await;
    let lease = auth.client.as_ref().and_then(|c| c.lease());
    if e.session_generation != generation
        || !auth.logged_in
        || !lease.is_some_and(|l| l.is_active())
    {
        return Err(CommandError::new(
            "sessionChanged",
            "会话已变化，通知定位未确认",
            false,
        ));
    }
    if core.migration.blocks_writes() {
        return Err(CommandError::new(
            "migrationPending",
            "请先处理待恢复迁移",
            true,
        ));
    }
    if let Some(route) = st
        .notification_routes
        .pending()?
        .into_iter()
        .find(|r| r.id == id)
    {
        if lease.is_none_or(|l| l.identity.owner != route.target.owner)
            || !core.meta.belongs_to(&route.target.owner)
        {
            return Err(CommandError::new(
                "sessionChanged",
                "通知不属于当前数据账号",
                false,
            ));
        }
        st.notification_routes.acknowledge(id)?;
    }
    Ok(())
}

#[cfg(test)]
async fn take_notification_target(st: &SyncShared, id: Uuid) -> Option<NotificationTarget> {
    st.notification_routes.activate(id).unwrap();
    let next = next_notification(st).await.unwrap()?;
    if next.notification_id != id {
        return None;
    }
    let target = st
        .notification_routes
        .pending()
        .unwrap()
        .into_iter()
        .find(|r| r.id == id)?
        .target;
    acknowledge_notification(st, id, next.session_generation)
        .await
        .unwrap();
    Some(target)
}
#[cfg(test)]
mod tests;
