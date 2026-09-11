//! macOS 旧 Swift 版事项与可选 UserDefaults 偏好的可恢复一次性导入。
//! 规则（计划 §7.2）：检测→备份→解析→转换→暂存校验→原子提交→幂等；
//! 只在“新版尚未初始化”时自动进入；绝不修改/删除旧文件。

use serde::Serialize;
use ts_rs::TS;
use uuid::Uuid;

use doing_core::clock::SystemClock;
use doing_core::data::{parse_file, DataFile, FileParse};
use doing_core::store::Store;
use doing_core::Clock;

use crate::engine;
use crate::events::EVT_MIGRATION;
use crate::SyncShared;

mod journal;
mod legacy_preferences;
mod platform;
mod source;
#[cfg(test)]
mod transaction_tests;

use crate::error::CommandError;
use crate::state::{AppState, CoreInner};
use journal::{Coordinator, Journal, Phase};
use std::path::Path;

pub(crate) fn problem(code: &str, message: &str) -> CommandError {
    CommandError::new(code, message, true)
}

#[derive(Clone, Default)]
pub struct MigrationRuntime {
    journal: Option<Journal>,
    invalid_journal: bool,
    error: Option<CommandError>,
}
impl MigrationRuntime {
    pub fn blocks_writes(&self) -> bool {
        self.invalid_journal || self.journal.as_ref().is_some_and(|j| j.phase.active())
    }
    pub fn requires_login(&self) -> bool {
        self.blocks_writes() || self.journal.as_ref().is_some_and(|j| j.requires_login)
    }
}
fn coordinator(st: &AppState) -> Coordinator {
    Coordinator::new(
        st.repo
            .path()
            .parent()
            .expect("app data parent")
            .to_path_buf(),
    )
}
fn runtime(st: &AppState) -> MigrationRuntime {
    match coordinator(st).read() {
        Ok(journal) => MigrationRuntime {
            error: journal
                .as_ref()
                .and_then(|j| j.error.as_ref())
                .map(|e| problem(&e.code, &e.message)),
            journal,
            invalid_journal: false,
        },
        Err(error) => MigrationRuntime {
            journal: None,
            invalid_journal: true,
            error: Some(error),
        },
    }
}
fn native_preferences_available(st: &AppState, source: &Path) -> bool {
    cfg!(target_os = "macos")
        && st.legacy_path.as_ref().is_some_and(|old| {
            old.canonicalize()
                .ok()
                .zip(source.canonicalize().ok())
                .is_some_and(|(a, b)| a == b)
        })
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct MigrationStatus {
    pub event_revision: u64,
    pub available: bool,
    pub detected_file: Option<String>,
    pub imported: bool,
    pub error: Option<String>,
    pub backup_path: Option<String>,
    #[ts(type = "string | null")]
    pub transaction_id: Option<Uuid>,
    pub recovery_required: bool,
    pub preferences_available: bool,
    pub imported_preferences: bool,
    pub requires_login: bool,
    pub warnings: Vec<String>,
}

/// 解析旧 Swift v1 快照（宽松字段兜底）→ 新格式 DataFile。
/// 重复/缺失标识在迁移层显式报错，不静默跳过任务。
pub fn parse_legacy(bytes: &[u8]) -> Result<DataFile, String> {
    let value = match parse_file(bytes) {
        FileParse::Legacy(value) => value,
        FileParse::Data(_) => return Err("数据已是新版格式".into()),
        FileParse::UnknownSchema(_) => {
            return Err("目标数据由更新的版本写入，无法迁移".into());
        }
        FileParse::Malformed(e) => return Err(format!("旧数据损坏：{e}")),
    };
    let obj = value
        .as_object()
        .ok_or_else(|| "旧数据不是 JSON 对象".to_string())?;
    let raw_items = obj
        .get("items")
        .ok_or_else(|| "旧数据缺少 items".to_string())?
        .clone();
    let items: Vec<doing_core::Item> =
        serde_json::from_value(raw_items).map_err(|e| format!("任务解析失败：{e}"))?;
    let mut seen = std::collections::HashSet::new();
    for item in &items {
        if !seen.insert(item.id) {
            return Err(format!("旧数据包含重复任务标识 {}", item.id));
        }
    }
    let focus_id: Option<Uuid> = obj
        .get("focusID")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .filter(|id| items.iter().any(|it| it.id == *id && !it.done));
    let mut notified: Vec<Uuid> = match obj.get("notifiedDueIDs") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| "旧提醒记录包含无效标识".to_owned())?,
    };
    let mut notified_seen = std::collections::HashSet::new();
    notified.retain(|id| seen.contains(id) && notified_seen.insert(*id));
    Ok(DataFile {
        schema_version: doing_core::data::SCHEMA_VERSION,
        revision: 0,
        items,
        focus_id,
        notified_due_ids: notified,
        sync: Default::default(),
    })
}

