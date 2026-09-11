//! IPC 命令层：前端唯一调用入口。
//! 分组：workspace / auth / sync / settings / system。
//! 所有输入先校验（Rust 侧再校验，UI 校验不是安全边界），
//! 变更经核心串行锁并原子落盘，成功后广播事件。

use serde::Deserialize;
use ts_rs::TS;
use uuid::Uuid;

use doing_core::clock::{parse_rfc3339, SystemClock};
use doing_core::store::Store;
use doing_core::Clock;

use crate::auth;
use crate::desktop;
use crate::engine;
use crate::error::CommandError;
use crate::events::*;
use crate::reminder;
use crate::SyncShared;

pub type CmdResult<T> = Result<T, CommandError>;

async fn store_mutate<F>(st: &SyncShared, f: F) -> CmdResult<MutationView>
where
    F: FnOnce(
        &mut Store,
        chrono::DateTime<chrono::Utc>,
    ) -> Result<doing_core::Mutation, doing_core::CoreError>,
{
    let (result, saved) = engine::mutate_core(st, true, |core| {
        let now = SystemClock.now();
        let mutation = f(&mut core.store, now)?;
        Ok((mutation, core.store.revision()))
    })
    .await;
    let (mutation, revision) = result.map_err(CommandError::from)?;
    if !saved {
        return Err(CommandError::persistence(
            "本地保存失败：请检查磁盘空间或数据目录权限",
        ));
    }
    // 收尾：快照事件 + 自动同步触发 + 提醒/托盘刷新。
    engine::emit_snapshot(st).await;
    let st2 = st.clone();
    tauri::async_runtime::spawn(async move {
        engine::on_core_mutated(&st2).await;
        reminder::refresh(&st2).await;
        desktop::refresh_tray(&st2).await;
    });
    Ok(MutationView {
        revision,
        message: mutation.message,
        offers_undo: mutation.offers_undo,
        id: mutation.id,
    })
}

// MARK: - 初始状态

#[derive(serde::Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct StartupView {
    pub auth: AuthStateView,
    pub snapshot: SnapshotView,
    pub settings: SettingsView,
    pub sync: SyncStateViewPayload,
    /// 启动早期可能已进入冲突（订阅前的事件会丢）：随握手返回当前候选。
    pub conflict: Option<ConflictView>,
    pub legacy_import_available: bool,
    pub migration: Option<crate::migrate::MigrationStatus>,
}

#[tauri::command]
pub async fn init_state(state: tauri::State<'_, SyncShared>) -> CmdResult<StartupView> {
    let st: &SyncShared = state.inner();
    let e = st.engine.read().await;
    let auth = st.auth.lock().await;
    let core = st.core.lock().await;
    let revision = st.next_event_revision();
    Ok(StartupView {
        auth: auth::view(&auth, e.session_generation, revision),
        snapshot: SnapshotView::from_store(
            &core.store,
            core.save_failed,
            e.session_generation,
            revision,
        ),
        settings: SettingsView::from_core(&core, revision),
        sync: engine::sync_view(&e, revision),
        conflict: e
            .conflict
            .as_ref()
            .map(|c| ConflictView::from(c, e.session_generation, revision)),
        legacy_import_available: st.legacy_path.as_ref().is_some_and(|p| {
            p.exists() && matches!(st.repo.load(), Ok(doing_core::repo::LoadResult::Empty))
        }),
        migration: Some(crate::migrate::current_status(st, &core, revision)),
    })
}

