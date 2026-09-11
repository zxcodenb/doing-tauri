use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::item::Item;

/// 本地持久化文件当前支持的格式版本。
pub const SCHEMA_VERSION: i64 = 2;

/// 与任务共用同一提交边界的同步元数据。
/// 凭据不落盘；这里只记录“数据归属/基线”，供重启后仲裁与账号切换防护。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[derive(Default)]
pub struct SyncMeta {
    /// 最后同步的服务端地址（明文记录非敏感：不含凭据）。
    pub server_url: Option<String>,
    /// 最后登录的用户名（展示与归属提示用）。
    pub username: Option<String>,
    /// 服务端 access JWT 的 sub，仅用于本地隔离，不替代服务器认证。
    pub account_id: Option<String>,
    /// 已确认的服务端快照 version（乐观锁基线）。
    pub known_server_version: Option<i64>,
    /// 本地存在未上传的修改（持久化 dirty，重启后可见）。
    pub dirty: bool,
    /// 最近一次任务编辑的本地修订；提醒等本机元数据不能使 PUT 确认误判为新任务修改。
    pub local_edit_revision: u64,
    /// 有候选或候选尚未拉取时都必须持久化，重启不能绕过仲裁。
    pub pending_conflict: Option<PendingConflict>,
    /// 在 PUT 发出前提交；崩溃/超时后必须先 GET 重新确认基线。
    pub uncertain_upload: Option<UploadMarker>,
}

/// 服务地址与稳定账号 ID 共同定义归属；用户名仅作展示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountOwner {
    pub server_url: String,
    pub account_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudData {
    pub items: Vec<Item>,
    pub focus_id: Option<Uuid>,
    pub version: i64,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictReason {
    Ownership,
    Initial,
    RemoteChanged,
    LocalChanged,
    UploadUncertain,
    Restore,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingConflict {
    pub candidate_id: Uuid,
    pub owner: AccountOwner,
    pub reason: ConflictReason,
    /// None 表示必须先拉取候选，仍然阻断任何上传。
    pub cloud: Option<CloudData>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadMarker {
    pub id: Uuid,
    pub owner: AccountOwner,
    pub base_version: i64,
    pub revision: u64,
}

impl SyncMeta {
    pub fn account_owner(&self) -> Option<AccountOwner> {
        Some(AccountOwner {
            server_url: self.server_url.clone()?,
            account_id: self.account_id.clone()?,
        })
    }
    pub fn belongs_to(&self, owner: &AccountOwner) -> bool {
        self.account_owner().as_ref() == Some(owner)
    }
    pub fn claim(&mut self, owner: &AccountOwner, username: &str) {
        self.server_url = Some(owner.server_url.clone());
        self.account_id = Some(owner.account_id.clone());
        self.username = Some(username.to_owned());
    }
}

/// 版本化本地数据文件（原子整体替换，任务与同步元数据同一提交边界）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataFile {
    #[serde(rename = "schemaVersion")]
    pub schema_version: i64,
    /// 与任务及同步元数据同时提交的本地修订号。旧开发文件缺省为 0。
    #[serde(default)]
    pub revision: u64,
    pub items: Vec<Item>,
    /// 注意：本地文件键是 focusID（旧版沿袭），API 契约是 focusId。
    #[serde(rename = "focusID")]
    pub focus_id: Option<Uuid>,
    #[serde(rename = "notifiedDueIDs")]
    pub notified_due_ids: Vec<Uuid>,
    #[serde(default)]
    pub sync: SyncMeta,
}

impl DataFile {
    pub fn new() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            items: Vec::new(),
            focus_id: None,
            notified_due_ids: Vec::new(),
            sync: SyncMeta::default(),
        }
    }
}

impl Default for DataFile {
    fn default() -> Self {
        Self::new()
    }
}

/// 解析结果：能区分“旧版格式（可迁移）”、“新版未知格式（只读，不覆盖）”与损坏。
#[derive(Debug)]
pub enum FileParse {
    Data(Box<DataFile>),
    /// 没有 schemaVersion：是旧 Swift 格式或任意导入 JSON。
    Legacy(serde_json::Value),
    /// 有 schemaVersion 但不认识：不允许降级写回。
    UnknownSchema(serde_json::Value),
    Malformed(String),
}

