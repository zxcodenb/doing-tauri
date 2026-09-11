//! 系统安全存储 + 会话凭据租约。HTTP 客户端永远不持有可无条件改写的凭据仓库。
//! 租约检查与读/写/删除同处一把锁，认证切换后旧请求不能读到或覆盖新账号 Token。
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base64::Engine;
use doing_core::data::AccountOwner;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::net::dto::{ApiError, AuthDto};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredError {
    Unavailable,
    Unauthorized,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountIdentity {
    pub owner: AccountOwner,
    pub username: String,
}
impl AccountIdentity {
    /// 只读取服务器已返回 Token 的隔离信息；绝不据此授权请求，也不持有 JWT 签名密钥。
    pub fn from_tokens(server_url: &str, tokens: &AuthDto) -> Result<Self, CredError> {
        if tokens.access.len() > 32_768
            || tokens.refresh.is_empty()
            || tokens.refresh.len() > 32_768
            || tokens.expires_in <= 0
        {
            return Err(CredError::Unauthorized);
        }
        let parts: Vec<_> = tokens.access.split('.').collect();
        if parts.len() != 3 {
            return Err(CredError::Unauthorized);
        }
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_| CredError::Unauthorized)?;
        let claims: serde_json::Value =
            serde_json::from_slice(&decoded).map_err(|_| CredError::Unauthorized)?;
        let sub = claims
            .get("sub")
            .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
            .filter(|id| *id > 0)
            .ok_or(CredError::Unauthorized)?;
        let username = claims
            .get("username")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty() && v.len() <= 64)
            .ok_or(CredError::Unauthorized)?;
        if claims.get("type").and_then(|v| v.as_str()) != Some("access") {
            return Err(CredError::Unauthorized);
        }
        Ok(Self {
            owner: AccountOwner {
                server_url: crate::net::client::normalize_base_url(server_url)
                    .map_err(|_| CredError::Unauthorized)?,
                account_id: sub.to_string(),
            },
            username: username.to_owned(),
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedSession {
    pub schema_version: u32,
    pub session_id: Uuid,
    pub identity: AccountIdentity,
    pub tokens: AuthDto,
}
impl SavedSession {
    pub fn new(server_url: &str, tokens: AuthDto) -> Result<Self, CredError> {
        Ok(Self {
            schema_version: 1,
            session_id: Uuid::new_v4(),
            identity: AccountIdentity::from_tokens(server_url, &tokens)?,
            tokens,
        })
    }
    fn validate(&self) -> Result<(), CredError> {
        if self.schema_version != 1
            || self.session_id.is_nil()
            || AccountIdentity::from_tokens(&self.identity.owner.server_url, &self.tokens)?
                != self.identity
        {
            return Err(CredError::Unauthorized);
        }
        Ok(())
    }
}

pub trait CredentialStore: Send + Sync {
    fn load(&self) -> Result<SavedSession, CredError>;
    fn save(&self, session: &SavedSession) -> Result<(), CredError>;
    fn clear(&self) -> Result<(), CredError>;
}

/// 本地控制文件只存随机会话 ID，不包含 Token/用户名。与 Keychain 记录匹配才可恢复。
/// 保存中断、登出后系统凭据删除失败、旧 Token-only 格式，都必须 fail closed。
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionControl {
    session_id: Option<Uuid>,
}

// 只抽象系统秘密条目的 I/O，控制文件和原子提交仍走真实生产路径，供隔离故障注入验证。
trait SecretBackend: Send + Sync {
    fn get(&self) -> Result<String, CredError>;
    fn set(&self, value: &str) -> Result<(), CredError>;
    fn delete(&self) -> Result<(), CredError>;
}
struct KeychainBackend(keyring::Entry);
impl SecretBackend for KeychainBackend {
    fn get(&self) -> Result<String, CredError> {
        self.0.get_password().map_err(|error| match error {
            keyring::Error::NoEntry => CredError::Unauthorized,
            _ => CredError::Unavailable,
        })
    }
    fn set(&self, value: &str) -> Result<(), CredError> {
        self.0
            .set_password(value)
            .map_err(|_| CredError::Unavailable)
    }
    fn delete(&self) -> Result<(), CredError> {
        match self.0.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(CredError::Unavailable),
        }
    }
}

pub struct KeychainStore {
    secret: Arc<dyn SecretBackend>,
    control_path: PathBuf,
}
impl KeychainStore {
    pub fn new(namespace: &str, data_dir: &std::path::Path) -> Self {
        Self {
            secret: Arc::new(KeychainBackend(
                keyring::Entry::new(namespace, "tokens").expect("keyring entry 构造失败"),
            )),
            control_path: data_dir.join("auth-session.json"),
        }
    }
    fn write_control(&self, id: Option<Uuid>) -> Result<(), CredError> {
        let bytes = serde_json::to_vec(&SessionControl { session_id: id })
            .map_err(|_| CredError::Unavailable)?;
        doing_core::repo::write_atomic(&self.control_path, &bytes)
            .map_err(|_| CredError::Unavailable)
    }
}
impl CredentialStore for KeychainStore {
    fn load(&self) -> Result<SavedSession, CredError> {
        let bytes = std::fs::read(&self.control_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CredError::Unauthorized
            } else {
                CredError::Unavailable
            }
        })?;
        let control: SessionControl =
            serde_json::from_slice(&bytes).map_err(|_| CredError::Unauthorized)?;
        let id = control.session_id.ok_or(CredError::Unauthorized)?;
        let raw = self.secret.get()?;
        let session: SavedSession =
            serde_json::from_str(&raw).map_err(|_| CredError::Unauthorized)?;
        if session.session_id != id {
            return Err(CredError::Unauthorized);
        }
        session.validate()?;
        Ok(session)
    }
    fn save(&self, session: &SavedSession) -> Result<(), CredError> {
        session.validate()?;
        let raw = serde_json::to_string(session).map_err(|_| CredError::Unavailable)?;
        self.secret.set(&raw)?;
        if self.write_control(Some(session.session_id)).is_err() {
            let _ = self.secret.delete();
            return Err(CredError::Unavailable);
        }
        Ok(())
    }
    fn clear(&self) -> Result<(), CredError> {
        // 先阻断恢复。即使删除系统条目失败，下次启动也不会恢复已登出的会话。
        let _blocked = self.write_control(None);
        // 删除成功即已安全退出；删除失败仍报告错误，但成功写入的 None 会阻断重启恢复。
        self.secret.delete()
    }
}