// MARK: - workspace

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct TextArg {
    pub text: String,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct DueArg {
    #[serde(default)]
    pub due: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct MoveArg {
    #[ts(type = "string")]
    pub id: Uuid,
    #[ts(type = "string")]
    pub target_id: Uuid,
}

#[derive(serde::Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct MutationView {
    /// 此响应对应的已落盘修订，而不是稍后读取的内存修订。
    pub revision: u64,
    pub message: String,
    pub offers_undo: bool,
    #[ts(type = "string | null")]
    pub id: Option<Uuid>,
}

fn parse_due(due: Option<&str>) -> CmdResult<Option<chrono::DateTime<chrono::Utc>>> {
    match due {
        None => Ok(None),
        Some(s) => parse_rfc3339(s)
            .map(Some)
            .map_err(|_| CommandError::input("截止日期格式无效")),
    }
}

#[tauri::command]
pub async fn task_add(
    state: tauri::State<'_, SyncShared>,
    arg: TextArg,
    due: Option<String>,
) -> CmdResult<MutationView> {
    let st: &SyncShared = state.inner();
    let due = parse_due(due.as_deref())?;
    store_mutate(st, |store, now| store.add(&arg.text, due, now)).await
}

#[tauri::command]
pub async fn task_edit(
    state: tauri::State<'_, SyncShared>,
    id: Uuid,
    arg: TextArg,
) -> CmdResult<MutationView> {
    let st: &SyncShared = state.inner();
    store_mutate(st, |store, now| store.rename(id, &arg.text, now)).await
}

#[tauri::command]
pub async fn task_set_due(
    state: tauri::State<'_, SyncShared>,
    id: Uuid,
    arg: DueArg,
) -> CmdResult<MutationView> {
    let st: &SyncShared = state.inner();
    let due = parse_due(arg.due.as_deref())?;
    store_mutate(st, |store, now| store.set_due(id, due, now)).await
}

#[tauri::command]
pub async fn task_toggle_done(
    state: tauri::State<'_, SyncShared>,
    id: Uuid,
) -> CmdResult<MutationView> {
    let st: &SyncShared = state.inner();
    store_mutate(st, |store, now| store.toggle_done(id, now)).await
}

#[tauri::command]
pub async fn task_delete(state: tauri::State<'_, SyncShared>, id: Uuid) -> CmdResult<MutationView> {
    let st: &SyncShared = state.inner();
    store_mutate(st, |store, now| store.delete(id, now)).await
}

#[tauri::command]
pub async fn task_toggle_focus(
    state: tauri::State<'_, SyncShared>,
    id: Uuid,
) -> CmdResult<MutationView> {
    let st: &SyncShared = state.inner();
    store_mutate(st, |store, now| store.toggle_focus(id, now)).await
}

#[tauri::command]
pub async fn task_clear_completed(state: tauri::State<'_, SyncShared>) -> CmdResult<MutationView> {
    let st: &SyncShared = state.inner();
    store_mutate(st, |store, now| store.clear_completed(now)).await
}

#[tauri::command]
pub async fn task_move(
    state: tauri::State<'_, SyncShared>,
    arg: MoveArg,
) -> CmdResult<MutationView> {
    let st: &SyncShared = state.inner();
    store_mutate(st, |store, now| {
        store.move_before(arg.id, arg.target_id, now)
    })
    .await
}

#[tauri::command]
pub async fn history_undo(state: tauri::State<'_, SyncShared>) -> CmdResult<MutationView> {
    let st: &SyncShared = state.inner();
    store_mutate(st, |store, now| store.undo(now)).await
}

#[tauri::command]
pub async fn history_redo(state: tauri::State<'_, SyncShared>) -> CmdResult<MutationView> {
    let st: &SyncShared = state.inner();
    store_mutate(st, |store, now| store.redo(now)).await
}

// MARK: - auth

#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct AuthArg {
    pub username: String,
    pub password: String,
}

#[tauri::command]
pub async fn auth_login(state: tauri::State<'_, SyncShared>, arg: AuthArg) -> CmdResult<()> {
    auth::authenticate(
        state.inner().clone(),
        "api/v1/auth/login",
        crate::net::client::default_base_url(),
        &arg.username,
        &arg.password,
    )
    .await
}

#[tauri::command]
pub async fn auth_register(state: tauri::State<'_, SyncShared>, arg: AuthArg) -> CmdResult<()> {
    auth::authenticate(
        state.inner().clone(),
        "api/v1/auth/register",
        crate::net::client::default_base_url(),
        &arg.username,
        &arg.password,
    )
    .await
}

#[tauri::command]
pub async fn auth_logout(state: tauri::State<'_, SyncShared>) -> CmdResult<()> {
    let st: &SyncShared = state.inner();
    auth::logout(st).await
}

// MARK: - sync

#[tauri::command]
pub async fn sync_flush(state: tauri::State<'_, SyncShared>) -> CmdResult<()> {
    let st: &SyncShared = state.inner();
    engine::flush(st).await
}

