//! 网络 DTO 与服务端契约（与 Go 服务端 /api/v1 一致）。
//! 时间一律以 RFC 3339 字符串交换（避免跨语言精度分歧）。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use doing_core::clock::parse_rfc3339;
use doing_core::Item;

fn fmt(dt: DateTime<Utc>) -> String {
    let base = dt.format("%Y-%m-%dT%H:%M:%S").to_string();
    let nanos = dt.timestamp_subsec_nanos();
    if nanos == 0 {
        format!("{base}Z")
    } else {
        format!("{base}.{:03}Z", nanos / 1_000_000)
    }
}

fn parse(s: &str) -> Result<DateTime<Utc>, String> {
    parse_rfc3339(s).map_err(|e| format!("时间解析失败 {s}: {e}"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemDto {
    pub id: Uuid,
    pub text: String,
    pub done: bool,
    pub created_at: String,
    pub due_date: Option<String>,
    pub updated_at: String,
}

impl From<&Item> for ItemDto {
    fn from(item: &Item) -> Self {
        Self {
            id: item.id,
            text: item.text.clone(),
            done: item.done,
            created_at: fmt(item.created_at),
            due_date: item.due_date.map(fmt),
            updated_at: fmt(item.updated_at),
        }
    }
}

impl ItemDto {
    pub fn to_item(&self) -> Result<Item, String> {
        let created_at = parse(&self.created_at)?;
        let updated_at = parse(&self.updated_at)?;
        let due_date = match &self.due_date {
            Some(s) => Some(parse(s)?),
            None => None,
        };
        Ok(Item {
            id: self.id,
            text: self.text.clone(),
            done: self.done,
            created_at,
            due_date,
            updated_at,
        })
    }
}

/// 云端快照：version 是乐观锁基线（必填），updatedAt 可空。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDto {
    pub items: Vec<ItemDto>,
    pub focus_id: Option<Uuid>,
    pub version: i64,
    pub updated_at: Option<String>,
}

impl SnapshotDto {
    pub fn to_local(&self) -> Result<(Vec<Item>, Option<Uuid>), String> {
        let items = self
            .items
            .iter()
            .map(ItemDto::to_item)
            .collect::<Result<Vec<_>, _>>()?;
        let mut ids = std::collections::HashSet::new();
        if self.version < 0 || items.iter().any(|item| !ids.insert(item.id)) {
            return Err("快照版本或任务标识无效".into());
        }
        let focus = self
            .focus_id
            .filter(|id| items.iter().any(|i| i.id == *id && !i.done));
        Ok((items, focus))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotPutRequest {
    pub items: Vec<ItemDto>,
    pub focus_id: Option<Uuid>,
    pub base_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotPutResponse {
    pub version: i64,
    pub updated_at: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthDto {
    pub access: String,
    pub refresh: String,
    pub expires_in: i64,
}

impl std::fmt::Debug for AuthDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthDto")
            .field("access", &"<redacted>")
            .field("refresh", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// 统一 API 错误：客户端只展示 message 或本地兜底文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    Network,
    SessionChanged,
    CredentialsUnavailable,
    InvalidConfiguration,
    InvalidResponse,
    Decoding,
    Unauthorized,
    RefreshFailed,
    Http {
        status: u16,
        code: String,
        message: String,
        current_version: Option<i64>,
    },
}

impl ApiError {
    pub fn is_conflict(&self) -> bool {
        match self {
            ApiError::Http { status, code, .. } => *status == 409 || code == "snapshot_conflict",
            _ => false,
        }
    }

    pub fn is_auth_failure(&self) -> bool {
        matches!(
            self,
            ApiError::Unauthorized | ApiError::RefreshFailed | ApiError::CredentialsUnavailable
        )
    }

    pub fn display_message(&self) -> &str {
        match self {
            ApiError::Http { message, .. } => message,
            ApiError::Unauthorized | ApiError::RefreshFailed => "登录状态已失效，请重新登录",
            ApiError::Network => "无法连接服务器，请检查网络",
            ApiError::SessionChanged => "会话已变化，请重新操作",
            ApiError::CredentialsUnavailable => "系统安全存储不可用，请检查权限后重新登录",
            ApiError::InvalidConfiguration => {
                "服务地址配置无效；正式环境必须使用 HTTPS，且地址不能包含凭据或查询参数"
            }
            ApiError::InvalidResponse | ApiError::Decoding => "服务器响应异常",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub current_version: Option<i64>,
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;
