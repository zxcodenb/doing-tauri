//! IPC 的安全错误协议。内部路径、原始文件内容及凭据错误详情不能跨 WebView 边界。
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "types.gen.ts")]
pub struct CommandError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    /// 乐观锁错误的服务端版本仅以字符串展示，不能经 JS number 往返。
    pub current_version: Option<String>,
}
impl CommandError {
    pub fn new(code: &str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
            current_version: None,
        }
    }
    pub fn input(message: &str) -> Self {
        Self::new("invalidInput", message, false)
    }
    pub fn persistence(message: &str) -> Self {
        Self::new("persistenceFailed", message, true)
    }
    pub fn authentication_required() -> Self {
        Self::new("authenticationRequired", "请先登录后再操作", false)
    }
}
impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for CommandError {}
// 只用于代码中已经面向用户的静态文案；内部 I/O / API 错误必须走下面的显式转换。
impl From<&str> for CommandError {
    fn from(message: &str) -> Self {
        Self::new("operationFailed", message, false)
    }
}
impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::new("operationFailed", message, false)
    }
}
impl From<doing_core::CoreError> for CommandError {
    fn from(error: doing_core::CoreError) -> Self {
        use doing_core::CoreError::*;
        match error {
            AuthenticationRequired => Self::authentication_required(),
            NotFound => Self::new("notFound", "事项不存在或已删除", false),
            NoOp => Self::new("noChange", "没有可保存的修改", false),
            InvalidFocus => Self::input("不能对已完成事项设置焦点"),
            OutOfRange => Self::input("设置值超出范围"),
            UnknownSchema(_) => Self::new(
                "unsupportedSchema",
                "数据由更新版本写入，已阻止降级覆盖",
                false,
            ),
            Corrupt(_) => Self::new(
                "invalidData",
                "数据格式异常，原文件未被覆盖；请检查备份",
                false,
            ),
            Io(_) | Encode(_) => Self::persistence("本地保存失败，请检查磁盘空间或数据目录权限"),
        }
    }
}
impl From<crate::net::dto::ApiError> for CommandError {
    fn from(error: crate::net::dto::ApiError) -> Self {
        use crate::net::dto::ApiError;
        let message = error.display_message().to_owned();
        match error {
            ApiError::Network => Self::new("networkUnavailable", message, true),
            ApiError::SessionChanged => Self::new("sessionChanged", message, false),
            ApiError::CredentialsUnavailable => Self::new("credentialsUnavailable", message, true),
            ApiError::Unauthorized | ApiError::RefreshFailed => {
                Self::new("unauthorized", message, false)
            }
            ApiError::InvalidConfiguration => Self::new("invalidConfiguration", message, false),
            ApiError::InvalidResponse | ApiError::Decoding => {
                Self::new("invalidResponse", message, true)
            }
            ApiError::Http {
                status,
                code,
                current_version,
                ..
            } => Self {
                code: if code.len() <= 64
                    && code.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                {
                    code
                } else {
                    "httpError".into()
                },
                message: message
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(512)
                    .collect(),
                retryable: status == 429 || status >= 500,
                current_version: current_version.map(|v| v.to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn internal_file_content_and_paths_are_not_serialized_to_ipc_errors() {
        for error in [
            doing_core::CoreError::Io("/Users/private/path/secret-file".into()),
            doing_core::CoreError::UnknownSchema(
                serde_json::json!({"token": "private-token", "items": ["private task"]}),
            ),
            doing_core::CoreError::Corrupt("private contents".into()),
        ] {
            let wire = serde_json::to_string(&CommandError::from(error)).unwrap();
            assert!(!wire.contains("private"));
            assert!(wire.contains("retryable"));
            assert!(wire.contains("code"));
        }
    }
    #[test]
    fn server_conflict_code_and_full_width_version_survive_the_error_boundary() {
        let error = CommandError::from(crate::net::dto::ApiError::Http {
            status: 409,
            code: "snapshot_conflict".into(),
            message: "conflict".into(),
            current_version: Some(9_007_199_254_740_993),
        });
        assert_eq!(error.code, "snapshot_conflict");
        assert!(!error.retryable);
        assert_eq!(
            serde_json::to_value(error).unwrap()["currentVersion"],
            "9007199254740993"
        );
        assert!(CommandError::from(crate::net::dto::ApiError::Network).retryable);
    }
}
