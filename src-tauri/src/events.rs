//! 前端事件与快照载荷（Rust → WebView 的唯一通道）。
//! 事件名以 `doing://` 前缀组织；所有载荷都携带 revision 以便前端丢弃旧消息。

use serde::Serialize;
use ts_rs::TS;
use uuid::Uuid;

use doing_core::store::Store;

pub const EVT_SNAPSHOT: &str = "doing://snapshot";
pub const EVT_SYNC_STATE: &str = "doing://sync-state";
pub const EVT_AUTH_STATE: &str = "doing://auth-state";
pub const EVT_SETTINGS: &str = "doing://settings";
pub const EVT_SESSION_LOST: &str = "doing://session-lost";
pub const EVT_CONFLICT: &str = "doing://conflict";
pub const EVT_MIGRATION: &str = "doing://migration";
pub const EVT_SAVE_FAILED: &str = "doing://save-failed";
pub const EVT_OPEN_SETTINGS: &str = "doing://open-settings";

/// 任务项（展示层只读投影）。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct ItemView {
    #[ts(type = "string")]
    pub id: Uuid,
    pub text: String,
    pub done: bool,
    pub created_at: String,
    pub due_date: Option<String>,
    pub updated_at: String,
}

impl From<&doing_core::Item> for ItemView {
    fn from(item: &doing_core::Item) -> Self {
        fn fmt(dt: chrono::DateTime<chrono::Utc>) -> String {
            let base = dt.format("%Y-%m-%dT%H:%M:%S").to_string();
            let nanos = dt.timestamp_subsec_nanos();
            if nanos == 0 {
                format!("{base}Z")
            } else {
                format!("{base}.{:03}Z", nanos / 1_000_000)
            }
        }
        Self {
            id: item.id,
            text: item.text.clone(),
            done: item.done,
            created_at: fmt(item.created_at),
            due_date: item.due_date.map(fmt),
            updated_at: fmt(item.updated_at),
        }
    }
}

/// 一次变更后的完整快照（前端据此整体重渲染）。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct SnapshotView {
    pub session_generation: u64,
    pub event_revision: u64,
    pub revision: u64,
    pub items: Vec<ItemView>,
    #[ts(type = "string | null")]
    pub focus_id: Option<Uuid>,
    pub undo_title: Option<String>,
    pub redo_title: Option<String>,
    #[ts(type = "Array<string>")]
    pub notified_due_ids: Vec<Uuid>,
    pub save_failed: bool,
}

impl SnapshotView {
    pub fn from_store(
        store: &Store,
        save_failed: bool,
        session_generation: u64,
        event_revision: u64,
    ) -> Self {
        Self {
            session_generation,
            event_revision,
            revision: store.revision(),
            items: store.items().iter().map(ItemView::from).collect(),
            focus_id: store.focus_id(),
            undo_title: store.undo_title().map(ToOwned::to_owned),
            redo_title: store.redo_title().map(ToOwned::to_owned),
            notified_due_ids: store.notified_due_ids().to_vec(),
            save_failed,
        }
    }
}

/// 同步状态可见视图。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "../src/types.gen.ts", rename = "SyncStateName")]
pub enum SyncStateView {
    Idle,
    Pending,
    Syncing,
    Synced,
    Failed,
    Conflict,
    Unauthorized,
}

impl SyncStateView {
    pub fn display_text(self) -> &'static str {
        match self {
            SyncStateView::Idle => "空闲",
            SyncStateView::Pending => "等待同步",
            SyncStateView::Syncing => "同步中",
            SyncStateView::Synced => "已同步",
            SyncStateView::Failed => "同步失败",
            SyncStateView::Conflict => "需要处理冲突",
            SyncStateView::Unauthorized => "登录已失效",
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "../src/types.gen.ts", rename = "SyncStatePayload")]
pub struct SyncStateViewPayload {
    pub session_generation: u64,
    pub event_revision: u64,
    pub state: SyncStateView,
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
    pub conflict_cloud_count: Option<usize>,
    pub conflict_cloud_version: Option<String>,
    #[ts(type = "string | null")]
    pub conflict_id: Option<Uuid>,
}

/// 原生通知点击定位；旧会话事件不能把新账号 UI 聚焦到相同 UUID 的任务。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct ScrollTargetView {
    #[ts(type = "string")]
    pub notification_id: Uuid,
    pub session_generation: u64,
    pub event_revision: u64,
    #[ts(type = "string")]
    pub item_id: Uuid,
}