/// Snapshot from cached authoritative migration state; callers already hold the core lock.
pub fn current_status(st: &AppState, core: &CoreInner, event_revision: u64) -> MigrationStatus {
    let migration = &core.migration;
    let journal = migration.journal.as_ref();
    let source = journal.map(|j| &j.source_path).or(st.legacy_path.as_ref());
    let empty = matches!(st.repo.load(), Ok(doing_core::repo::LoadResult::Empty));
    let available =
        !migration.blocks_writes() && empty && source.is_some_and(|path| path.is_file());
    MigrationStatus {
        event_revision,
        available,
        detected_file: source.map(|path| path.display().to_string()),
        imported: journal.is_some_and(|j| j.phase == Phase::Committed),
        error: migration.error.as_ref().map(|e| e.message.clone()),
        backup_path: journal.map(|j| {
            coordinator(st)
                .backup_path(j.transaction_id)
                .join("legacy-items.json")
                .display()
                .to_string()
        }),
        transaction_id: journal.map(|j| j.transaction_id),
        recovery_required: migration.blocks_writes(),
        preferences_available: source.is_some_and(|path| native_preferences_available(st, path)),
        imported_preferences: journal
            .and_then(|j| j.summary.as_ref())
            .is_some_and(|s| s.imported_preferences),
        requires_login: migration.requires_login(),
        warnings: journal
            .and_then(|j| j.summary.as_ref())
            .map(|s| s.warnings.clone())
            .unwrap_or_default(),
    }
}
pub async fn probe_and_emit(st: &SyncShared) {
    let core = st.core.lock().await;
    let status = current_status(st, &core, st.next_event_revision());
    drop(core);
    let _ = engine::try_emit(st, EVT_MIGRATION, status);
}

/// Runs before task/preferences loading and before any credential restoration/bootstrap.
pub fn recover_before_load(st: &AppState) {
    let state = runtime(st);
    if let Some(journal) = state.journal.as_ref().filter(|j| j.phase.active()) {
        let environment = platform::NativeEnvironment {
            app: st.try_handle(),
            allow_preferences: native_preferences_available(st, &journal.source_path),
        };
        let _ = recover_with_environment(st, &environment);
    } else {
        st.core.blocking_lock().migration = state;
    }
}
fn recover_with_environment(
    st: &AppState,
    environment: &dyn source::Environment,
) -> Result<(), CommandError> {
    let state = runtime(st);
    let result = if let Some(journal) = state.journal.as_ref().filter(|j| j.phase.active()) {
        let mut e = st.engine.blocking_write();
        let mut auth = st.auth.blocking_lock();
        coordinator(st)
            .resume(
                journal.transaction_id,
                environment,
                &mut || crate::auth::reset_for_migration(&mut e, &mut auth),
                &mut (),
            )
            .map(|_| ())
    } else {
        Ok(())
    };
    let mut state = runtime(st);
    if let Err(error) = &result {
        state.error = Some(error.clone());
    }
    st.core.blocking_lock().migration = state;
    result
}

/// Enforced independently of OS credential availability, so a failed Keychain deletion cannot restore a pre-import session.
pub(crate) fn acknowledge_new_login(
    st: &AppState,
    core: &mut CoreInner,
) -> Result<(), CommandError> {
    if core.migration.blocks_writes() {
        return Err(problem(
            "migrationPending",
            "请先完成、撤销或处理待恢复迁移",
        ));
    }
    if core.migration.requires_login() {
        let result = coordinator(st).acknowledge_new_login();
        core.migration = runtime(st);
        result?;
    }
    Ok(())
}

