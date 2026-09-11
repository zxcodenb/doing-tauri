use super::{legacy_preferences, problem};
use crate::error::CommandError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
pub fn checksum(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn valid_checksum(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileStamp {
    pub bytes: u64,
    pub modified_unix_nanos: String,
    pub file_identity: Option<String>,
}
impl FileStamp {
    fn of(metadata: &fs::Metadata) -> Result<Self, CommandError> {
        let modified = metadata
            .modified()
            .map_err(|_| problem("sourceUnavailable", "无法读取来源修改时间"))?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| problem("sourceUnavailable", "来源修改时间无效"))?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            Some(format!("{}:{}", metadata.dev(), metadata.ino()))
        };
        #[cfg(not(unix))]
        let identity = None;
        Ok(Self {
            bytes: metadata.len(),
            modified_unix_nanos: modified.as_nanos().to_string(),
            file_identity: identity,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceProof {
    pub path: PathBuf,
    pub format: String,
    pub stamp: FileStamp,
    pub sha256: String,
}

pub struct SourceSnapshot {
    pub proof: SourceProof,
    pub bytes: Vec<u8>,
}
pub fn read_source(path: &Path) -> Result<SourceSnapshot, CommandError> {
    if !path.is_absolute()
        || path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("token.json"))
    {
        return Err(problem(
            "invalidSource",
            "请选择旧版事项 JSON；不会读取旧 Token 文件",
        ));
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| problem("sourceUnavailable", "读取旧文件失败：文件不存在或不可访问"))?;
    if canonical
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("token.json"))
    {
        return Err(problem("invalidSource", "不会读取旧 Token 文件"));
    }
    let file = File::open(&canonical)
        .map_err(|_| problem("sourceUnavailable", "读取旧文件失败：请检查权限"))?;
    let metadata = file
        .metadata()
        .map_err(|_| problem("sourceUnavailable", "无法读取旧文件信息"))?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(problem(
            "invalidSource",
            "旧文件不是常规文件或超过 64 MiB 导入上限",
        ));
    }
    let before = FileStamp::of(&metadata)?;
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| problem("sourceUnavailable", "读取旧文件失败：请检查权限"))?;
    let after = FileStamp::of(
        &file
            .metadata()
            .map_err(|_| problem("sourceUnavailable", "无法复检旧文件"))?,
    )?;
    if before != after || bytes.len() as u64 != before.bytes {
        return Err(problem(
            "sourceChanged",
            "旧文件在读取时变化，请先退出旧版后重试",
        ));
    }
    let proof = SourceProof {
        path: canonical,
        format: "doing-swift-v1-json".into(),
        stamp: before,
        sha256: checksum(&bytes),
    };
    Ok(SourceSnapshot { proof, bytes })
}

/// A defaults domain is read through the OS, not inferred from a possibly stale plist body.
/// The optional backing-file stamp is an additional change detector, not the value authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreferenceProof {
    pub domain: String,
    pub sha256: String,
    pub backing_stamp: Option<FileStamp>,
}
pub struct PreferenceSnapshot {
    pub proof: PreferenceProof,
    pub bytes: Vec<u8>,
}
impl PreferenceSnapshot {
    pub fn selected(raw: &[u8], backing_file: Option<&Path>) -> Result<Self, CommandError> {
        let bytes = legacy_preferences::selected_snapshot(raw)?;
        let backing_stamp = match backing_file.map(fs::metadata) {
            Some(Ok(metadata)) => Some(FileStamp::of(&metadata)?),
            Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(problem("sourceUnavailable", "无法读取旧偏好的修改时间"));
            }
            _ => None,
        };
        let proof = PreferenceProof {
            domain: legacy_preferences::DOMAIN.into(),
            sha256: checksum(&bytes),
            backing_stamp,
        };
        Ok(Self { proof, bytes })
    }
}

pub trait Environment: Send + Sync {
    fn check_legacy_stopped(&self) -> Result<(), CommandError>;
    fn preferences(&self) -> Result<PreferenceSnapshot, CommandError>;
    fn geometry(&self) -> legacy_preferences::Geometry {
        legacy_preferences::Geometry::default()
    }
    fn free_space(&self, directory: &Path) -> Result<u64, CommandError> {
        super::platform::free_space(directory)
    }
}

pub fn read_regular(path: &Path, max: u64) -> Result<Option<Vec<u8>>, CommandError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(problem("migrationIO", "无法读取迁移文件；请检查目录权限")),
    };
    if !metadata.is_file() || metadata.len() > max {
        return Err(problem(
            "invalidMigrationFile",
            "迁移文件不是常规文件、是链接或超过大小限制",
        ));
    }
    let file = File::open(path).map_err(|_| problem("migrationIO", "无法打开迁移文件"))?;
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| problem("migrationIO", "读取迁移文件失败"))?;
    if bytes.len() as u64 > max {
        return Err(problem("invalidMigrationFile", "迁移文件超过大小限制"));
    }
    Ok(Some(bytes))
}
