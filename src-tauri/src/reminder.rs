//! 到期提醒：应用进程运行期间生效；每分钟核对，唤醒/变更后刷新。
//! 提醒记录先随数据文件持久化成功，再投递系统通知（资格规则见 doing-core）。

use std::time::Duration;

use doing_core::clock::SystemClock;
use doing_core::Clock;
use uuid::Uuid;

use crate::desktop;
use crate::engine;
use crate::SyncShared;

const TICK_SECONDS: u64 = 60;

/// 启动提醒循环。
pub fn start_ticker(state: SyncShared) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(TICK_SECONDS)).await;
            refresh(&state).await;
        }
    });
}

/// 数据/设置变化后核对一次（含启动时）。
pub async fn refresh(st: &SyncShared) {
    let enabled = st.core.lock().await.settings.notifications_enabled;
    if enabled {
        let (due_now, sound) = {
            let core = st.core.lock().await;
            let now = SystemClock.now();
            let notified: std::collections::HashSet<Uuid> = core.store.notified_set();
            let sound = core.settings.notification_sound;
            let list = core
                .store
                .items()
                .iter()
                .filter(|it| {
                    !it.done
                        && it.due_date.is_some_and(|d| d <= now)
                        && !notified.contains(&it.id)
                })
                .map(|it| (it.id, it.text.clone()))
                .collect::<Vec<_>>();
            (list, sound)
        };
        let any = !due_now.is_empty();
        for (id, text) in due_now {
            // 先持久化提醒记录（成功提交才认为已提醒），再投递。
            let saved = engine::mutate_core(st, |core| {
                let now = SystemClock.now();
                core.store.mark_notified(id, now);
            })
            .await
            .1;
            if saved {
                notify(st, id, &text, sound).await;
            }
        }
        if any {
            engine::emit_snapshot(st).await;
        }
    }
    // 数据/设置变化后同步托盘（逾期标题 + 菜单状态）。
    desktop::refresh_tray(st).await;
}

async fn notify(st: &SyncShared, item_id: Uuid, body: &str, sound: bool) {
    use tauri_plugin_notification::NotificationExt;
    let Some(handle) = st.try_handle() else {
        return;
    };
    // 通知 id 用数字；点击回调经映射表还原任务 id。
    let numeric = st
        .next_notification_id
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    st.due_notification_map.lock().unwrap().insert(numeric, item_id);
    let mut builder = handle
        .notification()
        .builder()
        .title("已截止")
        .body(body.to_string())
        .id(numeric);
    if sound {
        builder = builder.sound("default");
    }
    if let Err(e) = builder.show() {
        eprintln!("[reminder] 通知投递失败: {e}");
    }
}

/// 取出“通知数字 id → 任务 id”映射（命中即移除；纯逻辑，便于单测）。
pub fn take_notification_target(st: &crate::state::AppState, numeric_id: i32) -> Option<Uuid> {
    st.due_notification_map.lock().unwrap().remove(&numeric_id)
}

/// 通知点击（由前端插件事件转发）：定位任务并打开面板。
pub async fn handle_notification_clicked(st: &SyncShared, numeric_id: i32) {
    let item_id = take_notification_target(st, numeric_id);
    crate::handle_due_notification(st, item_id).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creds::CredentialStore;
    use std::sync::Arc;

    fn state() -> Arc<crate::state::AppState> {
        let dir = tempfile::tempdir().unwrap();
        let creds = Arc::new(crate::creds::memory::MemoryStore::default());
        Arc::new(crate::state::AppState::new(
            dir.path().to_path_buf(),
            None,
            creds as Arc<dyn CredentialStore>,
        ))
    }

    #[test]
    fn notification_map_roundtrip_and_miss() {
        let st = state();
        let item = Uuid::new_v4();
        st.due_notification_map.lock().unwrap().insert(7, item);
        assert_eq!(take_notification_target(&st, 7), Some(item), "命中并取出");
        assert_eq!(take_notification_target(&st, 7), None, "取出后不重复消费");
        assert_eq!(take_notification_target(&st, 99), None, "未知数字 id 返回 None");
    }

    async fn seed_due(st: &Arc<crate::state::AppState>) -> Uuid {
        let mut core = st.core.lock().await;
        let past = doing_core::clock::SystemClock.now() - chrono::Duration::minutes(5);
        core.store.add("已到期任务", Some(past), doing_core::clock::SystemClock.now())
            .unwrap()
            .id
            .unwrap()
    }

    #[tokio::test]
    async fn disabled_refresh_neither_marks_nor_fires() {
        let st = state();
        st.core.lock().await.settings.notifications_enabled = false;
        let id = seed_due(&st).await;
        refresh(&st).await;
        assert!(
            !st.core.lock().await.store.notified_due_ids().contains(&id),
            "通知关闭时不得写入提醒记录"
        );
    }

    #[tokio::test]
    async fn enabled_refresh_marks_due_items_once() {
        let st = state();
        let id = seed_due(&st).await;
        refresh(&st).await;
        assert!(
            st.core.lock().await.store.notified_due_ids().contains(&id),
            "到期项应被标记为已提醒"
        );
        // 再刷新一次不得重复处理（记录已持久化到内存态）。
        refresh(&st).await;
        let count = st
            .core
            .lock()
            .await
            .store
            .notified_due_ids()
            .iter()
            .filter(|x| **x == id)
            .count();
        assert_eq!(count, 1, "重复刷新不应重复标记");
    }
}
