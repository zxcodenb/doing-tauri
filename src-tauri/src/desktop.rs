//! 桌面协调：托盘/菜单、窗口形态（popover/panel）、菜单栏标题与位置记忆。
//! 事件入口：托盘点击/菜单；窗口失焦由前端决定是否收起。

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use doing_core::clock::SystemClock;
use doing_core::Clock;

use crate::auth;
use crate::commands;
use crate::engine;
use crate::events::*;
use crate::migrate;
use crate::reminder;
use crate::state::AppState;
use crate::SyncShared;

pub const MAIN_WINDOW: &str = "main";
pub const SETTINGS_WINDOW: &str = "settings";
pub(crate) mod placement;

const WINDOW_W: f64 = 380.0;
const WINDOW_H: f64 = 580.0;

pub fn main_window(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window(MAIN_WINDOW)
}

// MARK: - 托盘

/// 菜单/标题的一次性快照（避免在异步上下文用 blocking 锁）。
pub struct MenuState {
    pub sync_text: String,
    pub has_conflict: bool,
    pub mode_panel: bool,
    pub has_completed: bool,
    pub logged_in: bool,
    pub syncing: bool,
    pub focus_text: Option<String>,
    pub show_overdue_in_menu_bar: bool,
    pub has_overdue: bool,
    pub show_focus: bool,
    pub text_limit: i64,
}

impl MenuState {
    /// 菜单栏标题文本（macOS）：逾期优先，否则焦点文字。
    pub fn tray_title(&self) -> String {
        if self.show_overdue_in_menu_bar && self.has_overdue {
            return " ⏰ 已逾期".to_string();
        }
        if !self.show_focus {
            return String::new();
        }
        match &self.focus_text {
            Some(text) if !text.is_empty() => {
                let limit = self.text_limit.max(1) as usize;
                let mut chars = text.chars();
                let head: String = chars.by_ref().take(limit).collect();
                if chars.next().is_some() {
                    format!(" {head}…")
                } else {
                    format!(" {head}")
                }
            }
            _ => String::new(),
        }
    }
}

async fn menu_state(st: &SyncShared) -> MenuState {
    let (engine_part, auth_logged_in) = {
        let e = st.engine.read().await;
        let conflict = if e.conflict.is_some() {
            "（有未处理冲突）"
        } else {
            ""
        };
        (
            (
                format!("同步状态：{}{}", e.state.display_text(), conflict),
                e.conflict.is_some(),
                e.state == SyncStateView::Syncing,
            ),
            st.auth.lock().await.logged_in,
        )
    };
    let core = st.core.lock().await;
    let now = SystemClock.now();
    MenuState {
        sync_text: engine_part.0,
        has_conflict: engine_part.1,
        syncing: engine_part.2,
        logged_in: auth_logged_in,
        mode_panel: core.settings.mode_is_panel(),
        has_completed: core.store.has_completed(),
        focus_text: core.store.focus_text().map(str::to_owned),
        show_overdue_in_menu_bar: core.settings.show_overdue_in_menu_bar,
        has_overdue: core.store.has_overdue(now),
        show_focus: core.settings.show_focus_in_menu_bar,
        text_limit: core.settings.menu_bar_text_limit,
    }
}

