//! 版本化本机偏好与窗口坐标。与任务文件分离，但共享核心锁和“先落盘、后发布”的提交规则。
use crate::error::CommandError;
use crate::events::{SettingsView, EVT_SETTINGS};
use crate::state::{AppState, CoreInner};
use crate::{engine, SyncShared};
use doing_core::AppSettings;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SCHEMA_VERSION: u32 = 1;
const SAVE_ERROR: &str = "偏好保存失败：请检查磁盘空间、目录权限或文件版本；原设置未改变";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowOrigin {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreferencesFile {
    pub(crate) schema_version: u32,
    pub revision: u64,
    pub settings: AppSettings,
    #[serde(default)]
    pub window_origin: Option<WindowOrigin>,
}
impl Default for PreferencesFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            settings: AppSettings::default(),
            window_origin: None,
        }
    }
}
impl PreferencesFile {
    fn from_core(core: &CoreInner) -> Self {
        Self {
            settings: core.settings.clone(),
            revision: core.preferences_revision,
            window_origin: core.window_origin,
            ..Self::default()
        }
    }
}

pub struct PreferencesRepo {
    path: PathBuf,
    writer: Mutex<()>,
}
impl PreferencesRepo {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            writer: Mutex::new(()),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn backup_path(&self) -> PathBuf {
        self.path.with_extension("json.bak")
    }
    pub fn load(&self) -> Result<PreferencesFile, CommandError> {
        let bytes = read_optional(&self.path)?;
        let legacy = bytes.as_ref().is_none_or(|raw| {
            serde_json::from_slice::<serde_json::Value>(raw)
                .is_ok_and(|v| v.get("schemaVersion").is_none())
        });
        let mut file = match bytes {
            Some(bytes) => parse(&bytes)?,
            None => PreferencesFile::default(),
        };
        // 仅兼容旧开发版独立 window.json；新版缺省坐标不重新导入已被重置的旧坐标。
        if legacy {
            let old_origin = self.path.with_file_name("window.json");
            if let Some(bytes) = read_optional(&old_origin)? {
                file.window_origin = Some(serde_json::from_slice(&bytes).map_err(|_| {
                    CommandError::new(
                        "invalidPreferences",
                        "旧窗口位置格式异常，请检查备份",
                        false,
                    )
                })?);
            }
        }
        Ok(file)
    }
    pub fn save(&self, data: &PreferencesFile) -> Result<(), CommandError> {
        let _writer = self
            .writer
            .lock()
            .map_err(|_| CommandError::persistence(SAVE_ERROR))?;
        let json =
            serde_json::to_vec_pretty(data).map_err(|_| CommandError::persistence(SAVE_ERROR))?;
        parse(&json)?;
        if let Some(old) = read_optional(&self.path)? {
            let previous = parse(&old)?;
            if data.revision <= previous.revision {
                return Err(CommandError::new(
                    "preferencesChanged",
                    "偏好已被更新，请重新打开设置后再操作",
                    true,
                ));
            }
            doing_core::repo::write_atomic(&self.backup_path(), &old)
                .map_err(|_| CommandError::persistence(SAVE_ERROR))?;
        }
        doing_core::repo::write_atomic(&self.path, &json)
            .map_err(|_| CommandError::persistence(SAVE_ERROR))
    }
}
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, CommandError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(CommandError::persistence(SAVE_ERROR)),
    }
}
pub(crate) fn parse(bytes: &[u8]) -> Result<PreferencesFile, CommandError> {
    let invalid = || {
        CommandError::new(
            "invalidPreferences",
            "偏好文件格式异常，原文件未被覆盖；请检查备份",
            false,
        )
    };
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if !value.is_object() {
        return Err(invalid());
    }
    let mut file = match value.get("schemaVersion") {
        Some(version) if version.as_u64() == Some(SCHEMA_VERSION as u64) => {
            serde_json::from_value::<PreferencesFile>(value).map_err(|_| invalid())?
        }
        Some(_) => {
            return Err(CommandError::new(
                "unsupportedSchema",
                "偏好由更新版本写入，已阻止降级覆盖",
                false,
            ))
        }
        None => PreferencesFile {
            settings: serde_json::from_value(value).map_err(|_| invalid())?,
            ..PreferencesFile::default()
        },
    };
    file.settings.normalize();
    Ok(file)
}

