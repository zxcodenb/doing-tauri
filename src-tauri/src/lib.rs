//! Doing 桌面客户端（Tauri 2）入口：装配状态、插件、托盘、提醒与命令。

mod auth;
#[cfg(test)]
mod bindings;
mod commands;
mod creds;
mod desktop;
mod engine;
mod events;
mod migrate;
mod net;
mod reminder;
mod state;

use std::path::PathBuf;
use std::sync::Arc;

use doing_core::repo::LoadResult;
use doing_core::store::Store;
use tauri::{Emitter, Manager};

use crate::creds::CredentialStore;
use crate::state::AppState;

pub type SyncShared = Arc<AppState>;

fn legacy_macos_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home).join("Library/Application Support/Doing/items.json")
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 与托盘点击一致的显示路径：定位、置顶策略与焦点统一由 present_main 处理。
            let shared = app.state::<SyncShared>().inner().clone();
            tauri::async_runtime::spawn(async move { desktop::present_main(&shared).await });
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = app
                .path()
                .app_data_dir()
                .expect("应用数据目录不可用");
            std::fs::create_dir_all(&data_dir).expect("创建应用数据目录失败");
            let identifier = app.config().identifier.clone();
            let creds: Arc<dyn CredentialStore> = Arc::new(
                creds::KeychainStore::new(&format!("{identifier}.tokens")),
            );
            let st = AppState::new(data_dir.clone(), legacy_macos_path(), creds);
            *st.app_handle.write().unwrap() = Some(handle.clone());
            app.manage(Arc::new(st));
            let state = app.state::<SyncShared>();

            load_persisted(state.inner());

            desktop::build_tray(&handle, state.inner()).expect("托盘创建失败");
            let tray_state = state.inner().clone();
            tauri::async_runtime::spawn(async move { desktop::refresh_tray(&tray_state).await });
            reminder::start_ticker(state.inner().clone());
            engine::start_retry_driver(state.inner().clone());

            let st = state.inner().clone();
            tauri::async_runtime::spawn(async move {
                auth::emit_auth(&st).await;
                engine::emit_snapshot(&st).await;
                engine::emit_sync_state(&st).await;
                emit_settings_view(&st).await;
                desktop::post_startup(&st).await;
                request_notification_permission_if_needed(&st).await;
                // 重启恢复：同归属自动续传 / 云端恢复 / 无归属或换号则冲突挂起（不自动上传）。
                auth::resume_after_restart(&st).await;
                // 桌面浮窗形态：启动即显示；菜单栏形态保持隐藏等待点击。
                // 开发辅助：DOING_SHOW_ON_LAUNCH=1 时强制显示主面板便于截图/交互验证。
                let force_show = cfg!(debug_assertions)
                    && std::env::var("DOING_SHOW_ON_LAUNCH").map(|v| v == "1").unwrap_or(false);
                let panel = st.core.lock().await.settings.mode_is_panel();
                if panel || force_show {
                    desktop::present_main(&st).await;
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Focused(false) = event {
                // 通知前端“窗口失焦”，由前端决定是否收起（输入法候选场景豁免）。
                let _ = window.emit("doing://window-blurred", ());
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::init_state,
            commands::task_add,
            commands::task_edit,
            commands::task_set_due,
            commands::task_toggle_done,
            commands::task_delete,
            commands::task_toggle_focus,
            commands::task_clear_completed,
            commands::task_move,
            commands::history_undo,
            commands::history_redo,
            commands::auth_login,
            commands::auth_register,
            commands::auth_logout,
            commands::sync_flush,
            commands::sync_restore,
            commands::conflict_choose_local,
            commands::conflict_choose_cloud,
            commands::conflict_defer,
            commands::settings_update,
            commands::settings_reset,
            commands::system_open_settings,
            commands::system_hide_main,
            commands::system_toggle_mode,
            commands::system_quit,
            commands::system_save_now,
            commands::system_data_path,
            commands::system_reveal_data,
            commands::system_notify_clicked,
            commands::migration_import,
            commands::data_export_legacy,
            commands::launch_at_login_get,
            commands::launch_at_login_set,
        ])
        .run(tauri::generate_context!())
        .expect("Doing 启动失败");
}

fn load_persisted(st: &AppState) {
    // 设置文件。
    if let Ok(bytes) = std::fs::read(st.settings_repo.path()) {
        if let Ok(settings) = serde_json::from_slice(&bytes) {
            let mut settings: doing_core::AppSettings = settings;
            settings.normalize();
            st.core.blocking_lock().settings = settings;
        }
    }
    // 数据文件。
    match st.repo.load() {
        Ok(LoadResult::Loaded(data)) => {
            let mut core = st.core.blocking_lock();
            core.store = Store::from_data(&data);
            core.meta = data.sync;
        }
        Ok(LoadResult::Empty) => {}
        Ok(LoadResult::NeedsAttention { .. }) | Err(_) => {
            let mut core = st.core.blocking_lock();
            core.save_failed = true;
        }
    }
    // 恢复登录态（凭据在系统安全存储中）。
    let mut auth = st.auth.blocking_lock();
    if auth.creds.load().is_ok() {
        auth.logged_in = true;
        auth.client = Some(crate::net::client::ApiClient::new(
            crate::net::client::default_base_url(),
            auth.creds.clone(),
        ));
        let core = st.core.blocking_lock();
        auth.username = core.meta.username.clone();
        auth.server_url = core.meta.server_url.clone();
    }
    // 仅 debug 构建：本地联调演示（DOING_SKIP_LOGIN=1 跳过登录门槛）。
    // 令牌注入内存凭据（不触碰系统 Keychain），配合本地 mock/开发服务端使用。
    #[cfg(debug_assertions)]
    if std::env::var("DOING_SKIP_LOGIN").map(|v| v == "1").unwrap_or(false) && !auth.logged_in {
        let dev_token =
            std::env::var("DOING_DEV_TOKEN").unwrap_or_else(|_| "mock-access".to_string());
        let mem = std::sync::Arc::new(crate::creds::memory::MemoryStore::default());
        let _ = mem.save(&crate::net::dto::AuthDto {
            access: dev_token.clone(),
            refresh: dev_token,
            expires_in: 900,
        });
        auth.creds = mem.clone();
        auth.logged_in = true;
        auth.username = Some("dev-local".into());
        auth.server_url = Some(crate::net::client::default_base_url());
        auth.client = Some(crate::net::client::ApiClient::new(
            crate::net::client::default_base_url(),
            mem,
        ));
    }
    drop(auth);
    // 引擎自动同步开关与 dirty 初始化。
    {
        let core = st.core.blocking_lock();
        let mut e = st.engine.blocking_write();
        e.automatic_enabled = core.settings.automatic_sync;
        e.dirty = core.meta.dirty;
        e.known_version = core.meta.known_server_version;
    }
    // 登录恢复后：本地有 dirty 或云端待仲裁 → 由前端 init 后的流程触发仲裁。
}

async fn emit_settings_view(st: &SyncShared) {
    let view = {
        let core = st.core.lock().await;
        crate::events::SettingsView::from(&core.settings)
    };
    let _ = st
        .handle()
        .emit(crate::events::EVT_SETTINGS, view);
}

async fn request_notification_permission_if_needed(st: &SyncShared) {
    use tauri_plugin_notification::NotificationExt;
    let handle = st.handle();
    let enabled = st.core.lock().await.settings.notifications_enabled;
    if enabled {
        if let Ok(tauri_plugin_notification::PermissionState::Prompt) = handle.notification().permission_state() {
            let _ = handle.notification().request_permission();
        }
    }
}

/// 通知点击（由 reminder 转发）：打开面板并定位任务。
pub async fn handle_due_notification(st: &SyncShared, item_id: Option<uuid::Uuid>) {
    let Some(handle) = st.try_handle() else {
        return;
    };
    desktop::present_main(st).await;
    if let Some(id) = item_id {
        let exists = st.core.lock().await.store.contains_id(id);
        if exists {
            let _ = handle.emit(crate::events::EVT_SCROLL_TO_ITEM, id.to_string());
        }
    }
}