fn build_menu(app: &AppHandle, ms: &MenuState) -> tauri::Result<Menu<tauri::Wry>> {
    let open = MenuItem::with_id(app, "open", "打开 Doing", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let pin = MenuItem::with_id(
        app,
        "pin",
        if ms.mode_panel {
            "收回菜单栏"
        } else {
            "钉在桌面上"
        },
        true,
        None::<&str>,
    )?;
    let clear = MenuItem::with_id(app, "clear", "清除已完成", ms.has_completed, None::<&str>)?;
    let sync_status = MenuItem::with_id(app, "sync-status", &ms.sync_text, false, None::<&str>)?;
    let restore = MenuItem::with_id(
        app,
        "restore",
        "从云端恢复…",
        ms.logged_in && !ms.syncing,
        None::<&str>,
    )?;
    let conflict = MenuItem::with_id(
        app,
        "conflict",
        "处理同步冲突",
        ms.has_conflict,
        None::<&str>,
    )?;
    let logout = MenuItem::with_id(app, "logout", "退出登录", ms.logged_in, None::<&str>)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let reveal = MenuItem::with_id(app, "reveal", "显示数据文件", true, None::<&str>)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Doing", true, None::<&str>)?;
    Menu::with_items(
        app,
        &[
            &open,
            &settings,
            &sep1,
            &pin,
            &clear,
            &sync_status,
            &restore,
            &conflict,
            &logout,
            &sep2,
            &reveal,
            &sep3,
            &quit,
        ],
    )
}

pub fn build_tray(app: &AppHandle, state: &SyncShared) -> tauri::Result<()> {
    // 启动阶段（主线程）：用阻塞锁快照一次菜单。
    let ms = menu_state_blocking(state);
    let menu = build_menu(app, &ms)?;
    let builder = TrayIconBuilder::with_id("doing-tray")
        .tooltip("Doing · 打开事项，右键查看更多")
        .show_menu_on_left_click(false)
        .menu(&menu);
    #[cfg(target_os = "macos")]
    let builder = {
        if let Ok(icon) = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png")) {
            builder.icon(icon).icon_as_template(true)
        } else {
            builder
        }
    };
    let tray = builder
        .on_tray_icon_event({
            let state = state.clone();
            // 去抖：Down/Up（或辅助功能 performClick 的双派发）在 250ms 内只消费一次。
            let last = std::sync::Arc::new(std::sync::Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(1),
            ));
            move |_tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    ..
                } = event
                {
                    let mut g = last.lock().unwrap();
                    if g.elapsed() < std::time::Duration::from_millis(250) {
                        return;
                    }
                    *g = std::time::Instant::now();
                    drop(g);
                    let st = state.clone();
                    tauri::async_runtime::spawn(async move { toggle_main(&st).await });
                }
            }
        })
        .build(app)?;
    let _ = tray;
    // 菜单事件在 app.on_menu_event 注册一次（build_tray 内联处理）。
    let st = state.clone();
    app.on_menu_event(move |app, event| {
        let st = st.clone();
        match event.id().as_ref() {
            "open" => {
                tauri::async_runtime::spawn(async move { present_main(&st).await });
            }
            "settings" => {
                if let Err(error) = open_settings_window(app) {
                    let _ = engine::try_emit(&st, EVT_SAVE_FAILED, error.message);
                }
            }
            "pin" => {
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = toggle_mode(&st).await {
                        let _ = engine::try_emit(&st, EVT_SAVE_FAILED, error.message);
                    }
                });
            }
            "clear" => {
                tauri::async_runtime::spawn(async move {
                    commands::clear_completed_cmd(&st).await;
                });
            }
            "restore" => {
                // 前端模态确认后调用 sync_restore；这里只打开设置-账户页提示。
                tauri::async_runtime::spawn(async move {
                    let _ = st.handle().emit(EVT_OPEN_SETTINGS, "account");
                });
            }
            "conflict" => {
                tauri::async_runtime::spawn(async move {
                    present_main(&st).await;
                    engine::emit_sync_state(&st).await;
                });
            }
            "logout" => {
                tauri::async_runtime::spawn(async move {
                    let _ = auth::logout(&st).await;
                });
            }
            "reveal" => {
                reveal_data_file(st.as_ref());
            }
            "quit" => {
                tauri::async_runtime::spawn(async move { quit_async(&st).await });
            }
            _ => {}
        }
    });
    Ok(())
}

fn menu_state_blocking(st: &AppState) -> MenuState {
    let logged_in = st.auth.blocking_lock().logged_in;
    let e = st.engine.blocking_read();
    let core = st.core.blocking_lock();
    let conflict = if e.conflict.is_some() {
        "（有未处理冲突）"
    } else {
        ""
    };
    let now = SystemClock.now();
    MenuState {
        sync_text: format!("同步状态：{}{}", e.state.display_text(), conflict),
        has_conflict: e.conflict.is_some(),
        syncing: e.state == SyncStateView::Syncing,
        logged_in,
        mode_panel: core.settings.mode_is_panel(),
        has_completed: core.store.has_completed(),
        focus_text: core.store.focus_text().map(str::to_owned),
        show_overdue_in_menu_bar: core.settings.show_overdue_in_menu_bar,
        has_overdue: core.store.has_overdue(now),
        show_focus: core.settings.show_focus_in_menu_bar,
        text_limit: core.settings.menu_bar_text_limit,
    }
}