pub(crate) fn fence_incomplete(core: &mut CoreInner) {
    if core.migration.blocks_writes() {
        core.save_failed = true;
        core.settings.automatic_sync = false;
        core.settings.notifications_enabled = false;
        core.preferences_error =
            Some("旧版导入尚未完成；已暂停任务、偏好和同步写入，请恢复、撤销或保留当前文件".into());
    }
}

#[derive(Clone)]
enum Action {
    Import {
        source: std::path::PathBuf,
        preferences: bool,
    },
    Resume(Uuid),
    Cancel(Uuid),
    KeepCurrent(Uuid),
}
async fn run_action(
    st: &SyncShared,
    action: Action,
    environment: &dyn source::Environment,
) -> Result<MigrationStatus, CommandError> {
    run_action_observed(st, action, environment, &mut ()).await
}

async fn run_action_observed(
    st: &SyncShared,
    action: Action,
    environment: &dyn source::Environment,
    observer: &mut dyn journal::Observer,
) -> Result<MigrationStatus, CommandError> {
    // Network completions and all ordinary writers need these same locks. Rejected/no-op
    // commands must not invalidate their work, revoke a lease, or reload away undo history.
    let mut e = st.engine.write().await;
    let mut auth = st.auth.lock().await;
    let mut core = st.core.lock().await;
    let coordinator = coordinator(st);
    let before = coordinator.read()?;
    let revision = match &action {
        Action::Import { .. } => {
            if !core.store.is_empty() || core.meta.dirty {
                return Err(problem(
                    "targetInitialized",
                    "新版已有本地内容，不能用首次导入覆盖现有工作",
                ));
            }
            core.store
                .revision()
                .checked_add(1)
                .ok_or_else(|| problem("migrationValidationFailed", "本地修订号已耗尽"))?
        }
        Action::Resume(id) | Action::Cancel(id) | Action::KeepCurrent(id) => {
            let journal = before
                .as_ref()
                .filter(|j| j.transaction_id == *id)
                .ok_or_else(|| problem("migrationChanged", "迁移记录已变化，旧操作已取消"))?;
            if matches!(action, Action::Cancel(_))
                && matches!(journal.phase, Phase::Committed | Phase::KeptCurrent)
            {
                return Err(problem(
                    "migrationChanged",
                    "此迁移已结束，不能撤销之后的新工作",
                ));
            }
            if !journal.phase.active()
                && !core.migration.blocks_writes()
                && core.migration.journal.as_ref() == Some(journal)
            {
                return Ok(current_status(st, &core, st.next_event_revision()));
            }
            0 // Recovery reuses the staged revision; it must not allocate a new one.
        }
    };
    let was_blocked = core.migration.blocks_writes();
    let mut authorized = false;
    let result = {
        // Publication calls this only after validation and a durable reauthentication fence,
        // and BEFORE touching targets. Preflight failures must leave the current session alone.
        let mut authorize = || {
            authorized = true;
            crate::auth::reset_for_migration(&mut e, &mut auth)
        };
        match action {
            Action::Import {
                source,
                preferences,
            } => coordinator.begin(
                &source,
                preferences,
                revision,
                environment,
                &mut authorize,
                observer,
            ),
            Action::Resume(id) => coordinator.resume(id, environment, &mut authorize, observer),
            Action::Cancel(id) => coordinator.cancel(id, observer),
            Action::KeepCurrent(id) => coordinator.keep_current(id),
        }
    };
    let mut after = runtime(st);
    let blocked = after.blocks_writes();
    let phase_key = |j: &Journal| (j.transaction_id, j.phase);
    let published_transition = after.journal.as_ref().is_some_and(|j| {
        matches!(
            j.phase,
            Phase::Committed | Phase::Cancelled | Phase::KeptCurrent
        ) && before.as_ref().map(phase_key) != Some(phase_key(j))
    });
    // The durable phase, not Result::is_ok(), decides authority. An error after the final
    // marker must still install the committed pair; an error before staging must not reload.
    let reload = !blocked && (was_blocked || published_transition);
    if !authorized && !reload && !blocked && before == after.journal {
        return result.map(|_| current_status(st, &core, st.next_event_revision()));
    }
    if let Err(error) = &result {
        after.error = Some(error.clone());
    }
    core.migration = after;
    if blocked {
        // Do not expose a half-published data/preferences pair to editable state.
        fence_incomplete(&mut core);
    } else if reload {
        if core.migration.requires_login() && !authorized {
            let _ = crate::auth::reset_for_migration(&mut e, &mut auth);
            authorized = true;
        }
        match st.repo.load() {
            Ok(doing_core::repo::LoadResult::Loaded(data)) => {
                core.store = Store::from_data(&data);
                core.meta = data.sync;
                core.save_failed = false;
            }
            Ok(doing_core::repo::LoadResult::Empty) => {
                core.store = Store::new();
                core.meta = Default::default();
                core.save_failed = false;
            }
            _ => {
                core.save_failed = true;
            }
        }
        match st.settings_repo.load() {
            Ok(file) => {
                core.settings = file.settings;
                core.preferences_revision = file.revision;
                core.window_origin = file.window_origin;
                core.preferences_error = None;
            }
            Err(error) => {
                core.settings.automatic_sync = false;
                core.settings.notifications_enabled = false;
                core.preferences_error = Some(error.message);
            }
        }
        core.store.clear_history();
    }
    let changed = authorized || reload || (blocked && !was_blocked);
    if changed {
        if !authorized {
            e.operation += 1;
        }
        // Cancellation/keep-current also hold the engine lock and begin behind the active
        // migration gate. Invalidate before releasing it, so no old completion can commit.
        e.bump_generation();
        engine::refresh_view(
            &mut e,
            &core,
            auth.client
                .as_ref()
                .and_then(|c| c.lease())
                .map(|l| &l.identity.owner),
        );
    }
    let status = current_status(st, &core, st.next_event_revision());
    drop(core);
    drop(auth);
    drop(e);
    if changed {
        crate::auth::emit_auth(st).await;
        engine::emit_snapshot(st).await;
        engine::emit_sync_state(st).await;
        crate::preferences::emit_current(st).await;
    }
    let _ = engine::try_emit(st, EVT_MIGRATION, status.clone());
    if reload {
        crate::desktop::apply_mode_after_commit(st).await;
        crate::desktop::refresh_tray(st).await;
        let _ = crate::reminder::permission_view(st, false).await;
        // Query live OS state; never enable a login item or inherit notification authorization here.
        if let Some(handle) = st.try_handle() {
            use tauri_plugin_autostart::ManagerExt;
            let _ = handle.autolaunch().is_enabled();
        }
    }
    result.map(|_| status)
}