#[tauri::command]
pub async fn sync_restore(state: tauri::State<'_, SyncShared>) -> CmdResult<()> {
    let st: &SyncShared = state.inner();
    engine::restore_from_cloud(st).await
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct CloudChoiceArg {
    #[ts(type = "string")]
    pub candidate_id: Uuid,
    /// 服务端 i64 保持十进制字符串跨 IPC，不能经过 JS number。
    pub cloud_version: String,
}
fn choice_version(value: &str) -> CmdResult<i64> {
    let version: i64 = value
        .parse()
        .map_err(|_| CommandError::input("云端版本无效"))?;
    if version < 0 || version.to_string() != value {
        return Err(CommandError::input("云端版本无效"));
    }
    Ok(version)
}
#[tauri::command]
pub async fn conflict_choose_local(
    state: tauri::State<'_, SyncShared>,
    arg: CloudChoiceArg,
) -> CmdResult<()> {
    engine::choose_local(
        state.inner(),
        arg.candidate_id,
        choice_version(&arg.cloud_version)?,
    )
    .await
}
#[tauri::command]
pub async fn conflict_choose_cloud(
    state: tauri::State<'_, SyncShared>,
    arg: CloudChoiceArg,
) -> CmdResult<()> {
    engine::choose_cloud(
        state.inner(),
        arg.candidate_id,
        choice_version(&arg.cloud_version)?,
    )
    .await
}
#[tauri::command]
pub async fn conflict_defer(state: tauri::State<'_, SyncShared>) -> CmdResult<()> {
    engine::defer_conflict(state.inner()).await
}

// MARK: - settings

#[derive(Debug, Default, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct SettingsPatch {
    #[ts(optional)]
    pub appearance: Option<String>,
    #[ts(optional)]
    pub show_focus_in_menu_bar: Option<bool>,
    #[ts(optional)]
    pub menu_bar_text_limit: Option<i64>,
    #[ts(optional)]
    pub notifications_enabled: Option<bool>,
    #[ts(optional)]
    pub notification_sound: Option<bool>,
    #[ts(optional)]
    pub due_soon_enabled: Option<bool>,
    #[ts(optional)]
    pub due_soon_hours: Option<f64>,
    #[ts(optional)]
    pub show_overdue_in_menu_bar: Option<bool>,
    #[ts(optional)]
    pub show_overdue_banner: Option<bool>,
    #[ts(optional)]
    pub automatic_sync: Option<bool>,
}

#[tauri::command]
pub async fn settings_update(
    state: tauri::State<'_, SyncShared>,
    patch: SettingsPatch,
) -> CmdResult<SettingsView> {
    settings_update_inner(state.inner(), patch).await
}

async fn settings_update_inner(st: &SyncShared, patch: SettingsPatch) -> CmdResult<SettingsView> {
    crate::preferences::update(st, |settings| {
        if let Some(value) = patch.appearance {
            settings.appearance = match value.as_str() {
                "system" => doing_core::AppAppearance::System,
                "light" => doing_core::AppAppearance::Light,
                "dark" => doing_core::AppAppearance::Dark,
                _ => return Err(CommandError::input("外观选项无效")),
            };
        }
        if let Some(value) = patch.show_focus_in_menu_bar {
            settings.show_focus_in_menu_bar = value;
        }
        if let Some(value) = patch.menu_bar_text_limit {
            settings.menu_bar_text_limit = value.clamp(1, 60);
        }
        if let Some(value) = patch.notifications_enabled {
            settings.notifications_enabled = value;
        }
        if let Some(value) = patch.notification_sound {
            settings.notification_sound = value;
        }
        if let Some(value) = patch.due_soon_enabled {
            settings.due_soon_enabled = value;
        }
        if let Some(value) = patch.due_soon_hours {
            if !value.is_finite() {
                return Err(CommandError::input("提醒时长必须为有限数字"));
            }
            settings.due_soon_hours = value.clamp(0.25, 168.0);
        }
        if let Some(value) = patch.show_overdue_in_menu_bar {
            settings.show_overdue_in_menu_bar = value;
        }
        if let Some(value) = patch.show_overdue_banner {
            settings.show_overdue_banner = value;
        }
        if let Some(value) = patch.automatic_sync {
            settings.automatic_sync = value;
        }
        Ok(())
    })
    .await
}

/// 恢复默认设置（偏好；不动事项与账户）。
#[tauri::command]
pub async fn settings_reset(state: tauri::State<'_, SyncShared>) -> CmdResult<SettingsView> {
    settings_reset_inner(state.inner()).await
}

async fn settings_reset_inner(st: &SyncShared) -> CmdResult<SettingsView> {
    crate::preferences::reset(st).await
}

// MARK: - system

#[tauri::command]
pub async fn system_open_settings(state: tauri::State<'_, SyncShared>) -> CmdResult<()> {
    desktop::open_settings_window(&state.inner().handle())
}

