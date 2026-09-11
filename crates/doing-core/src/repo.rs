use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::data::{parse_file, DataFile, FileParse};
use crate::error::{CoreError, Result};

/// 加载结果：主文件异常必须由用户显式处理，不能退回空库后覆盖原文件。
#[derive(Debug)]
pub enum LoadResult {
    Empty,
    Loaded(Box<DataFile>),
    NeedsAttention { issue: FileParse, has_backup: bool },
}

/// 单一写入方的版本化 JSON 仓库。所有提交在同目录暂存并 fsync，
/// 校验旧主文件后原子轮换已知良好备份，最后原子替换主文件。
/// 锁保证同一个仓库实例不会交错轮换备份；应用层还需串行化内存事务。
pub struct JsonRepo {
    path: PathBuf,
    writer: Mutex<()>,
}

impl JsonRepo {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            writer: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn backup_path(&self) -> PathBuf {
        let mut os = self.path.as_os_str().to_owned();
        os.push(".bak");
        PathBuf::from(os)
    }

    pub fn load(&self) -> Result<LoadResult> {
        let Some(bytes) = read_optional(&self.path)? else {
            return Ok(LoadResult::Empty);
        };
        match parse_file(&bytes) {
            FileParse::Data(data) => Ok(LoadResult::Loaded(data)),
            issue => Ok(LoadResult::NeedsAttention {
                issue,
                has_backup: self.backup_exists(),
            }),
        }
    }

    /// 失败时不会发布未提交的主文件；未知版本、损坏主文件、备份失败均拒绝写入。
    pub fn save(&self, data: &DataFile) -> Result<()> {
        if data.schema_version != crate::data::SCHEMA_VERSION {
            return Err(CoreError::Corrupt(
                "只允许写入当前数据格式，不能降级保存".into(),
            ));
        }

        let _writer = self
            .writer
            .lock()
            .map_err(|_| CoreError::Io("存储锁不可用".into()))?;
        let json = serde_json::to_vec_pretty(data).map_err(|e| CoreError::Encode(e.to_string()))?;
        require_supported(&json)?;
        if let Some(old) = read_optional(&self.path)? {
            // 不能以未知/损坏文件污染已知良好的备份，更不能清空它。
            require_supported(&old)?;
            write_atomic(&self.backup_path(), &old)?;
        }
        write_atomic(&self.path, &json)
    }

    /// 导入只允许初始化空仓库；检查与发布共用写入锁，不能覆盖并发的首次任务提交。
    pub fn initialize(&self, data: &DataFile) -> Result<()> {
        let _writer = self
            .writer
            .lock()
            .map_err(|_| CoreError::Io("存储锁不可用".into()))?;
        if read_optional(&self.path)?.is_some() {
            return Err(CoreError::Corrupt("新版已初始化，拒绝覆盖导入".into()));
        }
        if data.schema_version != crate::data::SCHEMA_VERSION {
            return Err(CoreError::Corrupt("只允许写入当前数据格式".into()));
        }
        let json = serde_json::to_vec_pretty(data).map_err(|e| CoreError::Encode(e.to_string()))?;
        require_supported(&json)?;
        write_new_atomic(&self.path, &json)
    }

    /// 在归属变化/破坏性恢复前保存独立归档，失败必须阻止后续操作。
    pub fn archive_current(&self) -> Result<Option<PathBuf>> {
        let _writer = self
            .writer
            .lock()
            .map_err(|_| CoreError::Io("存储锁不可用".into()))?;
        let Some(current) = read_optional(&self.path)? else {
            return Ok(None);
        };
        require_supported(&current)?;
        let target = self
            .path
            .parent()
            .ok_or_else(|| CoreError::Io("数据目录缺失".into()))?
            .join("archives")
            .join(format!("{}.json", uuid::Uuid::new_v4()));
        write_atomic(&target, &current)?;
        Ok(Some(target))
    }

    /// 显式恢复：验证备份、保留被替换文件的副本，备份本身不被消耗。
    /// 即使用户请求恢复，旧程序也不得写回无法识别的未来格式。
    pub fn restore_backup(&self) -> Result<()> {
        let _writer = self
            .writer
            .lock()
            .map_err(|_| CoreError::Io("存储锁不可用".into()))?;
        let backup = fs::read(self.backup_path()).map_err(io_error)?;
        require_supported(&backup)?;
        if let Some(current) = read_optional(&self.path)? {
            if let FileParse::UnknownSchema(value) = parse_file(&current) {
                return Err(CoreError::UnknownSchema(value));
            }
            let mut archive = self.path.as_os_str().to_owned();
            archive.push(format!(".before-recovery-{}", uuid::Uuid::new_v4()));
            write_atomic(Path::new(&archive), &current)?;
        }
        write_atomic(&self.path, &backup)
    }