#[derive(Default)]
struct VaultState {
    generation: u64,
    active: Option<Uuid>,
}
pub struct CredentialVault {
    store: Arc<dyn CredentialStore>,
    state: Mutex<VaultState>,
}
#[derive(Clone)]
pub struct SessionLease {
    vault: Arc<CredentialVault>,
    generation: u64,
    pub id: Uuid,
    pub identity: AccountIdentity,
}
impl CredentialVault {
    pub fn new(store: Arc<dyn CredentialStore>) -> Arc<Self> {
        Arc::new(Self {
            store,
            state: Mutex::new(VaultState::default()),
        })
    }
    pub fn revoke(&self) -> Result<(), CredError> {
        let mut state = self.state.lock().map_err(|_| CredError::Unavailable)?;
        state.generation += 1;
        state.active = None;
        self.store.clear()
    }
    pub fn activate(self: &Arc<Self>, session: SavedSession) -> Result<SessionLease, CredError> {
        session.validate()?;
        let mut state = self.state.lock().map_err(|_| CredError::Unavailable)?;
        state.generation += 1;
        state.active = None;
        if let Err(error) = self.store.save(&session) {
            let _ = self.store.clear();
            return Err(error);
        }
        state.active = Some(session.session_id);
        Ok(SessionLease {
            vault: self.clone(),
            generation: state.generation,
            id: session.session_id,
            identity: session.identity,
        })
    }
    pub fn restore(self: &Arc<Self>, server_url: &str) -> Result<SessionLease, CredError> {
        let mut state = self.state.lock().map_err(|_| CredError::Unavailable)?;
        state.generation += 1;
        state.active = None;
        let session = self.store.load()?;
        session.validate()?;
        let server_url = crate::net::client::normalize_base_url(server_url)
            .map_err(|_| CredError::Unauthorized)?;
        if session.identity.owner.server_url != server_url {
            return Err(CredError::Unauthorized);
        }
        state.active = Some(session.session_id);
        Ok(SessionLease {
            vault: self.clone(),
            generation: state.generation,
            id: session.session_id,
            identity: session.identity,
        })
    }
}
impl SessionLease {
    fn checked(&self, state: &VaultState) -> Result<SavedSession, CredError> {
        if state.generation != self.generation || state.active != Some(self.id) {
            return Err(CredError::Stale);
        }
        let session = self.vault.store.load()?;
        session.validate()?;
        if session.session_id != self.id || session.identity != self.identity {
            return Err(CredError::Stale);
        }
        Ok(session)
    }
    pub fn load(&self) -> Result<AuthDto, CredError> {
        let state = self
            .vault
            .state
            .lock()
            .map_err(|_| CredError::Unavailable)?;
        Ok(self.checked(&state)?.tokens)
    }
    pub fn is_current(&self) -> bool {
        self.load().is_ok()
    }
    pub fn is_active(&self) -> bool {
        self.vault
            .state
            .lock()
            .is_ok_and(|s| s.generation == self.generation && s.active == Some(self.id))
    }
    pub fn replace_tokens(
        &self,
        used_refresh: &str,
        tokens: AuthDto,
    ) -> Result<AuthDto, CredError> {
        let state = self
            .vault
            .state
            .lock()
            .map_err(|_| CredError::Unavailable)?;
        let mut session = self.checked(&state)?;
        if session.tokens.refresh != used_refresh {
            return Ok(session.tokens);
        }
        if AccountIdentity::from_tokens(&self.identity.owner.server_url, &tokens)? != self.identity
        {
            return Err(CredError::Unauthorized);
        }
        session.tokens = tokens.clone();
        self.vault.store.save(&session)?;
        Ok(tokens)
    }
    /// 失败的旧刷新不能清掉已经轮换的同会话 Token，更不能影响新会话。
    pub fn reject_refresh(&self, used_refresh: &str) -> Result<Option<AuthDto>, CredError> {
        let mut state = self
            .vault
            .state
            .lock()
            .map_err(|_| CredError::Unavailable)?;
        let session = self.checked(&state)?;
        if session.tokens.refresh != used_refresh {
            return Ok(Some(session.tokens));
        }
        state.generation += 1;
        state.active = None;
        self.vault.store.clear()?;
        Ok(None)
    }
}