pub async fn import_legacy_cmd(
    st: &SyncShared,
    source: String,
    preferences: bool,
) -> Result<MigrationStatus, CommandError> {
    let source = std::path::PathBuf::from(source);
    let environment = platform::NativeEnvironment {
        app: st.try_handle(),
        allow_preferences: native_preferences_available(st, &source),
    };
    if preferences && !environment.allow_preferences {
        return Err(problem("invalidSource", "此来源不允许继承本机旧偏好"));
    }
    run_action(
        st,
        Action::Import {
            source,
            preferences,
        },
        &environment,
    )
    .await
}
pub async fn resume_cmd(st: &SyncShared, id: Uuid) -> Result<MigrationStatus, CommandError> {
    let journal = coordinator(st)
        .read()?
        .ok_or_else(|| problem("migrationChanged", "没有待恢复迁移"))?;
    let environment = platform::NativeEnvironment {
        app: st.try_handle(),
        allow_preferences: native_preferences_available(st, &journal.source_path),
    };
    run_action(st, Action::Resume(id), &environment).await
}
pub async fn cancel_cmd(st: &SyncShared, id: Uuid) -> Result<MigrationStatus, CommandError> {
    let environment = platform::NativeEnvironment {
        app: st.try_handle(),
        allow_preferences: false,
    };
    run_action(st, Action::Cancel(id), &environment).await
}
pub async fn keep_current_cmd(st: &SyncShared, id: Uuid) -> Result<MigrationStatus, CommandError> {
    let environment = platform::NativeEnvironment {
        app: st.try_handle(),
        allow_preferences: false,
    };
    run_action(st, Action::KeepCurrent(id), &environment).await
}

