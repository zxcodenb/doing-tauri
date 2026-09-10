//! 凭据持久化：只使用系统安全存储（macOS Keychain / Windows 凭据库）。
//! 安全存储不可用时返回可见错误，绝不回退到明文文件（参照计划 §4.4）。

use serde::{Deserialize, Serialize};

use crate::net::dto::{ApiError, AuthDto};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredError {
    Unavailable,
    Unauthorized,
}

#[derive(Serialize, Deserialize)]
struct StoredTokens {
    access: String,
    refresh: String,
    expires_in: i64,
}

/// 生产用系统安全存储；测试用内存实现。
pub trait CredentialStore: Send + Sync {
    fn load(&self) -> Result<AuthDto, CredError>;
    fn save(&self, tokens: &AuthDto) -> Result<(), CredError>;
    fn clear(&self);
}

pub struct KeychainStore {
    entry: keyring::Entry,
}

impl KeychainStore {
    /// service 使用独立 namespace（开发/Beta/正式互不覆盖，也不与旧 Swift 版共用）。
    pub fn new(namespace: &str) -> Self {
        Self {
            entry: keyring::Entry::new(namespace, "tokens").expect("keyring entry 构造失败"),
        }
    }
}

impl CredentialStore for KeychainStore {
    fn load(&self) -> Result<AuthDto, CredError> {
        let raw = self.entry.get_password().map_err(|_| CredError::Unauthorized)?;
        let stored: StoredTokens =
            serde_json::from_str(&raw).map_err(|_| CredError::Unauthorized)?;
        Ok(AuthDto {
            access: stored.access,
            refresh: stored.refresh,
            expires_in: stored.expires_in,
        })
    }
    fn save(&self, tokens: &AuthDto) -> Result<(), CredError> {
        let raw = serde_json::to_string(&StoredTokens {
            access: tokens.access.clone(),
            refresh: tokens.refresh.clone(),
            expires_in: tokens.expires_in,
        })
        .map_err(|_| CredError::Unavailable)?;
        self.entry
            .set_password(&raw)
            .map_err(|_| CredError::Unavailable)
    }
    fn clear(&self) {
        let _ = self.entry.delete_credential();
    }
}

pub fn api_error_from_cred(err: CredError) -> ApiError {
    match err {
        CredError::Unauthorized => ApiError::Unauthorized,
        CredError::Unavailable => ApiError::RefreshFailed,
    }
}

#[cfg(any(test, debug_assertions))]
pub mod memory {
    use super::*;
    use std::sync::Mutex;

    pub struct MemoryStore(pub Mutex<Option<AuthDto>>);
    impl Default for MemoryStore {
        fn default() -> Self {
            Self(Mutex::new(None))
        }
    }
    impl CredentialStore for MemoryStore {
        fn load(&self) -> Result<AuthDto, CredError> {
            self.0.lock().unwrap().clone().ok_or(CredError::Unauthorized)
        }
        fn save(&self, tokens: &AuthDto) -> Result<(), CredError> {
            *self.0.lock().unwrap() = Some(tokens.clone());
            Ok(())
        }
        fn clear(&self) {
            *self.0.lock().unwrap() = None;
        }
    }
}