pub fn api_error_from_cred(error: CredError) -> ApiError {
    match error {
        CredError::Stale => ApiError::SessionChanged,
        CredError::Unavailable => ApiError::CredentialsUnavailable,
        CredError::Unauthorized => ApiError::Unauthorized,
    }
}

#[cfg(any(test, debug_assertions))]
pub mod memory {
    use super::*;
    #[derive(Default)]
    pub struct MemoryStore(pub Mutex<Option<SavedSession>>);
    impl CredentialStore for MemoryStore {
        fn load(&self) -> Result<SavedSession, CredError> {
            self.0
                .lock()
                .unwrap()
                .clone()
                .ok_or(CredError::Unauthorized)
        }
        fn save(&self, session: &SavedSession) -> Result<(), CredError> {
            *self.0.lock().unwrap() = Some(session.clone());
            Ok(())
        }
        fn clear(&self) -> Result<(), CredError> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::test_server::tokens_for;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct FakeSecret {
        value: Mutex<Option<String>>,
        fail_get: AtomicBool,
        fail_set: AtomicBool,
        fail_delete: AtomicBool,
    }
    impl SecretBackend for FakeSecret {
        fn get(&self) -> Result<String, CredError> {
            if self.fail_get.load(Ordering::SeqCst) {
                return Err(CredError::Unavailable);
            }
            self.value
                .lock()
                .unwrap()
                .clone()
                .ok_or(CredError::Unauthorized)
        }
        fn set(&self, value: &str) -> Result<(), CredError> {
            if self.fail_set.load(Ordering::SeqCst) {
                return Err(CredError::Unavailable);
            }
            *self.value.lock().unwrap() = Some(value.to_owned());
            Ok(())
        }
        fn delete(&self) -> Result<(), CredError> {
            if self.fail_delete.load(Ordering::SeqCst) {
                return Err(CredError::Unavailable);
            }
            *self.value.lock().unwrap() = None;
            Ok(())
        }
    }
    fn store(dir: &tempfile::TempDir, secret: &Arc<FakeSecret>) -> KeychainStore {
        KeychainStore {
            secret: secret.clone(),
            control_path: dir.path().join("auth-session.json"),
        }
    }
    fn saved() -> SavedSession {
        SavedSession::new(
            "https://api.example.test",
            tokens_for(1, "alice", "test-only"),
        )
        .unwrap()
    }