    pub fn backup_exists(&self) -> bool {
        fs::read(self.backup_path())
            .is_ok_and(|bytes| matches!(parse_file(&bytes), FileParse::Data(_)))
    }
}

fn io_error(error: std::io::Error) -> CoreError {
    CoreError::Io(error.to_string())
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(e)),
    }
}

fn require_supported(bytes: &[u8]) -> Result<()> {
    match parse_file(bytes) {
        FileParse::Data(_) => Ok(()),
        FileParse::UnknownSchema(value) => Err(CoreError::UnknownSchema(value)),
        FileParse::Legacy(_) => Err(CoreError::Corrupt("请通过迁移向导导入旧格式".into())),
        FileParse::Malformed(_) => Err(CoreError::Corrupt("请先恢复或导出异常数据文件".into())),
    }
}

/// 原子写入一个已由调用方校验的文件（也用于偏好/窗口位置）。
/// 不做删目标文件后重命名的降级；每个写入只清理自己创建的暂存文件。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    stage_and_publish(path, bytes, replace_file)
}

/// 原子发布新的不可覆盖文件（导出/首次初始化）。
/// 已存在的文件、目录或符号链接均导致失败；不采用 exists + rename 的竞态检查。
/// 暂存文件与目标位于同一目录，hard_link 只会把完整已刷新的内容排他发布。
/// 文件系统不支持硬链接时如实失败，绝不降级为直接写入可见目标或覆盖既有文件。
pub fn write_new_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    stage_and_publish(path, bytes, |from, to| fs::hard_link(from, to))
}