/// 数据/设置变化后刷新托盘标题与菜单（后台任务安全；无句柄环境跳过）。
pub async fn refresh_tray(st: &SyncShared) {
    let ms = menu_state(st).await;
    let Some(handle) = st.try_handle() else {
        return;
    };
    let Some(tray) = handle.tray_by_id("doing-tray") else {
        return;
    };
    #[cfg(target_os = "macos")]
    {
        let title = ms.tray_title();
        let tip = ms
            .focus_text
            .as_ref()
            .filter(|t| !t.is_empty())
            .map(|t| format!("Doing · 当前焦点：{t}"))
            .unwrap_or_else(|| "Doing · 打开事项，右键查看更多".to_string());
        let _ = tray.set_title(Some(&title));
        let _ = tray.set_tooltip(Some(&tip));
    }
    if let Ok(menu) = build_menu(&handle, &ms) {
        let _ = tray.set_menu(Some(menu));
    }
}

// MARK: - 窗口形态

pub async fn toggle_main(st: &SyncShared) {
    if is_visible(st).await {
        dismiss_main(st).await;
    } else {
        present_main(st).await;
    }
}

pub async fn is_visible(st: &SyncShared) -> bool {
    let Some(handle) = st.try_handle() else {
        return false;
    };
    let Some(window) = main_window(&handle) else {
        return false;
    };
    window.is_visible().unwrap_or(false)
}

pub async fn present_main(st: &SyncShared) {
    let Some(handle) = st.try_handle() else {
        return; // 无 UI 环境（测试）直接跳过。
    };
    let Some(window) = main_window(&handle) else {
        return;
    };
    position_window(st, &window).await;
    // 置顶：菜单栏形态对齐旧 NSPopover（浮于普通窗口之上）；浮窗形态对齐旧 NSPanel(.floating)。
    let _ = window.set_always_on_top(true);
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
    // 通知前端“这是主动唤起”，失焦收起逻辑在短窗口内豁免，避免抢焦点失败即被自收起。
    let _ = window.emit("doing://window-shown", ());
}

pub async fn dismiss_main(st: &SyncShared) {
    if let Err(error) = save_window_origin(st).await {
        let _ = crate::engine::try_emit(st, crate::events::EVT_SAVE_FAILED, error.message);
        return;
    }
    let Some(window) = main_window(&st.handle()) else {
        return;
    };
    let _ = window.hide();
}

async fn position_window(st: &SyncShared, window: &tauri::WebviewWindow) {
    let (panel, saved) = {
        let core = st.core.lock().await;
        (core.settings.mode_is_panel(), core.window_origin)
    };
    let monitors = window.available_monitors().unwrap_or_default();
    let areas: Vec<_> = monitors.iter().map(placement::WorkArea::from).collect();
    let fallback = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
        .map(|monitor| placement::WorkArea::from(&monitor))
        .or_else(|| areas.first().copied());
    let logical_size = window
        .outer_size()
        .ok()
        .zip(
            window
                .scale_factor()
                .ok()
                .filter(|scale| scale.is_finite() && (0.25..=8.0).contains(scale)),
        )
        .map(|(size, scale)| {
            (
                f64::from(size.width) / scale,
                f64::from(size.height) / scale,
            )
        })
        .unwrap_or((WINDOW_W, WINDOW_H));
    let position = if panel {
        saved
            .and_then(|origin| placement::restore(origin, &areas, logical_size))
            .or_else(|| fallback.and_then(|area| placement::centered(area, logical_size)))
    } else {
        fallback.and_then(|area| placement::popover_fallback(area, logical_size))
    };
    if let Some(position) = position {
        let _ = window.set_position(position.position());
    } else {
        // No trustworthy geometry is available; let the OS choose rather than replay an unsafe origin.
        let _ = window.center();
    }
}