pub fn load_persisted(st: &AppState) {
    let mut core = st.core.blocking_lock();
    match st.settings_repo.load() {
        Ok(file) => {
            core.settings = file.settings;
            core.preferences_revision = file.revision;
            core.window_origin = file.window_origin;
            core.preferences_error = None;
        }
        Err(error) => {
            // 不能因读取失败而默默把曾关闭的自动上传/通知恢复为开启。
            core.settings.automatic_sync = false;
            core.settings.notifications_enabled = false;
            core.preferences_error = Some(error.message);
        }
    }
}

pub async fn emit_current(st: &SyncShared) {
    let core = st.core.lock().await;
    let view = SettingsView::from_core(&core, st.next_event_revision());
    drop(core);
    let _ = engine::try_emit(st, EVT_SETTINGS, view);
}

async fn commit<F>(st: &SyncShared, change: F) -> Result<SettingsView, CommandError>
where
    F: FnOnce(&mut PreferencesFile) -> Result<(), CommandError>,
{
    let mut e = st.engine.write().await;
    let mut core = st.core.lock().await;
    if core.migration.blocks_writes() {
        return Err(crate::migrate::problem(
            "migrationPending",
            "请先完成或处理待恢复迁移，偏好未修改",
        ));
    }
    let before = PreferencesFile::from_core(&core);
    let mut next = before.clone();
    change(&mut next)?;
    next.settings.normalize();
    if next == before {
        return Ok(SettingsView::from_core(&core, st.next_event_revision()));
    }
    next.revision = before
        .revision
        .checked_add(1)
        .ok_or_else(|| CommandError::persistence("偏好修订号已耗尽"))?;
    let saved = st.settings_repo.save(&next);
    if let Err(error) = &saved {
        core.preferences_error = Some(error.message.clone());
    } else {
        core.settings = next.settings.clone();
        core.preferences_revision = next.revision;
        core.window_origin = next.window_origin;
        core.preferences_error = None;
        if before.settings.automatic_sync && !core.settings.automatic_sync {
            e.invalidate();
        }
    }
    let view = SettingsView::from_core(&core, st.next_event_revision());
    drop(core);
    drop(e);
    let _ = engine::try_emit(st, EVT_SETTINGS, view.clone());
    saved?;
    if before.settings.mode != next.settings.mode {
        crate::desktop::apply_mode_after_commit(st).await;
    }
    if before.settings != next.settings {
        if !before.settings.notifications_enabled
            && next.settings.notifications_enabled
            && st.try_handle().is_some()
        {
            crate::request_notification_permission_if_needed(st).await;
        }
        crate::reminder::refresh(st).await;
        crate::desktop::refresh_tray(st).await;
        if !before.settings.automatic_sync && next.settings.automatic_sync {
            engine::auto_flush(st).await;
        }
    }
    Ok(view)
}
pub async fn update<F>(st: &SyncShared, change: F) -> Result<SettingsView, CommandError>
where
    F: FnOnce(&mut AppSettings) -> Result<(), CommandError>,
{
    commit(st, |file| change(&mut file.settings)).await
}
pub async fn reset(st: &SyncShared) -> Result<SettingsView, CommandError> {
    commit(st, |file| {
        file.settings = AppSettings::default();
        file.window_origin = None;
        Ok(())
    })
    .await
}
pub async fn save_origin(st: &SyncShared, origin: WindowOrigin) -> Result<(), CommandError> {
    commit(st, |file| {
        file.window_origin = Some(origin);
        Ok(())
    })
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    fn state() -> (SyncShared, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let st = Arc::new(AppState::new(
            dir.path().into(),
            None,
            Arc::new(crate::creds::memory::MemoryStore::default()),
        ));
        (st, dir)
    }
    #[tokio::test]
    async fn window_coordinates_and_preferences_commit_together_and_survive_reload() {
        let (st, _dir) = state();
        update(&st, |s| {
            s.mode = "panel".into();
            s.automatic_sync = false;
            Ok(())
        })
        .await
        .unwrap();
        save_origin(&st, WindowOrigin { x: -1920, y: 180 })
            .await
            .unwrap();
        let saved = st.settings_repo.load().unwrap();
        assert_eq!(saved.revision, 2);
        assert_eq!(saved.window_origin, Some(WindowOrigin { x: -1920, y: 180 }));
        assert!(!saved.settings.automatic_sync);
        assert_eq!(saved.settings.mode, "panel");
        assert_eq!(st.core.lock().await.preferences_revision, 2);
    }
    #[tokio::test]
    async fn backup_failure_rolls_back_preference_and_window_origin() {
        let (st, _dir) = state();
        save_origin(&st, WindowOrigin { x: 10, y: 20 })
            .await
            .unwrap();
        let before = std::fs::read(st.settings_repo.path()).unwrap();
        std::fs::create_dir(st.settings_repo.backup_path()).unwrap();
        assert!(save_origin(&st, WindowOrigin { x: 80, y: 90 })
            .await
            .is_err());
        assert_eq!(
            st.core.lock().await.window_origin,
            Some(WindowOrigin { x: 10, y: 20 })
        );
        assert_eq!(std::fs::read(st.settings_repo.path()).unwrap(), before);
        assert!(st.core.lock().await.preferences_error.is_some());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_preference_transactions_never_split_disk_and_memory() {
        let (st, _dir) = state();
        let mut jobs = Vec::new();
        for index in 1..=30 {
            let st = st.clone();
            jobs.push(tokio::spawn(async move {
                update(&st, |s| {
                    s.menu_bar_text_limit = index;
                    s.appearance = doing_core::AppAppearance::Dark;
                    Ok(())
                })
                .await
                .unwrap();
            }));
        }
        for job in jobs {
            job.await.unwrap();
        }
        let core = st.core.lock().await;
        let saved = st.settings_repo.load().unwrap();
        assert_eq!(saved.settings, core.settings);
        assert_eq!(saved.revision, core.preferences_revision);
        assert!(saved.revision > 1);
    }
    #[test]
    fn old_development_preferences_and_window_file_are_read_without_rewriting() {
        let (st, dir) = state();
        let preferences = br#"{"appearance":"dark","automaticSync":false,"mode":"panel"}"#;
        let window = br#"{"x":-1200,"y":90}"#;
        std::fs::write(st.settings_repo.path(), preferences).unwrap();
        std::fs::write(dir.path().join("window.json"), window).unwrap();
        let loaded = st.settings_repo.load().unwrap();
        assert_eq!(loaded.revision, 0);
        assert_eq!(loaded.window_origin, Some(WindowOrigin { x: -1200, y: 90 }));
        assert!(!loaded.settings.automatic_sync);
        assert_eq!(std::fs::read(st.settings_repo.path()).unwrap(), preferences);
        assert_eq!(
            std::fs::read(dir.path().join("window.json")).unwrap(),
            window
        );
    }
    #[test]
    fn unreadable_or_future_preferences_disable_automatic_side_effects_and_report_error() {
        for bytes in [
            b"{corrupt".as_slice(),
            br#"{"schemaVersion":100}"#.as_slice(),
        ] {
            let (st, _dir) = state();
            std::fs::write(st.settings_repo.path(), bytes).unwrap();
            load_persisted(&st);
            let core = st.core.blocking_lock();
            assert!(!core.settings.automatic_sync);
            assert!(!core.settings.notifications_enabled);
            assert!(core.preferences_error.is_some());
            assert_eq!(std::fs::read(st.settings_repo.path()).unwrap(), bytes);
        }
    }
    #[test]
    fn overflowing_window_coordinates_are_rejected_instead_of_wrapping() {
        let (st, dir) = state();
        let bytes = br#"{"x":9223372036854775807,"y":90}"#;
        std::fs::write(dir.path().join("window.json"), bytes).unwrap();
        assert!(st.settings_repo.load().is_err());
        assert_eq!(
            std::fs::read(dir.path().join("window.json")).unwrap(),
            bytes
        );
    }
}