fn stage_and_publish(
    path: &Path,
    bytes: &[u8],
    publish: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| CoreError::Io("数据目录缺失".into()))?;
    fs::create_dir_all(dir).map_err(io_error)?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        uuid::Uuid::new_v4(),
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // 创建失败时没有“属于本次写入”的暂存文件，不能去删除同名文件。
    let file = options.open(&tmp).map_err(io_error)?;
    let result = (|| -> std::io::Result<()> {
        let mut file = file;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        publish(&tmp, path)?;
        // rename 后已不存在；no-clobber hard_link 成功后移除本次暂存名。
        // 不能因发布后的清理失败而谎报“未提交”。
        let _ = fs::remove_file(&tmp);
        // 文件内容在发布前已 fsync；目录 fsync 在支持它的平台上进一步固定发布。
        // 发布后的目录刷新不能被误报成“未提交”（否则内存回滚会与文件分叉）。
        #[cfg(unix)]
        if let Ok(parent) = fs::File::open(dir) {
            let _ = parent.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(io_error)
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // 两个路径都在同一目录/卷；明确使用 Win32 的替换语义，不先删除现有文件。
    // COPY_ALLOWED 未启用：不能降级成非原子的跨卷拷贝。
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Item;

    fn repo_in(tmp: &tempfile::TempDir) -> JsonRepo {
        JsonRepo::new(tmp.path().join("data.json"))
    }

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_in(&tmp);
        let mut data = DataFile::new();
        data.items.push(Item {
            created_at: crate::item::fixed_dt(2026, 9, 3, 1, 2, 3),
            updated_at: crate::item::fixed_dt(2026, 9, 3, 1, 2, 3),
            ..Item::new("第一件事")
        });
        repo.save(&data).unwrap();
        // 再次保存产生“上一次内容”的已知良好备份。
        repo.save(&data).unwrap();
        match repo.load().unwrap() {
            LoadResult::Loaded(back) => assert_eq!(*back, data),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(repo.backup_exists());
    }

    #[test]
    fn missing_file_is_empty_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_in(&tmp);
        assert!(matches!(repo.load().unwrap(), LoadResult::Empty));
    }

    #[test]
    fn corrupted_primary_keeps_backup_and_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_in(&tmp);
        let mut data = DataFile::new();
        data.items.push(Item {
            created_at: crate::item::fixed_dt(2026, 9, 3, 1, 2, 3),
            updated_at: crate::item::fixed_dt(2026, 9, 3, 1, 2, 3),
            ..Item::new("正常数据")
        });
        repo.save(&data).unwrap();
        // 第二次保存才会产生“上一次内容”的备份。
        let mut data2 = data.clone();
        data2.items.push(Item {
            created_at: crate::item::fixed_dt(2026, 9, 3, 1, 2, 4),
            updated_at: crate::item::fixed_dt(2026, 9, 3, 1, 2, 4),
            ..Item::new("第二件")
        });
        repo.save(&data2).unwrap();
        fs::write(repo.path(), "{corrupted").unwrap();
        match repo.load().unwrap() {
            LoadResult::NeedsAttention { has_backup, .. } => assert!(has_backup),
            other => panic!("unexpected: {other:?}"),
        }
        repo.restore_backup().unwrap();
        match repo.load().unwrap() {
            LoadResult::Loaded(back) => assert_eq!(back.items.len(), 1),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn legacy_file_reports_needs_attention() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = JsonRepo::new(tmp.path().join("items.json"));
        fs::write(repo.path(), r#"{"items":[],"focusID":null}"#).unwrap();
        match repo.load().unwrap() {
            LoadResult::NeedsAttention { has_backup, .. } => assert!(!has_backup),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn save_over_directory_fails_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = JsonRepo::new(tmp.path().join("target.json"));
        fs::create_dir(repo.path()).unwrap();
        let err = repo.save(&DataFile::new()).unwrap_err();
        assert!(matches!(err, CoreError::Io(_)));
        // 原目录仍在（没有被删除或半写覆盖）。
        assert!(repo.path().is_dir());
    }

    /// 伪中断：上次保存被杀进程留下半写 tmp。下一次 load/save 不受影响，
    /// 已提交数据保持完好。
    #[test]
    fn stale_tmp_from_interrupted_save_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_in(&tmp);
        let mut data = DataFile::new();
        data.items.push(Item::new("已提交"));
        repo.save(&data).unwrap();

        let stale = tmp.path().join(".data.json.tmp-99999");
        fs::write(&stale, "{half-writ").unwrap();

        match repo.load().unwrap() {
            LoadResult::Loaded(back) => assert_eq!(back.items.len(), 1),
            other => panic!("unexpected: {other:?}"),
        }
        let mut next = data.clone();
        next.items.push(Item::new("中断后新增"));
        repo.save(&next).unwrap();
        match repo.load().unwrap() {
            LoadResult::Loaded(back) => assert_eq!(back.items.len(), 2),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// 无写权限：提交失败后主文件内容不变（P1 门禁「写入失败不损坏已提交数据」）。
    #[cfg(unix)]
    #[test]
    fn save_without_write_permission_keeps_primary_intact() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_in(&tmp);
        let mut committed = DataFile::new();
        committed.items.push(Item::new("第一版"));
        repo.save(&committed).unwrap();
        let before = fs::read(repo.path()).unwrap();

        let mut perms = fs::metadata(tmp.path()).unwrap().permissions();
        perms.set_mode(0o555);
        fs::set_permissions(tmp.path(), perms).unwrap();
        // 以 root 运行时权限位无效：探测确认后跳过断言（CI 非 root）。
        let blocked = fs::File::create(tmp.path().join(".probe")).is_err();
        let result = blocked.then(|| {
            let mut second = committed.clone();
            second.items.push(Item::new("第二版"));
            repo.save(&second)
        });

        let mut perms = fs::metadata(tmp.path()).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(tmp.path(), perms).unwrap();

        if let Some(result) = result {
            assert!(matches!(result.unwrap_err(), CoreError::Io(_)));
            assert_eq!(
                fs::read(repo.path()).unwrap(),
                before,
                "失败提交不得改动主文件"
            );
        }
    }

    /// 磁盘满（ENOSPC）演练：需要小容量卷，由 scripts/exercise-disk-full.sh 驱动。
    /// 手工运行：DOING_SMALL_VOLUME=/Volumes/DoingTest \
    ///   cargo test -p doing-core --lib disk_full -- --ignored --nocapture
    #[test]
    #[ignore = "需要小容量卷（scripts/exercise-disk-full.sh）"]
    fn disk_full_save_fails_without_corrupting_primary() {
        let dir = PathBuf::from(
            std::env::var_os("DOING_SMALL_VOLUME")
                .expect("必须通过 scripts/exercise-disk-full.sh 创建隔离小卷，不能空跑为通过"),
        );
        assert!(dir.is_absolute());
        assert_eq!(
            fs::read(dir.join(".doing-disk-full-fixture")).unwrap(),
            b"doing-core-disk-full-fixture-v1"
        );
        assert!(!dir.join("data.json").exists(), "不能覆盖已有数据目录");
        let repo = JsonRepo::new(dir.join("data.json"));
        let mut small = DataFile::new();
        small.items.push(Item::new("磁盘满之前的已提交数据"));
        repo.save(&small).unwrap();
        let before = fs::read(repo.path()).unwrap();

        let mut big = DataFile::new();
        let mut item = Item::new("占位");
        item.text = "x".repeat(4_000_000);
        big.items.push(item);
        let err = repo.save(&big).unwrap_err();
        assert!(matches!(err, CoreError::Io(_)), "应为 IO 错误：{err:?}");
        #[cfg(unix)]
        assert!(
            err.to_string().contains("os error 28"),
            "必须实际触发 ENOSPC：{err}"
        );
        assert_eq!(
            fs::read(repo.path()).unwrap(),
            before,
            "磁盘满不得损坏已提交数据"
        );

        // 导出/初始化使用的新建发布路径同样不得暴露半个文件。
        // 此 FAT 小卷不支持 hard_link，但 4 MiB 写入必须先触发 ENOSPC，而非把“不支持”误记为磁盘满。
        let export = dir.join("new-export.json");
        let err = write_new_atomic(&export, &serde_json::to_vec(&big).unwrap()).unwrap_err();
        assert!(matches!(err, CoreError::Io(_)));
        #[cfg(unix)]
        assert!(
            err.to_string().contains("os error 28"),
            "必须在暂存阶段触发 ENOSPC：{err}"
        );
        assert!(!export.exists(), "导出失败不能留下可见的截断文件");
        assert_eq!(fs::read(repo.path()).unwrap(), before);

        let leftovers: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "失败提交应清理自身 tmp：{leftovers:?}"
        );
    }

    #[test]
    fn unknown_newer_schema_is_not_overwritten_by_save() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_in(&dir);
        let future = br#"{"schemaVersion":99,"items":[],"futureData":"must survive"}"#;
        fs::write(repo.path(), future).unwrap();
        assert!(matches!(
            repo.save(&DataFile::new()),
            Err(CoreError::UnknownSchema(_))
        ));
        assert_eq!(fs::read(repo.path()).unwrap(), future);
        assert!(!repo.backup_exists(), "未知版本不能被当作已知良好备份");
    }

    #[test]
    fn corrupted_primary_is_never_overwritten_by_normal_save() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_in(&dir);
        let mut data = DataFile::new();
        data.items.push(Item::new("可恢复的事项"));
        repo.save(&data).unwrap();
        repo.save(&data).unwrap();
        let backup = fs::read(repo.backup_path()).unwrap();
        fs::write(repo.path(), b"{interrupted").unwrap();
        assert!(matches!(
            repo.save(&DataFile::new()),
            Err(CoreError::Corrupt(_))
        ));
        assert_eq!(fs::read(repo.path()).unwrap(), b"{interrupted");
        assert_eq!(fs::read(repo.backup_path()).unwrap(), backup);
    }

    #[test]
    fn failed_backup_blocks_commit_and_preserves_primary() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_in(&dir);
        repo.save(&DataFile::new()).unwrap();
        let before = fs::read(repo.path()).unwrap();
        fs::create_dir(repo.backup_path()).unwrap();
        let mut next = DataFile::new();
        next.items.push(Item::new("不能失去回滚点"));
        assert!(repo.save(&next).is_err(), "备份未提交时不能报告主提交成功");
        assert_eq!(fs::read(repo.path()).unwrap(), before);
    }

    #[test]
    fn invalid_backup_cannot_replace_primary() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_in(&dir);
        repo.save(&DataFile::new()).unwrap();
        let before = fs::read(repo.path()).unwrap();
        fs::write(repo.backup_path(), b"{bad backup").unwrap();
        assert!(repo.restore_backup().is_err());
        assert_eq!(fs::read(repo.path()).unwrap(), before);
        assert_eq!(fs::read(repo.backup_path()).unwrap(), b"{bad backup");
    }
    #[test]
    fn explicit_recovery_keeps_backup_and_archives_replaced_file() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_in(&dir);
        let mut data = DataFile::new();
        data.items.push(Item::new("备份里的任务"));
        repo.save(&data).unwrap();
        repo.save(&data).unwrap();
        let backup = fs::read(repo.backup_path()).unwrap();
        fs::write(repo.path(), b"{broken primary").unwrap();
        repo.restore_backup().unwrap();
        assert_eq!(fs::read(repo.path()).unwrap(), backup);
        assert_eq!(fs::read(repo.backup_path()).unwrap(), backup);
        let archive = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains(".before-recovery-")
            })
            .expect("恢复前必须保留异常源文件");
        assert_eq!(fs::read(archive.path()).unwrap(), b"{broken primary");
    }

    #[test]
    fn unknown_primary_cannot_be_downgraded_by_backup_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_in(&dir);
        repo.save(&DataFile::new()).unwrap();
        repo.save(&DataFile::new()).unwrap();
        let future = br#"{"schemaVersion":99,"future":"preserve"}"#;
        fs::write(repo.path(), future).unwrap();
        assert!(matches!(
            repo.restore_backup(),
            Err(CoreError::UnknownSchema(_))
        ));
        assert_eq!(fs::read(repo.path()).unwrap(), future);
        assert!(repo.backup_exists());
    }

    #[test]
    fn concurrent_repo_saves_never_publish_partial_json_or_partial_backup() {
        let dir = tempfile::tempdir().unwrap();
        let repo = std::sync::Arc::new(repo_in(&dir));
        let threads: Vec<_> = (0..16)
            .map(|n| {
                let repo = repo.clone();
                std::thread::spawn(move || {
                    let mut data = DataFile::new();
                    data.revision = n;
                    data.items.push(Item::new(format!("writer-{n}")));
                    repo.save(&data).unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(matches!(repo.load().unwrap(), LoadResult::Loaded(_)));
        assert!(repo.backup_exists());
        assert!(fs::read_dir(dir.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".tmp-")));
    }
    #[test]
    fn initialize_cannot_overwrite_an_already_committed_file() {
        let dir = tempfile::tempdir().unwrap();
        let repo = JsonRepo::new(dir.path().join("data.json"));
        let data = DataFile::new();
        repo.initialize(&data).unwrap();
        let before = std::fs::read(repo.path()).unwrap();
        assert!(repo.initialize(&data).is_err());
        assert_eq!(std::fs::read(repo.path()).unwrap(), before);
        assert!(!repo.backup_path().exists(), "被拒绝的初始化不轮换备份");
    }

    #[test]
    fn new_atomic_file_is_complete_and_cannot_replace_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.json");
        let bytes = br#"{"items":[{"text":"complete first snapshot"}]}"#;
        write_new_atomic(&path, bytes).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(write_new_atomic(&path, b"must never overwrite").is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn new_atomic_file_cannot_follow_or_replace_a_target_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("preserve.json");
        let path = dir.path().join("export.json");
        fs::write(&source, b"must survive").unwrap();
        std::os::unix::fs::symlink(&source, &path).unwrap();
        assert!(write_new_atomic(&path, b"new data").is_err());
        assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
        assert_eq!(fs::read(&source).unwrap(), b"must survive");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn independent_repo_initializers_have_only_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let threads: Vec<_> = (0..16)
            .map(|index| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    // Not clones: separate writer locks, as in independent processes.
                    let repo = JsonRepo::new(path);
                    let mut data = DataFile::new();
                    let mut item = Item::new(format!("writer-{index}"));
                    let at = crate::clock::parse_rfc3339("2026-09-11T00:00:00.123Z").unwrap();
                    item.created_at = at;
                    item.updated_at = at;
                    data.items.push(item);
                    barrier.wait();
                    repo.initialize(&data).map(|_| data)
                })
            })
            .collect();
        let winners: Vec<_> = threads
            .into_iter()
            .filter_map(|thread| thread.join().unwrap().ok())
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "check-then-rename must not clobber another initializer"
        );
        let LoadResult::Loaded(actual) = JsonRepo::new(path).load().unwrap() else {
            panic!("missing committed file")
        };
        assert_eq!(*actual, winners[0]);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
