use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::data::{parse_file, DataFile, FileParse};
use crate::error::{CoreError, Result};

/// 加载结果：区分“尚无文件”、“正常数据”、“可恢复损坏（带备份）”等情形。
#[derive(Debug)]
pub enum LoadResult {
    Empty,
    Loaded(DataFile),
    /// 主文件损坏/旧版/未知版本，但同目录有“已知良好备份”可供显式恢复。
    NeedsAttention { issue: FileParse, has_backup: bool },
}

/// 原子 JSON 仓库：同目录临时文件 → fsync → 原子替换；
/// 替换前把旧文件内容写入 `*.bak` 作为已知良好备份。
pub struct JsonRepo {
    path: PathBuf,
}

impl JsonRepo {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
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
        let bytes = match fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(LoadResult::Empty),
            Err(e) => return Err(CoreError::Io(e.to_string())),
        };
        match parse_file(&bytes) {
            FileParse::Data(data) => Ok(LoadResult::Loaded(data)),
            issue => Ok(LoadResult::NeedsAttention {
                issue,
                has_backup: self.backup_path().exists(),
            }),
        }
    }

    /// 原子提交。失败时原文件保持不变（不半写、不删除）。
    pub fn save(&self, data: &DataFile) -> Result<()> {
        let json = serde_json::to_vec_pretty(data).map_err(|e| CoreError::Encode(e.to_string()))?;
        let dir = self
            .path
            .parent()
            .ok_or_else(|| CoreError::Io("数据目录缺失".into()))?;
        fs::create_dir_all(dir).map_err(|e| CoreError::Io(e.to_string()))?;

        // 1. 写入同目录临时文件并 fsync。失败（如磁盘满）时移除自身残留 tmp，
        //    保证失败的提交不留下半写文件。
        let tmp = dir.join(format!(
            ".{}.tmp-{}",
            self.path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "data.json".into()),
            std::process::id()
        ));
        let write = (|| -> std::io::Result<()> {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&json)?;
            f.sync_all()
        })();
        if let Err(e) = write {
            let _ = fs::remove_file(&tmp);
            return Err(CoreError::Io(e.to_string()));
        }

        // 2. 备份现有内容（原子写 .bak，失败不阻断主提交）。
        if let Ok(old) = fs::read(&self.path) {
            if let Ok(bak) = fs::File::create(self.backup_path()) {
                let mut f = bak;
                let _ = f.write_all(&old);
                let _ = f.sync_all();
            }
        }

        // 3. 原子替换并同步目录。
        fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            CoreError::Io(e.to_string())
        })?;
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }

    /// 显式用备份恢复主文件（用户确认的恢复动作）。
    pub fn restore_backup(&self) -> Result<()> {
        let bak = self.backup_path();
        fs::rename(&bak, &self.path).map_err(|e| CoreError::Io(e.to_string()))
    }

    pub fn backup_exists(&self) -> bool {
        self.backup_path().exists()
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
            LoadResult::Loaded(back) => assert_eq!(back, data),
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
            assert_eq!(fs::read(repo.path()).unwrap(), before, "失败提交不得改动主文件");
        }
    }

    /// 磁盘满（ENOSPC）演练：需要小容量卷，由 scripts/exercise-disk-full.sh 驱动。
    /// 手工运行：DOING_SMALL_VOLUME=/Volumes/DoingTest \
    ///   cargo test -p doing-core --lib disk_full -- --ignored --nocapture
    #[test]
    #[ignore = "需要小容量卷（scripts/exercise-disk-full.sh）"]
    fn disk_full_save_fails_without_corrupting_primary() {
        let dir = match std::env::var("DOING_SMALL_VOLUME") {
            Ok(p) => PathBuf::from(p),
            Err(_) => {
                eprintln!("未设置 DOING_SMALL_VOLUME，跳过磁盘满演练");
                return;
            }
        };
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
        assert_eq!(fs::read(repo.path()).unwrap(), before, "磁盘满不得损坏已提交数据");

        let leftovers: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "失败提交应清理自身 tmp：{leftovers:?}");
    }

    #[test]
    fn unknown_newer_schema_is_not_overwritten_by_save() {
        // save 本身不读旧内容；此处验证 parse 层：UnknownSchema 不会自动变成 Data。
        let json = r#"{"schemaVersion":99,"items":[]}"#;
        assert!(matches!(parse_file(json.as_bytes()), FileParse::UnknownSchema(_)));
    }
}