#[tauri::command]
pub async fn system_hide_main(state: tauri::State<'_, SyncShared>) -> CmdResult<()> {
    let st: &SyncShared = state.inner();
    desktop::dismiss_main(st).await;
    Ok(())
}

#[tauri::command]
pub async fn system_toggle_mode(state: tauri::State<'_, SyncShared>) -> CmdResult<bool> {
    let st: &SyncShared = state.inner();
    desktop::toggle_mode(st).await
}

#[tauri::command]
pub async fn system_quit(state: tauri::State<'_, SyncShared>) -> CmdResult<()> {
    let st: &SyncShared = state.inner();
    desktop::quit_async(st).await;
    Ok(())
}

#[tauri::command]
pub async fn system_save_now(state: tauri::State<'_, SyncShared>) -> CmdResult<bool> {
    let st: &SyncShared = state.inner();
    Ok(engine::save_now(st).await)
}

#[tauri::command]
pub async fn system_data_path(state: tauri::State<'_, SyncShared>) -> CmdResult<String> {
    let st: &SyncShared = state.inner();
    Ok(st.repo.path().display().to_string())
}

#[tauri::command]
pub async fn system_reveal_data(state: tauri::State<'_, SyncShared>) -> CmdResult<()> {
    let st: &SyncShared = state.inner();
    desktop::reveal_data_file(st);
    Ok(())
}

#[derive(Debug, Deserialize, TS)]
#[ts(export_to = "types.gen.ts")]
pub struct BoolArg {
    pub enabled: bool,
}

/// 开机启动：当前插件使用 macOS LaunchAgent / Windows 启动项；不等同于旧版 SMAppService。
#[tauri::command]
pub async fn launch_at_login_get(state: tauri::State<'_, SyncShared>) -> CmdResult<bool> {
    use tauri_plugin_autostart::ManagerExt;
    state
        .inner()
        .handle()
        .autolaunch()
        .is_enabled()
        .map_err(|_| CommandError::new("systemIntegrationFailed", "无法读取系统登录项状态", true))
}

#[tauri::command]
pub async fn launch_at_login_set(
    state: tauri::State<'_, SyncShared>,
    arg: BoolArg,
) -> CmdResult<String> {
    use tauri_plugin_autostart::ManagerExt;
    let handle = state.inner().handle();
    let manager = handle.autolaunch();
    let result = if arg.enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|_| {
        CommandError::new(
            "systemIntegrationFailed",
            "无法更新登录项，请检查系统权限后重试",
            true,
        )
    })?;
    let actual = manager.is_enabled().map_err(|_| {
        CommandError::new(
            "systemIntegrationFailed",
            "无法确认登录项是否已生效，请检查系统设置",
            true,
        )
    })?;
    if actual != arg.enabled {
        return Err(CommandError::new(
            "systemIntegrationFailed",
            "登录项尚未生效，请在系统设置中批准后重试",
            true,
        ));
    }
    Ok(String::new())
}

/// 迁移导入（前端确认后调用）。
#[tauri::command]
pub async fn migration_import(
    state: tauri::State<'_, SyncShared>,
    source: String,
    preferences: Option<bool>,
) -> CmdResult<crate::migrate::MigrationStatus> {
    let st: &SyncShared = state.inner();
    crate::migrate::import_legacy_cmd(st, source, preferences.unwrap_or(false)).await
}

#[tauri::command]
pub async fn migration_resume(
    state: tauri::State<'_, SyncShared>,
    transaction_id: Uuid,
) -> CmdResult<crate::migrate::MigrationStatus> {
    crate::migrate::resume_cmd(state.inner(), transaction_id).await
}
#[tauri::command]
pub async fn migration_cancel(
    state: tauri::State<'_, SyncShared>,
    transaction_id: Uuid,
) -> CmdResult<crate::migrate::MigrationStatus> {
    crate::migrate::cancel_cmd(state.inner(), transaction_id).await
}
#[tauri::command]
pub async fn migration_keep_current(
    state: tauri::State<'_, SyncShared>,
    transaction_id: Uuid,
) -> CmdResult<crate::migrate::MigrationStatus> {
    crate::migrate::keep_current_cmd(state.inner(), transaction_id).await
}

/// 回滚导出（计划 §7.4）：按旧兼容格式导出，并揭示到 Finder。
#[tauri::command]
pub async fn data_export_legacy(state: tauri::State<'_, SyncShared>) -> CmdResult<String> {
    let st: &SyncShared = state.inner();
    if st.core.lock().await.migration.blocks_writes() {
        return Err(crate::migrate::problem(
            "migrationPending",
            "请先处理待恢复迁移；半完成状态不会被导出，原始备份仍保留",
        ));
    }
    crate::migrate::export_legacy(st)
        .await
        .map_err(|_| CommandError::persistence("导出失败，请检查数据目录权限或磁盘空间"))
}