pub fn parse_file(bytes: &[u8]) -> FileParse {
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => return FileParse::Malformed(format!("JSON 解析失败: {e}")),
    };
    let has_edit_revision = value.pointer("/sync/localEditRevision").is_some();
    match value.get("schemaVersion") {
        None => FileParse::Legacy(value),
        Some(serde_json::Value::Number(n)) if matches!(n.as_i64(), Some(1 | SCHEMA_VERSION)) => {
            match serde_json::from_value::<DataFile>(value) {
                Ok(mut data) => {
                    if data.schema_version == 1 {
                        // v1 没有稳定账号 ID/冲突持久化，不从旧用户名猜测授权归属。
                        data.schema_version = SCHEMA_VERSION;
                        data.sync.account_id = None;
                        data.sync.pending_conflict = None;
                        data.sync.uncertain_upload = None;
                    }
                    // 对缺少此字段的开发文件保守处理：已有 dirty 不会被旧 PUT 提前确认。
                    if data.sync.dirty && !has_edit_revision {
                        data.sync.local_edit_revision = data.revision;
                    }
                    FileParse::Data(Box::new(data))
                }
                Err(e) => FileParse::Malformed(format!("字段不完整或类型错误: {e}")),
            }
        }
        Some(_) => FileParse::UnknownSchema(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_data_file_roundtrip() {
        let data = DataFile::new();
        let json = serde_json::to_string_pretty(&data).unwrap();
        match parse_file(json.as_bytes()) {
            FileParse::Data(back) => assert_eq!(*back, data),
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn legacy_snapshot_is_detected() {
        let json = r#"{"items":[],"focusID":null,"notifiedDueIDs":[]}"#;
        assert!(matches!(parse_file(json.as_bytes()), FileParse::Legacy(_)));
    }

    #[test]
    fn unknown_schema_version_is_detected() {
        let json = r#"{"schemaVersion":99,"items":[]}"#;
        assert!(matches!(
            parse_file(json.as_bytes()),
            FileParse::UnknownSchema(_)
        ));
    }

    #[test]
    fn malformed_json_is_detected() {
        assert!(matches!(parse_file(b"not json"), FileParse::Malformed(_)));
    }
    #[test]
    fn v1_upgrade_preserves_data_and_baseline_but_never_infers_stable_ownership() {
        let raw = br#"{"schemaVersion":1,"revision":7,"items":[],"focusID":null,"notifiedDueIDs":[],"sync":{"serverUrl":"https://api.example.test","username":"alice","knownServerVersion":9,"dirty":true}}"#;
        let FileParse::Data(data) = parse_file(raw) else {
            panic!("支持读取 v1")
        };
        assert_eq!(data.schema_version, SCHEMA_VERSION);
        assert_eq!(data.revision, 7);
        assert_eq!(data.sync.local_edit_revision, 7);
        assert!(data.sync.account_owner().is_none());
        assert_eq!(data.sync.known_server_version, Some(9));
        assert!(data.sync.dirty);
        assert!(data.sync.pending_conflict.is_none());
        assert!(data.sync.uncertain_upload.is_none());
    }

    #[test]
    fn v2_persists_candidate_identity_and_uncertain_write_with_exact_i64_versions() {
        let mut data = DataFile::new();
        let owner = AccountOwner {
            server_url: "https://api.example.test".into(),
            account_id: "42".into(),
        };
        data.revision = 7;
        data.sync.claim(&owner, "alice");
        data.sync.dirty = true;
        data.sync.local_edit_revision = 7;
        data.sync.pending_conflict = Some(PendingConflict {
            candidate_id: Uuid::new_v4(),
            owner: owner.clone(),
            reason: ConflictReason::UploadUncertain,
            cloud: Some(CloudData {
                items: vec![],
                focus_id: None,
                version: 9_007_199_254_740_993,
                updated_at: None,
            }),
        });
        data.sync.uncertain_upload = Some(UploadMarker {
            id: Uuid::new_v4(),
            owner,
            base_version: 9_007_199_254_740_992,
            revision: 7,
        });
        let FileParse::Data(back) = parse_file(&serde_json::to_vec(&data).unwrap()) else {
            panic!("有效 v2")
        };
        assert_eq!(*back, data);
    }
}