#[cfg(test)]
fn run_import(st: &SyncShared, source_path: &Path) -> Result<MigrationStatus, String> {
    coordinator(st)
        .begin(
            source_path,
            false,
            1,
            &transaction_tests::FileEnvironment::default(),
            &mut || Ok(()),
            &mut (),
        )
        .map_err(|e| e.message)?;
    let mut core = st.core.blocking_lock();
    core.migration = runtime(st);
    Ok(current_status(st, &core, st.next_event_revision()))
}
#[cfg(test)]
pub(crate) async fn import_fixture(
    st: &SyncShared,
    source: String,
) -> Result<MigrationStatus, CommandError> {
    run_action(
        st,
        Action::Import {
            source: source.into(),
            preferences: false,
        },
        &transaction_tests::FileEnvironment::default(),
    )
    .await
}

/// 回滚导出（计划 §7.4）：把当前本地数据按旧 Swift v1 兼容格式写出，供旧版读取；
/// 输出到应用数据目录 `exports/`，绝不写回旧文件路径，完成后在 Finder 中揭示。
pub async fn export_legacy(st: &SyncShared) -> Result<String, String> {
    export_legacy_at(st, SystemClock.now()).await
}

async fn export_legacy_at(
    st: &SyncShared,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String, String> {
    #[derive(Serialize)]
    struct LegacyExport<'a> {
        items: &'a [doing_core::Item],
        #[serde(rename = "focusID")]
        focus_id: Option<Uuid>,
        #[serde(rename = "notifiedDueIDs")]
        notified_due_ids: &'a [Uuid],
    }
    let bytes = {
        let core = st.core.lock().await;
        let doc = LegacyExport {
            items: core.store.items(),
            focus_id: core.store.focus_id(),
            notified_due_ids: core.store.notified_due_ids(),
        };
        // 只序列化旧任务字段；不含账号、同步候选、Token 或本机偏好。
        // 失败不能以 null 替代某项后假装导出成功。
        serde_json::to_vec_pretty(&doc).map_err(|_| "导出内容序列化失败".to_owned())?
    };
    let stamp = now.format("%Y%m%d-%H%M%S").to_string();
    let dir = st
        .repo
        .path()
        .parent()
        .ok_or_else(|| "数据目录缺失".to_string())?
        .join("exports");
    let path = dir.join(format!(
        "doing-legacy-export-{stamp}-{}.json",
        Uuid::new_v4()
    ));
    doing_core::repo::write_new_atomic(&path, &bytes).map_err(|_| {
        "写入导出文件失败：请检查空间、权限或文件系统能力；已有导出未被覆盖".to_owned()
    })?;
    if let Some(handle) = st.try_handle() {
        let _ = tauri_plugin_opener::OpenerExt::opener(&handle)
            .reveal_item_in_dir(path.to_str().unwrap_or_default());
    }
    Ok(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creds::CredentialStore;
    use doing_core::repo::LoadResult;
    use std::sync::Arc;

    const LEGACY: &str = r#"{"items":[
        {"id":"11111111-1111-1111-1111-111111111111","text":"旧版事项","done":false,"createdAt":"2026-09-01T00:00:00Z","dueDate":null,"updatedAt":"2026-09-01T01:00:00Z"},
        {"id":"22222222-2222-2222-2222-222222222222","text":"完成的","done":true,"createdAt":"2026-09-01T00:00:00Z"}
    ],"focusID":"11111111-1111-1111-1111-111111111111","notifiedDueIDs":["11111111-1111-1111-1111-111111111111"]}"#;

    fn state_with(dir: &tempfile::TempDir) -> Arc<crate::state::AppState> {
        let creds = Arc::new(crate::creds::memory::MemoryStore::default());
        Arc::new(crate::state::AppState::new(
            dir.path().to_path_buf(),
            None,
            creds as Arc<dyn CredentialStore>,
        ))
    }

    #[test]
    fn parses_legacy_fields_and_defaults() {
        let data = parse_legacy(LEGACY.as_bytes()).expect("解析成功");
        assert_eq!(data.items.len(), 2);
        assert_eq!(data.items[0].text, "旧版事项");
        assert_eq!(
            data.notified_due_ids,
            vec![Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()]
        );
        // 第二项缺少 updatedAt：兜底为 createdAt。
        assert_eq!(data.items[1].updated_at, data.items[1].created_at);
        assert!(data.items[1].done);
    }

    #[test]
    fn rejects_duplicate_ids() {
        let dup = r#"{"items":[
            {"id":"11111111-1111-1111-1111-111111111111","text":"a","createdAt":"2026-09-01T00:00:00Z"},
            {"id":"11111111-1111-1111-1111-111111111111","text":"b","createdAt":"2026-09-01T00:00:00Z"}
        ]}"#;
        let err = parse_legacy(dup.as_bytes()).unwrap_err();
        assert!(err.contains("重复"), "{err}");
    }

    #[test]
    fn rejects_corrupt_and_unknown_schema() {
        assert!(parse_legacy(b"not-json").unwrap_err().contains("损坏"));
        let unknown = r#"{"schemaVersion":99,"items":[]}"#;
        assert!(parse_legacy(unknown.as_bytes())
            .unwrap_err()
            .contains("更新的版本"));
    }

    #[test]
    fn invalid_focus_is_cleared() {
        let json = r#"{"items":[{"id":"11111111-1111-1111-1111-111111111111","text":"做完","done":true,"createdAt":"2026-09-01T00:00:00Z"}],"focusID":"11111111-1111-1111-1111-111111111111"}"#;
        let data = parse_legacy(json.as_bytes()).unwrap();
        assert_eq!(data.focus_id, None, "已完成任务不能成为焦点");
    }

    #[test]
    fn run_import_backs_up_commits_and_leaves_source_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_with(&dir);
        let source = dir.path().join("legacy-items.json");
        std::fs::write(&source, LEGACY).unwrap();
        let before = std::fs::read(&source).unwrap();

        let status = run_import(&st, &source).expect("导入成功");
        assert!(status.imported);
        let backup = status.backup_path.clone().unwrap();
        assert!(std::path::Path::new(&backup).exists(), "应有备份文件");
        // 原子发布后可读回。
        match st.repo.load().unwrap() {
            LoadResult::Loaded(data) => {
                assert_eq!(data.items.len(), 2);
                assert_eq!(data.items[0].text, "旧版事项");
            }
            other => panic!("unexpected {other:?}"),
        }
        // 原文件不被修改。
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }

    #[test]
    fn run_import_refuses_when_target_initialized() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_with(&dir);
        let source = dir.path().join("legacy-items.json");
        std::fs::write(&source, LEGACY).unwrap();
        // 目标已有内容。
        let mut data = DataFile::new();
        data.items.push(doing_core::Item::new("已有"));
        st.repo.save(&data).unwrap();
        let err = run_import(&st, &source).unwrap_err();
        assert!(err.contains("已初始化"), "{err}");
    }

    #[test]
    fn run_import_reports_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_with(&dir);
        let missing = dir.path().join("no-such-file.json");
        let err = run_import(&st, &missing).unwrap_err();
        assert!(err.contains("读取旧文件失败"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn run_import_on_readonly_target_fails_without_touching_source() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let st = state_with(&dir);
        let source = dir.path().join("legacy-items.json");
        std::fs::write(&source, LEGACY).unwrap();
        let before = std::fs::read(&source).unwrap();

        // 目标目录置为只读：备份步骤应失败并如实报错，源文件保持不变。
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(dir.path(), perms).unwrap();
        let result = run_import(&st, &source);
        // 恢复权限以便 TempDir 清理。
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(dir.path(), perms).unwrap();

        let err = result.unwrap_err();
        assert!(
            err.contains("备份旧文件失败") || err.contains("写入新版数据失败"),
            "{err}"
        );
        assert_eq!(std::fs::read(&source).unwrap(), before, "源文件不得被修改");
    }

    #[test]
    fn run_import_refuses_when_target_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_with(&dir);
        let source = dir.path().join("legacy-items.json");
        std::fs::write(&source, LEGACY).unwrap();
        // 目标文件损坏：必须先引导修复，而不是被迁移覆盖。
        std::fs::write(st.repo.path(), "{corrupted").unwrap();
        let err = run_import(&st, &source).unwrap_err();
        assert!(err.contains("数据文件异常"), "{err}");
        assert_eq!(
            std::fs::read(st.repo.path()).unwrap(),
            b"{corrupted",
            "损坏的目标文件不得被导入流程改写"
        );
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures")
            .join(name)
    }

    #[test]
    fn parses_repo_fixture_legacy_file() {
        let bytes = std::fs::read(fixture("legacy-items.json")).unwrap();
        let data = parse_legacy(&bytes).expect("fixture 解析");
        assert_eq!(data.items.len(), 2);
        assert_eq!(data.items[0].text, "旧版事项 A");
        assert!(data.focus_id.is_some());
        assert_eq!(data.notified_due_ids.len(), 1);
    }

    #[tokio::test]
    async fn import_cmd_publishes_then_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_with(&dir);
        let source = fixture("legacy-items.json").display().to_string();
        let status = import_fixture(&st, source.clone())
            .await
            .expect("首次导入成功");
        assert!(status.imported);
        assert_eq!(st.core.lock().await.store.items().len(), 2, "内存态已重载");
        let again = import_fixture(&st, source).await;
        assert!(
            again.unwrap_err().message.contains("已有本地内容"),
            "重复导入必须被幂等拒绝"
        );
    }

    #[tokio::test]
    async fn export_legacy_roundtrips_and_stays_out_of_old_path() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_with(&dir);
        {
            let mut core = st.core.lock().await;
            let now = SystemClock.now();
            let a = core.store.add("导出 A", None, now).unwrap().id.unwrap();
            core.store.add("导出 B", None, now).unwrap();
            core.store.mark_notified(a, now);
        }
        let path = export_legacy(&st).await.expect("导出成功");
        assert!(path.contains("exports"), "{path}");
        assert!(std::path::Path::new(&path).exists(), "导出文件应存在");
        let bytes = std::fs::read(&path).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let fields: std::collections::BTreeSet<_> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            std::collections::BTreeSet::from(["items", "focusID", "notifiedDueIDs"])
        );
        let back = parse_legacy(&bytes).expect("旧格式解析器应能读回导出文件");
        assert_eq!(back.items.len(), 2);
        assert!(back.focus_id.is_some());
        assert_eq!(back.notified_due_ids.len(), 1);
    }

    #[tokio::test]
    async fn repeated_exports_in_the_same_second_keep_each_complete_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let st = state_with(&directory);
        let original = parse_legacy(LEGACY.as_bytes()).unwrap();
        st.core.lock().await.store = Store::from_data(&original);
        let now = doing_core::clock::parse_rfc3339("2026-09-11T01:02:03Z").unwrap();
        let first = export_legacy_at(&st, now).await.unwrap();
        let first_bytes = std::fs::read(&first).unwrap();
        let id = original.items[0].id;
        st.core
            .lock()
            .await
            .store
            .rename(id, "下一份导出，不可覆盖上一份", now)
            .unwrap();
        let second = export_legacy_at(&st, now).await.unwrap();
        assert_ne!(first, second, "同一秒导出不能覆盖先前已交付文件");
        assert_eq!(std::fs::read(&first).unwrap(), first_bytes);
        assert_eq!(parse_legacy(&first_bytes).unwrap().items, original.items);
        assert_eq!(
            parse_legacy(&std::fs::read(&second).unwrap())
                .unwrap()
                .items[0]
                .text,
            "下一份导出，不可覆盖上一份"
        );
        assert_eq!(
            std::fs::read_dir(directory.path().join("exports"))
                .unwrap()
                .count(),
            2
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_export_leaves_previous_files_and_memory_untouched() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let st = state_with(&directory);
        st.core.lock().await.store = Store::from_data(&parse_legacy(LEGACY.as_bytes()).unwrap());
        let original = st.core.lock().await.to_data_file();
        let exports = directory.path().join("exports");
        std::fs::create_dir(&exports).unwrap();
        let previous = exports.join("previous.json");
        std::fs::write(&previous, b"previous complete export").unwrap();
        std::fs::set_permissions(&exports, std::fs::Permissions::from_mode(0o500)).unwrap();
        let outcome = export_legacy(&st).await;
        std::fs::set_permissions(&exports, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(outcome.is_err());
        assert_eq!(
            std::fs::read(previous).unwrap(),
            b"previous complete export"
        );
        assert_eq!(std::fs::read_dir(exports).unwrap().count(), 1);
        assert_eq!(st.core.lock().await.to_data_file(), original);
    }
}