#[tauri::command]
pub async fn notification_permission(
    state: tauri::State<'_, SyncShared>,
) -> CmdResult<crate::reminder::NotificationPermissionView> {
    crate::reminder::permission_view(state.inner(), false).await
}
#[tauri::command]
pub async fn notification_request_permission(
    state: tauri::State<'_, SyncShared>,
) -> CmdResult<crate::reminder::NotificationPermissionView> {
    crate::reminder::permission_view(state.inner(), true).await
}
#[tauri::command]
pub async fn notification_next(
    state: tauri::State<'_, SyncShared>,
) -> CmdResult<Option<ScrollTargetView>> {
    crate::reminder::next_notification(state.inner()).await
}
#[tauri::command]
pub async fn notification_ack(
    state: tauri::State<'_, SyncShared>,
    notification_id: Uuid,
    session_generation: u64,
) -> CmdResult<()> {
    crate::reminder::acknowledge_notification(state.inner(), notification_id, session_generation)
        .await
}

/// 托盘菜单直接调用的清除已完成（无前端）。
pub async fn clear_completed_cmd(st: &SyncShared) {
    let _ = store_mutate(st, |store, now| store.clear_completed(now)).await;
}

#[cfg(test)]
mod transaction_tests {
    use super::*;
    use std::sync::Arc;

    fn state() -> (SyncShared, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let st = Arc::new(crate::state::AppState::new(
            dir.path().to_path_buf(),
            None,
            Arc::new(crate::creds::memory::MemoryStore::default()),
        ));
        {
            let mut auth = st.auth.try_lock().unwrap();
            let lease = auth
                .vault
                .activate(
                    crate::creds::SavedSession::new(
                        "http://127.0.0.1:9",
                        crate::net::test_server::tokens_for(1, "tester", "local"),
                    )
                    .unwrap(),
                )
                .unwrap();
            auth.client = Some(crate::net::client::ApiClient::authenticated(lease));
            auth.logged_in = true;
            auth.username = Some("tester".into());
            auth.server_url = Some("http://127.0.0.1:9".into());
            auth.account_id = Some("1".into());
        }
        st.core.try_lock().unwrap().settings.automatic_sync = false;
        (st, dir)
    }

    #[tokio::test]
    async fn task_commit_persists_dirty_before_returning_success() {
        let (st, _dir) = state();
        let result = store_mutate(&st, |store, now| {
            store.add("已保存就必须是 dirty", None, now)
        })
        .await
        .unwrap();
        let doing_core::repo::LoadResult::Loaded(data) = st.repo.load().unwrap() else {
            panic!("成功返回前必须已提交文件");
        };
        assert_eq!(data.items.len(), 1);
        assert_eq!(data.revision, result.revision);
        assert_eq!(Store::from_data(&data).revision(), result.revision);
        assert!(
            data.sync.dirty,
            "不能等异步收尾才写 dirty，进程可能已经退出"
        );
    }

    #[tokio::test]
    async fn failed_task_save_rolls_back_items_focus_history_and_revision() {
        let (st, _dir) = state();
        std::fs::create_dir(st.repo.path()).unwrap();
        let result = store_mutate(&st, |store, now| store.add("请保留录入草稿", None, now)).await;
        assert!(result.is_err());
        let core = st.core.lock().await;
        assert!(
            core.store.is_empty(),
            "失败事务不能把未保存任务混入权威状态"
        );
        assert_eq!(core.store.focus_id(), None);
        assert_eq!(core.store.undo_title(), None);
        assert_eq!(core.store.revision(), 0);
        assert!(core.save_failed);
    }

