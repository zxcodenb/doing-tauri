//! macOS 旧 Swift 版数据一次性导入（迁移流程子集）。
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

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct MigrationStatus {
    pub available: bool,
    pub detected_file: Option<String>,
    pub imported: bool,
    pub error: Option<String>,
    pub backup_path: Option<String>,
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
    let mut notified: Vec<Uuid> = obj
        .get("notifiedDueIDs")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    notified.dedup();
    Ok(DataFile {
        schema_version: doing_core::data::SCHEMA_VERSION,
        items,
        focus_id,
        notified_due_ids: notified,
        sync: Default::default(),
    })
}

/// 执行迁移：备份旧文件副本 → 原子发布新数据（绝不写回旧文件）。
/// 幂等：新版已有数据时拒绝。
pub fn run_import(
    st: &SyncShared,
    source_path: &std::path::Path,
) -> Result<MigrationStatus, String> {
    match st.repo.load() {
        Ok(doing_core::repo::LoadResult::Empty) => {}
        Ok(doing_core::repo::LoadResult::Loaded(_)) => {
            return Err("新版已初始化，导入需要显式确认且会覆盖现有内容".into());
        }
        Ok(doing_core::repo::LoadResult::NeedsAttention { .. }) => {
            return Err("新版数据文件异常，请先在数据设置中恢复或检查".into());
        }
        Err(e) => return Err(e.to_string()),
    }
    let bytes =
        std::fs::read(source_path).map_err(|e| format!("读取旧文件失败：{e}"))?;
    let data = parse_legacy(&bytes)?;

    let now = SystemClock.now();
    let stamp = now.format("%Y%m%d-%H%M%S").to_string();
    let backup = st
        .repo
        .path()
        .with_file_name(format!("legacy-import-backup-{stamp}.json"));
    std::fs::copy(source_path, &backup).map_err(|e| format!("备份旧文件失败：{e}"))?;

    // 源文件一致性复检（计划 §7.2-2）：导入过程中源文件变化则停止，不发布目标。
    let verify = std::fs::read(source_path).map_err(|e| format!("复检旧文件失败：{e}"))?;
    if verify != bytes {
        return Err("旧文件在导入过程中发生变化，已停止导入（未修改源文件与新版数据）".into());
    }

    // 暂存校验：焦点/提醒规则在 Store::from_data 内收敛。
    let store = Store::from_data(&data);
    if store.items().len() != data.items.len() {
        return Err("迁移校验失败：条目数不一致".into());
    }
    st.repo
        .save(&data)
        .map_err(|e| format!("写入新版数据失败：{e}"))?;

    match st.repo.load() {
        Ok(doing_core::repo::LoadResult::Loaded(_)) => Ok(MigrationStatus {
            available: false,
            detected_file: None,
            imported: true,
            error: None,
            backup_path: Some(backup.display().to_string()),
        }),
        other => Err(format!("迁移后校验失败：{other:?}")),
    }
}

/// 启动探测：发现旧文件且新版空 → 事件提示导入。
pub async fn probe_and_emit(st: &SyncShared) {
    let legacy = st.legacy_path.clone();
    let empty = st.core.lock().await.store.is_empty();
    let available = legacy.as_ref().map(|p| p.exists()).unwrap_or(false) && empty;
    let status = MigrationStatus {
        available,
        detected_file: if available {
            legacy.as_ref().map(|p| p.display().to_string())
        } else {
            None
        },
        imported: false,
        error: None,
        backup_path: None,
    };
    let _ = crate::engine::try_emit(st, EVT_MIGRATION, status);
}

/// 显式导入命令入口（前端确认后调用）。
pub async fn import_legacy_cmd(st: &SyncShared, source: String) -> Result<MigrationStatus, String> {
    let path = std::path::PathBuf::from(source);
    let status = run_import(st, &path)?;
    reload_from_disk(st).await;
    let _ = crate::engine::try_emit(st, EVT_MIGRATION, status.clone());
    Ok(status)
}

/// 从磁盘重载（导入后）：单次切换视图。
async fn reload_from_disk(st: &SyncShared) {
    if let Ok(doing_core::repo::LoadResult::Loaded(data)) = st.repo.load() {
        let mut core = st.core.lock().await;
        core.store = Store::from_data(&data);
        core.meta = data.sync;
        core.save_failed = false;
        drop(core);
        engine::emit_snapshot(st).await;
    }
}

/// 回滚导出（计划 §7.4）：把当前本地数据按旧 Swift v1 兼容格式写出，供旧版读取；
/// 输出到应用数据目录 `exports/`，绝不写回旧文件路径，完成后在 Finder 中揭示。
pub async fn export_legacy(st: &SyncShared) -> Result<String, String> {
    let (items, focus_id, notified) = {
        let core = st.core.lock().await;
        let items: Vec<serde_json::Value> = core
            .store
            .items()
            .iter()
            .map(|it| serde_json::to_value(it).unwrap_or(serde_json::Value::Null))
            .collect();
        (
            items,
            core.store.focus_id(),
            core.store.notified_due_ids().to_vec(),
        )
    };
    let doc = serde_json::json!({
        "items": items,
        "focusID": focus_id.map(|u| u.to_string()),
        "notifiedDueIDs": notified.iter().map(|u| u.to_string()).collect::<Vec<_>>(),
    });
    let stamp = SystemClock.now().format("%Y%m%d-%H%M%S").to_string();
    let dir = st
        .repo
        .path()
        .parent()
        .ok_or_else(|| "数据目录缺失".to_string())?
        .join("exports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建导出目录失败：{e}"))?;
    let path = dir.join(format!("doing-legacy-export-{stamp}.json"));
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("写入导出文件失败：{e}"))?;
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
        assert_eq!(data.notified_due_ids, vec![Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()]);
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
        assert!(parse_legacy(unknown.as_bytes()).unwrap_err().contains("更新的版本"));
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
        let status = import_legacy_cmd(&st, source.clone())
            .await
            .expect("首次导入成功");
        assert!(status.imported);
        assert_eq!(st.core.lock().await.store.items().len(), 2, "内存态已重载");
        let again = import_legacy_cmd(&st, source).await;
        assert!(
            again.unwrap_err().contains("已初始化"),
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
        let back = parse_legacy(&bytes).expect("旧格式解析器应能读回导出文件");
        assert_eq!(back.items.len(), 2);
        assert!(back.focus_id.is_some());
        assert_eq!(back.notified_due_ids.len(), 1);
    }
}
