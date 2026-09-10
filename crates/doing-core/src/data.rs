use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::item::Item;

/// 本地持久化文件当前支持的格式版本。
pub const SCHEMA_VERSION: i64 = 1;

/// 与任务共用同一提交边界的同步元数据。
/// 凭据不落盘；这里只记录“数据归属/基线”，供重启后仲裁与账号切换防护。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[derive(Default)]
pub struct SyncMeta {
    /// 最后同步的服务端地址（明文记录非敏感：不含凭据）。
    pub server_url: Option<String>,
    /// 最后登录的用户名（展示与归属提示用）。
    pub username: Option<String>,
    /// 已确认的服务端快照 version（乐观锁基线）。
    pub known_server_version: Option<i64>,
    /// 本地存在未上传的修改（持久化 dirty，重启后可见）。
    pub dirty: bool,
}


impl SyncMeta {
    /// 账号/服务归属标识：用于切换账号前提示与首次同步仲裁。
    pub fn owner(&self) -> Option<String> {
        match (&self.server_url, &self.username) {
            (Some(url), Some(name)) if !url.is_empty() && !name.is_empty() => {
                Some(format!("{url}#{name}"))
            }
            _ => None,
        }
    }
    pub fn is_unowned(&self) -> bool {
        self.owner().is_none()
    }
}

/// 版本化本地数据文件（原子整体替换，任务与同步元数据同一提交边界）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataFile {
    #[serde(rename = "schemaVersion")]
    pub schema_version: i64,
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
    Data(DataFile),
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
    match value.get("schemaVersion") {
        None => FileParse::Legacy(value),
        Some(serde_json::Value::Number(n)) if n.as_i64() == Some(SCHEMA_VERSION) => {
            match serde_json::from_value::<DataFile>(value) {
                Ok(data) => FileParse::Data(data),
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
            FileParse::Data(back) => assert_eq!(back, data),
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
}