    #[tokio::test]
    async fn rejected_task_is_a_true_noop_without_disk_write() {
        let (st, _dir) = state();
        assert!(store_mutate(&st, |store, now| store.add("   ", None, now))
            .await
            .is_err());
        assert!(!st.repo.path().exists(), "空操作不得创建文件或轮换备份");
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_task_commits_keep_disk_and_memory_at_same_revision() {
        let (st, _dir) = state();
        let mut jobs = Vec::new();
        for n in 0..12 {
            let st = st.clone();
            jobs.push(tokio::spawn(async move {
                store_mutate(&st, |store, now| store.add(format!("task-{n}"), None, now))
                    .await
                    .unwrap()
                    .revision
            }));
        }
        let mut revisions = Vec::new();
        for job in jobs {
            revisions.push(job.await.unwrap());
        }
        revisions.sort();
        assert_eq!(revisions, (1..=12).collect::<Vec<_>>());
        let doing_core::repo::LoadResult::Loaded(data) = st.repo.load().unwrap() else {
            panic!("已提交文件")
        };
        let core = st.core.lock().await;
        // 既定文件/IPC 协议将绝对时间截断至毫秒；比较完整序列化投影，
        // 而不是把未序列化的 SystemClock 亚毫秒精度当作数据回退。
        assert_eq!(
            serde_json::to_value(&data).unwrap(),
            serde_json::to_value(core.to_data_file()).unwrap()
        );
        assert_eq!(data.items.len(), 12);
        assert_eq!(data.revision, 12);
        assert!(data.sync.dirty);
    }
}

#[cfg(test)]
mod version_contract_tests {
    use super::*;
    #[test]
    fn cloud_choice_i64_roundtrips_without_a_js_number() {
        let arg: CloudChoiceArg = serde_json::from_str(r#"{"candidateId":"aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa","cloudVersion":"9007199254740993"}"#).unwrap();
        assert_eq!(
            choice_version(&arg.cloud_version).unwrap(),
            9_007_199_254_740_993
        );
        assert_eq!(choice_version("9223372036854775807").unwrap(), i64::MAX);
        for value in ["-1", "+1", "01", "1.0", "9e2", " 9", "9223372036854775808"] {
            assert_eq!(choice_version(value).unwrap_err().code, "invalidInput");
        }
        assert!(serde_json::from_str::<CloudChoiceArg>(r#"{"candidateId":"aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa","cloudVersion":9007199254740993}"#).is_err());
    }
    #[test]
    fn go_conflict_body_uses_camel_case_current_version() {
        let body: crate::net::dto::ApiErrorBody = serde_json::from_str(r#"{"code":"snapshot_conflict","message":"conflict","currentVersion":9007199254740993}"#).unwrap();
        assert_eq!(body.current_version, Some(9_007_199_254_740_993));
    }
}

#[cfg(test)]
mod preference_transaction_tests {
    use super::*;
    use std::sync::Arc;
    fn state() -> (SyncShared, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let st = Arc::new(crate::state::AppState::new(
            dir.path().into(),
            None,
            Arc::new(crate::creds::memory::MemoryStore::default()),
        ));
        (st, dir)
    }
    #[tokio::test]
    async fn failed_preference_save_does_not_publish_the_uncommitted_setting() {
        let (st, _dir) = state();
        std::fs::create_dir(st.settings_repo.path()).unwrap();
        let result = settings_update_inner(
            &st,
            SettingsPatch {
                automatic_sync: Some(false),
                ..Default::default()
            },
        )
        .await;
        assert!(result.is_err(), "磁盘保存失败必须返回错误");
        assert!(
            st.core.lock().await.settings.automatic_sync,
            "内存不能先于设置提交改变"
        );
    }
    #[tokio::test]
    async fn reset_preferences_is_transactional_too() {
        let (st, _dir) = state();
        st.core.lock().await.settings.appearance = doing_core::AppAppearance::Dark;
        std::fs::create_dir(st.settings_repo.path()).unwrap();
        assert!(settings_reset_inner(&st).await.is_err());
        assert_eq!(
            st.core.lock().await.settings.appearance,
            doing_core::AppAppearance::Dark
        );
    }
    #[tokio::test]
    async fn future_or_corrupt_preferences_are_not_silently_overwritten() {
        for bytes in [
            br#"{"schemaVersion":99,"private":"preserve"}"#.as_slice(),
            b"{broken".as_slice(),
        ] {
            let (st, _dir) = state();
            std::fs::write(st.settings_repo.path(), bytes).unwrap();
            assert!(settings_update_inner(
                &st,
                SettingsPatch {
                    automatic_sync: Some(false),
                    ..Default::default()
                }
            )
            .await
            .is_err());
            assert_eq!(std::fs::read(st.settings_repo.path()).unwrap(), bytes);
        }
    }
    #[tokio::test]
    async fn empty_preference_patch_does_not_create_or_rotate_files() {
        let (st, _dir) = state();
        settings_update_inner(&st, SettingsPatch::default())
            .await
            .unwrap();
        assert!(
            !st.settings_repo.path().exists(),
            "空操作不能覆盖/轮换设置文件"
        );
    }
}
