//! 应用级共享状态：核心（任务+设置+同步元数据）、引擎视图、认证会话与持久化仓库。

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, watch};

use doing_core::repo::JsonRepo;
use doing_core::store::Store;
use doing_core::{AppSettings, DataFile, SyncMeta};

use crate::creds::CredentialStore;
use crate::events::SyncStateView;
use crate::net::client::ApiClient;
use crate::net::dto::SnapshotDto;

/// 与任务共用同一持久化提交边界的核心数据。
#[derive(Default)]
pub struct CoreInner {
    pub store: Store,
    pub meta: SyncMeta,
    pub settings: AppSettings,
    /// 最近一次落盘结果；失败时快照载荷会带上 save_failed 供 UI 提示。
    pub save_failed: bool,
}

impl CoreInner {
    pub fn to_data_file(&self) -> DataFile {
        self.store.to_data_file(self.meta.clone())
    }
}

/// 认证会话（凭据只在系统安全存储中；这里只保留客户端与展示信息）。
pub struct AuthInner {
    pub client: Option<ApiClient>,
    pub creds: Arc<dyn CredentialStore>,
    pub logged_in: bool,
    pub is_authenticating: bool,
    pub username: Option<String>,
    pub server_url: Option<String>,
}

/// 同步引擎的可见状态与并发守卫。
pub struct EngineInner {
    pub state: SyncStateView,
    pub conflict: Option<SnapshotDto>,
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
    pub known_version: Option<i64>,
    /// dirty 持久化在 SyncMeta 中；内存镜像便于快速判断。
    pub dirty: bool,
    pub automatic_enabled: bool,
    /// 变更代次：任何新变更使旧的上传任务作废。
    pub generation: u64,
    /// 会话代次：登出/切换账号后旧请求结果不得写入新会话。
    pub session_generation: u64,
    /// 单调 epoch：任务凭此判断自己是否已被取代（watch 值变化即失效）。
    pub epoch: u64,
    pub cancel_tx: watch::Sender<u64>,
    /// 上传串行锁：同一时刻只有一个引擎网络流程在跑。
    pub push_lock: Arc<tokio::sync::Mutex<()>>,
}

impl EngineInner {
    /// 失效所有排定的后台动作，返回新 epoch。
    pub fn invalidate(&mut self) -> u64 {
        self.epoch += 1;
        let _ = self.cancel_tx.send(self.epoch);
        self.epoch
    }
    pub fn bump_generation(&mut self) -> u64 {
        self.generation += 1;
        self.invalidate()
    }
    pub fn bump_session(&mut self) {
        self.session_generation += 1;
        self.invalidate();
        self.conflict = None;
        self.last_sync_at = None;
        self.last_error = None;
        self.known_version = None;
        self.state = SyncStateView::Idle;
    }
}

pub struct AppState {
    pub core: Mutex<CoreInner>,
    pub repo: JsonRepo,
    pub settings_repo: JsonRepo,
    /// macOS 旧版数据文件位置（存在性探测用，不直接写）。
    pub legacy_path: Option<PathBuf>,
    pub auth: Mutex<AuthInner>,
    pub engine: RwLock<EngineInner>,
    /// 持久化的 owner（server#username）守卫：切换前要求确认并备份。
    pub owner_archive_counter: AtomicU64,
    /// 事件发射所需的句柄（setup 中注入）。
    pub app_handle: std::sync::RwLock<Option<tauri::AppHandle>>,
    /// 通知数字 id → 任务 id 的映射（点击定位）。
    pub next_notification_id: std::sync::atomic::AtomicI32,
    pub due_notification_map: std::sync::Mutex<std::collections::HashMap<i32, uuid::Uuid>>,
}

impl AppState {
    pub fn new(
        data_dir: PathBuf,
        legacy_path: Option<PathBuf>,
        creds: Arc<dyn CredentialStore>,
    ) -> Self {
        let repo = JsonRepo::new(data_dir.join("data.json"));
        let settings_repo = JsonRepo::new(data_dir.join("settings.json"));
        let (cancel_tx, _) = watch::channel(0);
        Self {
            core: Mutex::new(CoreInner::default()),
            repo,
            settings_repo,
            legacy_path,
            auth: Mutex::new(AuthInner {
                client: None,
                creds,
                logged_in: false,
                is_authenticating: false,
                username: None,
                server_url: None,
            }),
            engine: RwLock::new(EngineInner {
                state: SyncStateView::Idle,
                conflict: None,
                last_sync_at: None,
                last_error: None,
                known_version: None,
                dirty: false,
                automatic_enabled: true,
                generation: 0,
                session_generation: 0,
                epoch: 0,
                cancel_tx,
                push_lock: Arc::new(Mutex::new(())),
            }),
            owner_archive_counter: AtomicU64::new(0),
            app_handle: std::sync::RwLock::new(None),
            next_notification_id: std::sync::atomic::AtomicI32::new(1),
            due_notification_map: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub fn handle(&self) -> tauri::AppHandle {
        self.app_handle
            .read()
            .unwrap()
            .clone()
            .expect("AppHandle 未注入")
    }

    /// 容错句柄：未注入（单元测试）时返回 None，调用方跳过系统副作用。
    pub fn try_handle(&self) -> Option<tauri::AppHandle> {
        self.app_handle.read().ok().and_then(|g| g.clone())
    }
}
