//! IPC 命令层：前端唯一调用入口。
//! 分组：workspace / auth / sync / settings / system。
//! 所有输入先校验（Rust 侧再校验，UI 校验不是安全边界），
//! 变更经核心串行锁并原子落盘，成功后广播事件。

use serde::Deserialize;
use ts_rs::TS;
use uuid::Uuid;

use doing_core::clock::{parse_rfc3339, SystemClock};
use doing_core::store::Store;
use doing_core::AppSettings;
use doing_core::Clock;

use crate::auth;
use crate::desktop;
use crate::engine;
use crate::events::*;
use crate::reminder;
use tauri::Emitter;
use crate::SyncShared;

pub type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

async fn store_mutate<F>(st: &SyncShared, f: F) -> CmdResult<MutationView>
where
    F: FnOnce(&mut Store, chrono::DateTime<chrono::Utc>) -> Result<doing_core::Mutation, doing_core::CoreError>,
{
    let (result, saved) = engine::mutate_core(st, |core| {
        let now = SystemClock.now();
        f(&mut core.store, now)
    })
    .await;
    let mutation = result.map_err(|e| match e {
        doing_core::CoreError::NoOp => "没有可保存的修改".to_string(),
        doing_core::CoreError::NotFound => "事项不存在或已删除".to_string(),
        other => err(other),
    })?;
    if !saved {
        return Err("本地保存失败：请检查磁盘空间或数据目录权限".to_string());
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
    let core = st.core.lock().await;
    Ok(StartupView {
        auth: {
            let auth = st.auth.lock().await;
            AuthStateView {
                logged_in: auth.logged_in,
                username: auth.username.clone(),
                server_url: auth.server_url.clone(),
                is_authenticating: auth.is_authenticating,
            }
        },
        snapshot: SnapshotView::from_store(&core.store, core.save_failed),
        settings: SettingsView::from(&core.settings),
        sync: {
            let e = st.engine.read().await;
            SyncStateViewPayload {
                state: e.state,
                last_sync_at: e.last_sync_at.clone(),
                last_error: e.last_error.clone(),
                conflict_cloud_count: e.conflict.as_ref().map(|c| c.items.len()),
                conflict_cloud_version: e.conflict.as_ref().map(|c| c.version),
            }
        },
        conflict: {
            let e = st.engine.read().await;
            e.conflict.as_ref().map(ConflictView::from)
        },
        legacy_import_available: st
            .legacy_path
            .as_ref()
            .map(|p| p.exists() && core.store.is_empty())
            .unwrap_or(false),
        migration: None,
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
    pub message: String,
    pub offers_undo: bool,
    #[ts(type = "string | null")]
    pub id: Option<Uuid>,
}

fn parse_due(due: Option<&str>) -> CmdResult<Option<chrono::DateTime<chrono::Utc>>> {
    match due {
        None => Ok(None),
        Some(s) => parse_rfc3339(s).map(Some).map_err(err),
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
pub async fn task_delete(
    state: tauri::State<'_, SyncShared>,
    id: Uuid,
) -> CmdResult<MutationView> {
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
    store_mutate(st, |store, now| store.move_before(arg.id, arg.target_id, now)).await
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

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct AuthArg {
    pub username: String,
    pub password: String,
}

#[tauri::command]
pub async fn auth_login(
    state: tauri::State<'_, SyncShared>,
    arg: AuthArg,
) -> CmdResult<()> {
    auth::authenticate(
        state.inner().clone(),
        "api/v1/auth/login",
        crate::net::client::default_base_url(),
        &arg.username,
        &arg.password,
    )
    .await
    .map_err(err)
}

#[tauri::command]
pub async fn auth_register(
    state: tauri::State<'_, SyncShared>,
    arg: AuthArg,
) -> CmdResult<()> {
    auth::authenticate(
        state.inner().clone(),
        "api/v1/auth/register",
        crate::net::client::default_base_url(),
        &arg.username,
        &arg.password,
    )
    .await
    .map_err(err)
}

#[tauri::command]
pub async fn auth_logout(state: tauri::State<'_, SyncShared>) -> CmdResult<()> {
    let st: &SyncShared = state.inner();
    auth::logout(st).await;
    Ok(())
}

// MARK: - sync

#[tauri::command]
pub async fn sync_flush(state: tauri::State<'_, SyncShared>) -> CmdResult<()> {
    let st: &SyncShared = state.inner();
    engine::flush(st).await;
    Ok(())
}

#[tauri::command]
pub async fn sync_restore(state: tauri::State<'_, SyncShared>) -> CmdResult<()> {
    let st: &SyncShared = state.inner();
    engine::restore_from_cloud(st).await;
    Ok(())
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct CloudChoiceArg {
    /// 用户确认时看到的云端候选版本（防“确认的不是当前展示快照”）。
    pub cloud_version: i64,
}

#[tauri::command]
pub async fn conflict_choose_local(
    state: tauri::State<'_, SyncShared>,
    arg: CloudChoiceArg,
) -> CmdResult<()> {
    let st: &SyncShared = state.inner();
    verify_candidate(st, arg.cloud_version).await?;
    engine::choose_local(st).await;
    // 保留本地 = 采纳归属为当前账号（若本地此前无归属/异账号）。
    engine::claim_owner_public(st).await;
    Ok(())
}

#[tauri::command]
pub async fn conflict_choose_cloud(
    state: tauri::State<'_, SyncShared>,
    arg: CloudChoiceArg,
) -> CmdResult<()> {
    let st: &SyncShared = state.inner();
    verify_candidate(st, arg.cloud_version).await?;
    {
        let e = st.engine.read().await;
        let cloud = e.conflict.clone().ok_or("没有待处理的冲突")?;
        drop(e);
        engine::choose_cloud(st, &cloud).await;
    }
    engine::claim_owner_public(st).await;
    Ok(())
}

#[tauri::command]
pub async fn conflict_defer(state: tauri::State<'_, SyncShared>) -> CmdResult<()> {
    let st: &SyncShared = state.inner();
    st.engine.write().await.state = SyncStateView::Conflict;
    engine::emit_sync_state(st).await;
    Ok(())
}

async fn verify_candidate(st: &SyncShared, cloud_version: i64) -> CmdResult<()> {
    let e = st.engine.read().await;
    match &e.conflict {
        Some(cloud) if cloud.version == cloud_version => Ok(()),
        Some(cloud) => Err(format!(
            "云端快照已变化（版本 {}），请重新打开冲突面板确认",
            cloud.version
        )),
        None => Err("没有待处理的冲突".to_string()),
    }
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
    let st: &SyncShared = state.inner();
    let handle = st.handle();
    let view = {
        let mut core = st.core.lock().await;
        if let Some(v) = patch.appearance {
            core.settings.appearance = match v.as_str() {
                "light" => doing_core::AppAppearance::Light,
                "dark" => doing_core::AppAppearance::Dark,
                _ => doing_core::AppAppearance::System,
            };
        }
        if let Some(v) = patch.show_focus_in_menu_bar {
            core.settings.show_focus_in_menu_bar = v;
        }
        if let Some(v) = patch.menu_bar_text_limit {
            core.settings.menu_bar_text_limit = v.clamp(1, 60);
        }
        if let Some(v) = patch.notifications_enabled {
            core.settings.notifications_enabled = v;
        }
        if let Some(v) = patch.notification_sound {
            core.settings.notification_sound = v;
        }
        if let Some(v) = patch.due_soon_enabled {
            core.settings.due_soon_enabled = v;
        }
        if let Some(v) = patch.due_soon_hours {
            core.settings.due_soon_hours = v.clamp(0.25, 168.0);
        }
        if let Some(v) = patch.show_overdue_in_menu_bar {
            core.settings.show_overdue_in_menu_bar = v;
        }
        if let Some(v) = patch.show_overdue_banner {
            core.settings.show_overdue_banner = v;
        }
        if let Some(v) = patch.automatic_sync {
            core.settings.automatic_sync = v;
        }
        core.settings.normalize();
        let view = SettingsView::from(&core.settings);
        drop(core);
        save_settings(st, &view);
        view
    };
    let _ = handle.emit(EVT_SETTINGS, view.clone());
    let st2 = st.clone();
    let automatic = view.automatic_sync;
    tauri::async_runtime::spawn(async move {
        reminder::refresh(&st2).await;
        desktop::refresh_tray(&st2).await;
        if automatic {
            engine::flush(&st2).await;
        }
    });
    Ok(view)
}

fn save_settings(st: &SyncShared, view: &SettingsView) {
    // 持久化核心设置对象（SettingsView 与 AppSettings 字段一一对应）。
    let s = AppSettings {
        appearance: match view.appearance.as_str() {
            "light" => doing_core::AppAppearance::Light,
            "dark" => doing_core::AppAppearance::Dark,
            _ => doing_core::AppAppearance::System,
        },
        mode: view.mode.clone(),
        show_focus_in_menu_bar: view.show_focus_in_menu_bar,
        menu_bar_text_limit: view.menu_bar_text_limit,
        notifications_enabled: view.notifications_enabled,
        notification_sound: view.notification_sound,
        due_soon_enabled: view.due_soon_enabled,
        due_soon_hours: view.due_soon_hours,
        show_overdue_in_menu_bar: view.show_overdue_in_menu_bar,
        show_overdue_banner: view.show_overdue_banner,
        automatic_sync: view.automatic_sync,
    };
    let path = st.settings_repo.path();
    match serde_json::to_vec_pretty(&s) {
        Ok(bytes) => {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
        Err(e) => eprintln!("[settings] 序列化失败: {e}"),
    }
}

/// 恢复默认设置（偏好；不动事项与账户）。
#[tauri::command]
pub async fn settings_reset(state: tauri::State<'_, SyncShared>) -> CmdResult<SettingsView> {
    let st: &SyncShared = state.inner();
    let view = {
        let mut core = st.core.lock().await;
        core.settings.reset();
        let view = SettingsView::from(&core.settings);
        drop(core);
        save_settings(st, &view);
        view
    };
    let _ = st.handle().emit(EVT_SETTINGS, view.clone());
    Ok(view)
}

// MARK: - system

#[tauri::command]
pub async fn system_open_settings(state: tauri::State<'_, SyncShared>) -> CmdResult<()> {
    desktop::open_settings_window(&state.inner().handle());
    Ok(())
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
    let panel = !st.core.lock().await.settings.mode_is_panel();
    desktop::set_mode(st, panel).await;
    Ok(panel)
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

/// 开机启动（薄适配：macOS SMAppService / Windows 启动项）。
#[tauri::command]
pub async fn launch_at_login_get(state: tauri::State<'_, SyncShared>) -> CmdResult<bool> {
    use tauri_plugin_autostart::ManagerExt;
    Ok(state
        .inner()
        .handle()
        .autolaunch()
        .is_enabled()
        .unwrap_or(false))
}

#[tauri::command]
pub async fn launch_at_login_set(
    state: tauri::State<'_, SyncShared>,
    arg: BoolArg,
) -> CmdResult<String> {
    use tauri_plugin_autostart::ManagerExt;
    let handle = state.inner().handle();
    let result = if arg.enabled {
        handle.autolaunch().enable()
    } else {
        handle.autolaunch().disable()
    };
    match result {
        Ok(()) => Ok(String::new()),
        Err(e) => {
            if arg.enabled {
                Ok("需要在系统设置的登录项中批准。".into())
            } else {
                Err(format!("无法更新启动项：{e}"))
            }
        }
    }
}

/// 迁移导入（前端确认后调用）。
#[tauri::command]
pub async fn migration_import(
    state: tauri::State<'_, SyncShared>,
    source: String,
) -> CmdResult<crate::migrate::MigrationStatus> {
    let st: &SyncShared = state.inner();
    crate::migrate::import_legacy_cmd(st, source).await.map_err(err)
}

/// 回滚导出（计划 §7.4）：按旧兼容格式导出，并揭示到 Finder。
#[tauri::command]
pub async fn data_export_legacy(state: tauri::State<'_, SyncShared>) -> CmdResult<String> {
    let st: &SyncShared = state.inner();
    crate::migrate::export_legacy(st).await.map_err(err)
}

/// 通知点击（前端插件事件 → 定位任务/打开面板）。
#[tauri::command]
pub async fn system_notify_clicked(
    state: tauri::State<'_, SyncShared>,
    id: i32,
) -> CmdResult<()> {
    let st: &SyncShared = state.inner();
    crate::reminder::handle_notification_clicked(st, id).await;
    Ok(())
}

/// 托盘菜单直接调用的清除已完成（无前端）。
pub async fn clear_completed_cmd(st: &SyncShared) {
    let _ = store_mutate(st, |store, now| store.clear_completed(now)).await;
}
