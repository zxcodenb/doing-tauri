//! Doing 桌面客户端（Tauri 2）入口：装配状态、插件、托盘、提醒与命令。

mod auth;
#[cfg(test)]
mod bindings;
mod commands;
mod creds;
mod desktop;
mod engine;
mod error;
mod events;
#[cfg(test)]
mod go_mysql_tests;
mod migrate;
mod net;
mod preferences;
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
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support/Doing/items.json"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // 与托盘点击一致的显示路径：定位、置顶策略与焦点统一由 present_main 处理。
            let Some(shared) = app
                .try_state::<SyncShared>()
                .map(|state| state.inner().clone())
            else {
                return;
            };
            #[cfg(windows)]
            if args.len() == 2 {
                if let Some(id) =
                    doing_notifications::parse_activation_uri(reminder::ACTIVATION_SCHEME, &args[1])
                {
                    reminder::receive_activation(
                        &shared,
                        doing_notifications::Activation::Route(id),
                    );
                    return;
                }
            }
            #[cfg(not(windows))]
            let _ = args;
            tauri::async_runtime::spawn(async move { desktop::present_main(&shared).await });
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init());
    #[cfg(windows)]
    let builder = builder.plugin(tauri_plugin_deep_link::init());
    builder
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = app.path().app_data_dir().expect("应用数据目录不可用");
            std::fs::create_dir_all(&data_dir).expect("创建应用数据目录失败");
            let identifier = app.config().identifier.clone();
            let creds: Arc<dyn CredentialStore> = Arc::new(creds::KeychainStore::new(
                &format!("{identifier}.tokens"),
                &data_dir,
            ));
            let st = AppState::new(data_dir.clone(), legacy_macos_path(), creds);
            *st.app_handle.write().unwrap() = Some(handle.clone());
            app.manage(Arc::new(st));
            let state = app.state::<SyncShared>();
            reminder::install_native(state.inner(), identifier);

            load_persisted(state.inner(), &crate::net::client::default_base_url());
            #[cfg(windows)]
            install_windows_activations(state.inner());

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
                // 重启恢复：同归属自动续传 / 云端恢复 / 无归属或换号则冲突挂起（不自动上传）。
                auth::resume_after_restart(&st).await;
                // 桌面浮窗形态：启动即显示；菜单栏形态保持隐藏等待点击。
                // 开发辅助：DOING_SHOW_ON_LAUNCH=1 时强制显示主面板便于截图/交互验证。
                let force_show = cfg!(debug_assertions)
                    && std::env::var("DOING_SHOW_ON_LAUNCH")
                        .map(|v| v == "1")
                        .unwrap_or(false);
                let panel = st.core.lock().await.settings.mode_is_panel();
                if panel || force_show || st.notification_routes.has_pending().unwrap_or(false) {
                    desktop::present_main(&st).await;
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == desktop::MAIN_WINDOW {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let st = window.app_handle().state::<SyncShared>().inner().clone();
                    tauri::async_runtime::spawn(async move {
                        desktop::dismiss_main(&st).await;
                    });
                }
            }
            // 设置窗口正常销毁；下次按需重建并重新执行订阅握手。

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
            commands::notification_permission,
            commands::notification_request_permission,
            commands::notification_next,
            commands::notification_ack,
            commands::migration_import,
            commands::migration_resume,
            commands::migration_cancel,
            commands::migration_keep_current,
            commands::data_export_legacy,
            commands::launch_at_login_get,
            commands::launch_at_login_set,
        ])
        .run(tauri::generate_context!())
        .expect("Doing 启动失败");
}

fn load_persisted(st: &AppState, server_url: &str) {
    migrate::recover_before_load(st);
    load_after_migration(st, server_url);
}

fn load_after_migration(st: &AppState, server_url: &str) {
    if st.core.blocking_lock().migration.blocks_writes() {
        migrate::fence_incomplete(&mut st.core.blocking_lock());
        // Never load a partial pair or touch saved credentials while recovery is incomplete.
        return;
    }
    // 设置文件。
    preferences::load_persisted(st);
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
    // 调试会话始终使用内存仓库，先替换再恢复，绝不读取/写入真实 Keychain。
    #[cfg(all(debug_assertions, not(test)))]
    if std::env::var("DOING_SKIP_LOGIN").is_ok_and(|v| v == "1") {
        use base64::Engine;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"sub":1,"username":"dev-local","type":"access"}"#);
        let access = std::env::var("DOING_DEV_TOKEN")
            .unwrap_or_else(|_| format!("eyJhbGciOiJIUzI1NiJ9.{payload}.debug-only"));
        let memory = Arc::new(crate::creds::memory::MemoryStore::default());
        if let Ok(session) = crate::creds::SavedSession::new(
            server_url,
            crate::net::dto::AuthDto {
                access,
                refresh: "debug-refresh".into(),
                expires_in: 900,
            },
        ) {
            let _ = memory.save(&session);
        }
        st.auth.blocking_lock().vault = crate::creds::CredentialVault::new(memory);
    }
    auth::restore_credentials(st, server_url);
}

async fn emit_settings_view(st: &SyncShared) {
    preferences::emit_current(st).await;
}

/// Only an explicit reminders setting change requests OS authorization. Startup, migration
/// and timer ticks query/report state but must not repeatedly prompt a denied user.
async fn request_notification_permission_if_needed(st: &SyncShared) {
    if !st.core.lock().await.settings.notifications_enabled {
        return;
    }
    if let Ok(view) = reminder::permission_view(st, false).await {
        if matches!(view.status, reminder::NotificationPermission::NotDetermined) {
            let _ = reminder::permission_view(st, true).await;
        }
    }
}

#[cfg(windows)]
fn install_windows_activations(st: &SyncShared) {
    use tauri_plugin_deep_link::DeepLinkExt;
    let Some(handle) = st.try_handle() else {
        return;
    };
    let weak = Arc::downgrade(st);
    handle.deep_link().on_open_url(move |event| {
        if let Some(st) = weak.upgrade() {
            for url in event.urls() {
                if let Some(id) = doing_notifications::parse_activation_uri(
                    reminder::ACTIVATION_SCHEME,
                    url.as_str(),
                ) {
                    reminder::receive_activation(&st, doing_notifications::Activation::Route(id));
                }
            }
        }
    });
    if let Ok(Some(urls)) = handle.deep_link().get_current() {
        for url in urls {
            if let Some(id) =
                doing_notifications::parse_activation_uri(reminder::ACTIVATION_SCHEME, url.as_str())
            {
                reminder::receive_activation(st, doing_notifications::Activation::Route(id));
            }
        }
    }
    // Scheme registration is performed by the installer config, not an unsolicited
    // runtime registry write or register_all(). No remote/HTTP schemes are accepted.
}