/// 设置视图（与核心设置字段一一对应）。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct SettingsView {
    pub revision: u64,
    pub event_revision: u64,
    pub error: Option<String>,
    #[ts(type = "\"system\" | \"light\" | \"dark\"")]
    pub appearance: String,
    #[ts(type = "\"popover\" | \"panel\"")]
    pub mode: String,
    pub show_focus_in_menu_bar: bool,
    pub menu_bar_text_limit: i64,
    pub notifications_enabled: bool,
    pub notification_sound: bool,
    pub due_soon_enabled: bool,
    pub due_soon_hours: f64,
    pub show_overdue_in_menu_bar: bool,
    pub show_overdue_banner: bool,
    pub automatic_sync: bool,
}

impl SettingsView {
    pub fn from_core(core: &crate::state::CoreInner, event_revision: u64) -> Self {
        let s = &core.settings;
        Self {
            revision: core.preferences_revision,
            event_revision,
            error: core.preferences_error.clone(),
            appearance: match s.appearance {
                doing_core::AppAppearance::System => "system",
                doing_core::AppAppearance::Light => "light",
                doing_core::AppAppearance::Dark => "dark",
            }
            .to_string(),
            mode: s.mode.clone(),
            show_focus_in_menu_bar: s.show_focus_in_menu_bar,
            menu_bar_text_limit: s.menu_bar_text_limit,
            notifications_enabled: s.notifications_enabled,
            notification_sound: s.notification_sound,
            due_soon_enabled: s.due_soon_enabled,
            due_soon_hours: s.due_soon_hours,
            show_overdue_in_menu_bar: s.show_overdue_in_menu_bar,
            show_overdue_banner: s.show_overdue_banner,
            automatic_sync: s.automatic_sync,
        }
    }
}

/// 登录状态（不发 Token，只发布尔与归属展示信息）。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct AuthStateView {
    pub session_generation: u64,
    pub event_revision: u64,
    pub logged_in: bool,
    pub username: Option<String>,
    pub server_url: Option<String>,
    pub is_authenticating: bool,
    pub error: Option<String>,
}

/// 冲突面板展示的云端候选。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct ConflictView {
    pub session_generation: u64,
    pub event_revision: u64,
    #[ts(type = "string")]
    pub candidate_id: Uuid,
    pub reason: String,
    pub cloud_version: String,
    pub cloud_count: usize,
    pub cloud_preview: Vec<ItemView>,
    pub updated_at: Option<String>,
}

impl ConflictView {
    pub fn from(
        candidate: &crate::state::ConflictCandidate,
        session_generation: u64,
        event_revision: u64,
    ) -> Self {
        let snapshot = &candidate.snapshot;
        Self {
            session_generation,
            event_revision,
            candidate_id: candidate.id,
            reason: match candidate.reason {
                doing_core::data::ConflictReason::Ownership => {
                    "本地数据尚未归属当前账号，请明确选择后再同步。"
                }
                doing_core::data::ConflictReason::LocalChanged => {
                    "读取云端期间本地有新修改，已保留你的新工作。"
                }
                doing_core::data::ConflictReason::UploadUncertain => {
                    "上次上传结果未能确认，云端版本已变化，请核对两份快照。"
                }
                doing_core::data::ConflictReason::Restore => "云端恢复未完成，请确认要保留的快照。",
                _ => "本地与云端快照不同，请选择保留的版本。",
            }
            .into(),
            cloud_version: snapshot.version.to_string(),
            cloud_count: snapshot.items.len(),
            cloud_preview: snapshot
                .items
                .iter()
                .take(3)
                .map(|dto| {
                    // 预览只展示文本；转换失败时用占位。
                    let item = dto.to_item().ok();
                    ItemView {
                        id: dto.id,
                        text: dto.text.clone(),
                        done: dto.done,
                        created_at: dto.created_at.clone(),
                        due_date: dto.due_date.clone(),
                        updated_at: dto.updated_at.clone(),
                    }
                    .with_text(item.as_ref().map(|i| i.text.clone()))
                })
                .collect(),
            updated_at: snapshot.updated_at.clone(),
        }
    }
}

impl ItemView {
    fn with_text(mut self, text: Option<String>) -> Self {
        if let Some(text) = text {
            self.text = text;
        }
        self
    }
}
