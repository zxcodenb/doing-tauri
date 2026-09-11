//! Durable, account-scoped activation routes. Persist BEFORE OS submission, consume only
//! after the main workspace acknowledges navigation. There is no task text or credential here.
use super::NotificationTarget;
use crate::error::CommandError;
use doing_core::repo::write_atomic;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ROUTES: usize = 4096;
fn failure() -> CommandError {
    CommandError::new(
        "notificationStorageUnavailable",
        "通知定位记录无法读取或保存，请保全数据目录后检查文件版本、磁盘空间和权限；提醒资格已保留",
        true,
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Phase {
    Reserved,
    Pending,
    Consumed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Route {
    pub id: Uuid,
    pub target: NotificationTarget,
    phase: Phase,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileData {
    schema_version: u32,
    routes: Vec<Route>,
}
pub(crate) struct Routes {
    path: PathBuf,
}
impl Routes {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    fn lock(&self) -> Result<File, CommandError> {
        let path = self.path.with_extension("lock");
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
            return Err(failure());
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(path).map_err(|_| failure())?;
        file.lock().map_err(|_| failure())?;
        Ok(file)
    }
    fn read(&self) -> Result<FileData, CommandError> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(FileData {
                    schema_version: 1,
                    routes: Vec::new(),
                })
            }
            Err(_) => return Err(failure()),
        };
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err(failure());
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut bytes = Vec::new();
        options
            .open(&self.path)
            .map_err(|_| failure())?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failure())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(failure());
        }
        let value: FileData = serde_json::from_slice(&bytes).map_err(|_| failure())?;
        if value.schema_version != 1 || value.routes.len() > MAX_ROUTES {
            return Err(failure());
        }
        let mut seen = std::collections::HashSet::new();
        if value.routes.iter().any(|r| {
            r.id.is_nil()
                || !seen.insert(r.id)
                || r.target.item_id.is_nil()
                || r.target.owner.account_id.is_empty()
                || r.target.owner.account_id.len() > 128
                || r.target.owner.server_url.is_empty()
                || r.target.owner.server_url.len() > 2048
        }) {
            return Err(failure());
        }
        Ok(value)
    }
    fn save(&self, value: &FileData) -> Result<(), CommandError> {
        let bytes = serde_json::to_vec_pretty(value).map_err(|_| failure())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(failure());
        }
        write_atomic(&self.path, &bytes).map_err(|_| failure())
    }
    pub(super) fn reserve(&self, target: &NotificationTarget) -> Result<Uuid, CommandError> {
        let _lock = self.lock()?;
        let mut data = self.read()?;
        if let Some(route) = data
            .routes
            .iter()
            .find(|r| r.target == *target && r.phase != Phase::Consumed)
        {
            return Ok(route.id);
        }
        // Consumed IDs are never reused. An old OS activation whose tombstone was pruned
        // becomes unknown, not an alias for a later task. Do not evict live routes to make room.
        if data.routes.len() >= MAX_ROUTES {
            data.routes.retain(|r| r.phase != Phase::Consumed);
        }
        if data.routes.len() >= MAX_ROUTES {
            return Err(CommandError::new(
                "notificationStorageFull",
                "待处理通知定位记录已满，请处理已有通知后重试；不会丢弃旧记录或标成已提醒",
                true,
            ));
        }
        let id = loop {
            let id = Uuid::new_v4();
            if !data.routes.iter().any(|r| r.id == id) {
                break id;
            }
        };
        data.routes.push(Route {
            id,
            target: target.clone(),
            phase: Phase::Reserved,
        });
        self.save(&data)?;
        Ok(id)
    }
    pub(crate) fn activate(&self, id: Uuid) -> Result<bool, CommandError> {
        let _lock = self.lock()?;
        let mut data = self.read()?;
        let Some(route) = data.routes.iter_mut().find(|r| r.id == id) else {
            return Ok(false);
        };
        match route.phase {
            Phase::Consumed => Ok(false),
            Phase::Pending => Ok(true),
            Phase::Reserved => {
                route.phase = Phase::Pending;
                self.save(&data)?;
                Ok(true)
            }
        }
    }
    pub(super) fn pending(&self) -> Result<Vec<Route>, CommandError> {
        let _lock = self.lock()?;
        Ok(self
            .read()?
            .routes
            .into_iter()
            .filter(|r| r.phase == Phase::Pending)
            .collect())
    }
    pub(super) fn acknowledge(&self, id: Uuid) -> Result<(), CommandError> {
        let _lock = self.lock()?;
        let mut data = self.read()?;
        if let Some(route) = data
            .routes
            .iter_mut()
            .find(|r| r.id == id && r.phase == Phase::Pending)
        {
            route.phase = Phase::Consumed;
            self.save(&data)?;
        }
        Ok(())
    }
    pub(crate) fn has_pending(&self) -> Result<bool, CommandError> {
        Ok(!self.pending()?.is_empty())
    }
    #[cfg(test)]
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}
