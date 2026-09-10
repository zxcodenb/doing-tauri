//! 构建脚本：为应用自定义命令生成权限（allow-/deny-），未在 capability 中授予的命令会被拒绝。
//! 命令清单是唯一事实来源之一——与 lib.rs 的 generate_handler!、src/lib/ipc.ts 三方由
//! 前端契约测试（contract.test.ts）双向校验，新增命令必须同时登记。

const COMMANDS: &[&str] = &[
    "init_state",
    "task_add",
    "task_edit",
    "task_set_due",
    "task_toggle_done",
    "task_delete",
    "task_toggle_focus",
    "task_clear_completed",
    "task_move",
    "history_undo",
    "history_redo",
    "auth_login",
    "auth_register",
    "auth_logout",
    "sync_flush",
    "sync_restore",
    "conflict_choose_local",
    "conflict_choose_cloud",
    "conflict_defer",
    "settings_update",
    "settings_reset",
    "system_open_settings",
    "system_hide_main",
    "system_toggle_mode",
    "system_quit",
    "system_save_now",
    "system_data_path",
    "system_reveal_data",
    "system_notify_clicked",
    "migration_import",
    "data_export_legacy",
    "launch_at_login_get",
    "launch_at_login_set",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run build script");
}