    #[test]
    fn secure_roundtrip_uses_only_a_random_control_id_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let secret = Arc::new(FakeSecret::default());
        let session = saved();
        store(&dir, &secret).save(&session).unwrap();
        let restarted = store(&dir, &secret);
        assert_eq!(restarted.load().unwrap().identity, session.identity);
        let control = std::fs::read_to_string(&restarted.control_path).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&control).unwrap(),
            serde_json::json!({ "sessionId": session.session_id })
        );
        for private in [
            &session.tokens.access,
            &session.tokens.refresh,
            &session.identity.username,
        ] {
            assert!(!control.contains(private));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&restarted.control_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn legacy_tokens_missing_or_mismatched_control_never_restore() {
        let dir = tempfile::tempdir().unwrap();
        let secret = Arc::new(FakeSecret::default());
        let store = store(&dir, &secret);
        let session = saved();
        secret
            .set(&serde_json::to_string(&session.tokens).unwrap())
            .unwrap();
        assert!(matches!(store.load(), Err(CredError::Unauthorized)));
        store.write_control(Some(session.session_id)).unwrap();
        assert!(matches!(store.load(), Err(CredError::Unauthorized)));
        secret
            .set(&serde_json::to_string(&session).unwrap())
            .unwrap();
        store.write_control(Some(Uuid::new_v4())).unwrap();
        assert!(matches!(store.load(), Err(CredError::Unauthorized)));
        store.write_control(None).unwrap();
        assert!(matches!(store.load(), Err(CredError::Unauthorized)));
    }

    #[test]
    fn failed_secure_save_does_not_fall_back_to_a_plaintext_file() {
        let dir = tempfile::tempdir().unwrap();
        let secret = Arc::new(FakeSecret::default());
        secret.fail_set.store(true, Ordering::SeqCst);
        assert_eq!(
            store(&dir, &secret).save(&saved()),
            Err(CredError::Unavailable)
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert!(secret.value.lock().unwrap().is_none());
    }

    #[test]
    fn failed_control_commit_removes_the_attempted_secret() {
        let dir = tempfile::tempdir().unwrap();
        let secret = Arc::new(FakeSecret::default());
        let store = store(&dir, &secret);
        std::fs::create_dir(&store.control_path).unwrap();
        assert_eq!(store.save(&saved()), Err(CredError::Unavailable));
        assert!(secret.value.lock().unwrap().is_none());
        assert!(store.load().is_err());
    }

    #[test]
    fn failed_keychain_delete_is_visible_but_cannot_resurrect_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let secret = Arc::new(FakeSecret::default());
        let active = store(&dir, &secret);
        active.save(&saved()).unwrap();
        secret.fail_delete.store(true, Ordering::SeqCst);
        assert_eq!(active.clear(), Err(CredError::Unavailable));
        assert!(
            secret.value.lock().unwrap().is_some(),
            "确实模拟了系统条目删除失败"
        );
        assert!(
            matches!(store(&dir, &secret).load(), Err(CredError::Unauthorized)),
            "重启必须尊重已登出标记"
        );
    }

    #[test]
    fn successful_secret_delete_is_safe_even_if_control_file_cannot_be_written() {
        let dir = tempfile::tempdir().unwrap();
        let secret = Arc::new(FakeSecret::default());
        let active = store(&dir, &secret);
        active.save(&saved()).unwrap();
        std::fs::remove_file(&active.control_path).unwrap();
        std::fs::create_dir(&active.control_path).unwrap();
        assert!(active.clear().is_ok());
        assert!(secret.value.lock().unwrap().is_none());
        assert!(active.load().is_err());
    }

    #[test]
    fn double_failure_is_not_reported_as_persisted_logout_but_revokes_the_live_lease() {
        let dir = tempfile::tempdir().unwrap();
        let secret = Arc::new(FakeSecret::default());
        let active = Arc::new(store(&dir, &secret));
        let vault = CredentialVault::new(active.clone());
        let lease = vault.activate(saved()).unwrap();
        std::fs::remove_file(&active.control_path).unwrap();
        std::fs::create_dir(&active.control_path).unwrap();
        secret.fail_delete.store(true, Ordering::SeqCst);
        assert_eq!(vault.revoke(), Err(CredError::Unavailable));
        assert!(!lease.is_active());
        assert!(matches!(lease.load(), Err(CredError::Stale)));
    }

    #[test]
    fn safe_storage_read_failure_is_not_treated_as_an_empty_account() {
        let dir = tempfile::tempdir().unwrap();
        let secret = Arc::new(FakeSecret::default());
        let active = store(&dir, &secret);
        active.save(&saved()).unwrap();
        secret.fail_get.store(true, Ordering::SeqCst);
        assert!(matches!(active.load(), Err(CredError::Unavailable)));
        assert!(secret.value.lock().unwrap().is_some());
    }

    #[test]
    fn saved_identity_is_canonical_and_invalid_servers_are_never_restored() {
        let tokens = tokens_for(12, "CanonicalName", "test-only");
        let session = SavedSession::new("https://API.EXAMPLE.TEST:443/", tokens.clone()).unwrap();
        assert_eq!(
            session.identity.owner.server_url,
            "https://api.example.test"
        );
        assert_eq!(session.identity.owner.account_id, "12");
        assert_eq!(session.identity.username, "CanonicalName");
        for url in [
            "https://user:password@api.example.test",
            "https://api.example.test?q=token",
            "http://public.example.test",
        ] {
            assert!(SavedSession::new(url, tokens.clone()).is_err());
        }
        let mut invalid = session.clone();
        invalid.identity.owner.account_id = "99".into();
        assert!(invalid.validate().is_err());
        invalid = session;
        invalid.session_id = Uuid::nil();
        assert!(invalid.validate().is_err());
    }
}