/// 切换形态（草稿/编辑状态由前端持有，不受影响）。
pub async fn toggle_mode(st: &SyncShared) -> Result<bool, crate::error::CommandError> {
    save_window_origin(st).await?;
    let view = crate::preferences::update(st, |settings| {
        settings.mode = if settings.mode_is_panel() {
            "popover"
        } else {
            "panel"
        }
        .into();
        Ok(())
    })
    .await?;
    Ok(view.mode == "panel")
}

pub async fn apply_mode_after_commit(st: &SyncShared) {
    if st.try_handle().is_some() && is_visible(st).await {
        present_main(st).await;
    }
}

// MARK: - 位置记忆（与设置共用原子偏好仓库）
async fn save_window_origin(st: &SyncShared) -> Result<(), crate::error::CommandError> {
    if !st.core.lock().await.settings.mode_is_panel() {
        return Ok(());
    }
    let Some(handle) = st.try_handle() else {
        return Ok(());
    };
    let Some(window) = main_window(&handle) else {
        return Ok(());
    };
    let position = window.outer_position().map_err(|_| {
        crate::error::CommandError::new("windowUnavailable", "无法读取窗口位置", true)
    })?;
    crate::preferences::save_origin(
        st,
        crate::preferences::WindowOrigin {
            x: position.x,
            y: position.y,
        },
    )
    .await
}

// MARK: - 设置窗口 / 数据文件 / 持久化 / 退出

pub fn open_settings_window(app: &AppHandle) -> Result<(), crate::error::CommandError> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static OPENING: AtomicBool = AtomicBool::new(false);
    // 不在 UI 线程等待另一线程的窗口创建（它可能正等待 UI 线程），避免互锁。
    if OPENING.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    struct Opening;
    impl Drop for Opening {
        fn drop(&mut self) {
            OPENING.store(false, Ordering::SeqCst);
        }
    }
    let _opening = Opening;
    let failure =
        || crate::error::CommandError::new("windowUnavailable", "无法打开设置窗口，请重试", true);
    let window = if let Some(window) = app.get_webview_window(SETTINGS_WINDOW) {
        window
    } else {
        tauri::WebviewWindowBuilder::new(
            app,
            SETTINGS_WINDOW,
            tauri::WebviewUrl::App("index.html".into()),
        )
        .title("Doing 设置")
        .inner_size(760.0, 610.0)
        .min_inner_size(620.0, 480.0)
        .resizable(true)
        .decorations(true)
        .visible(false)
        .center()
        .build()
        .map_err(|_| failure())?
    };
    window.show().map_err(|_| failure())?;
    window.set_focus().map_err(|_| failure())
}

pub fn reveal_data_file(st: &AppState) {
    let path = st.repo.path().display().to_string();
    let _ = tauri_plugin_opener::OpenerExt::opener(&st.handle()).reveal_item_in_dir(&path);
}

pub async fn quit_async(st: &SyncShared) {
    if st.core.lock().await.migration.blocks_writes() {
        // The durable journal, not the fenced/possibly empty UI state, is the restart authority.
        // Never write that UI state over a partially published target merely to quit.
        if let Some(handle) = st.try_handle() {
            handle.exit(0);
        }
        return;
    }
    if let Err(error) = save_window_origin(st).await {
        let _ = crate::engine::try_emit(st, crate::events::EVT_SAVE_FAILED, error.message);
        return;
    }
    // 复用同一个串行仓库；失败时留在应用内让用户恢复/导出，不能吞错后退出。
    if engine::save_now(st).await {
        st.handle().exit(0);
    } else {
        present_main(st).await;
    }
}

/// 启动后的环境探测与首轮提醒（供 lib.rs setup 使用）。
pub async fn post_startup(st: &SyncShared) {
    migrate::probe_and_emit(st).await;
    reminder::refresh(st).await;
}
