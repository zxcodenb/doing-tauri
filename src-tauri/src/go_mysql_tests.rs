//! Opt-in tests against a fresh, private MySQL and the original Go router.
//! Never fall back to DOING_API_URL, real data directories or system credentials.
use crate::creds::memory::MemoryStore;
use crate::creds::CredentialStore;
use crate::events::SyncStateView;
use crate::net::client::ApiClient;
use crate::net::dto::{ApiError, ItemDto, SnapshotDto};
use crate::state::{AppState, ConflictCandidate};
use crate::{auth, engine, preferences, SyncShared};
use doing_core::clock::{parse_rfc3339, Clock, SystemClock};
use doing_core::data::{AccountOwner, ConflictReason};
use doing_core::repo::LoadResult;
use doing_core::store::Store;
use doing_core::{CoreError, DataFile};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

const KIND: &str = "doing-tauri-isolated-go-mysql-v1";

// Intentionally no Debug: even synthetic passwords/nonces must not end up in logs.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    kind: String,
    base_url: String,
    nonce: String,
    access_ttl_seconds: u64,
    username: String,
    other_username: String,
    password: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Audit {
    migrations: Vec<i64>,
    users: usize,
    items: usize,
    shared_item_ids: usize,
    active_sessions: usize,
    revoked_sessions: usize,
    requests: HashMap<String, usize>,
}
impl Audit {
    fn count(&self, request: &str) -> usize {
        *self.requests.get(request).unwrap_or(&0)
    }
}

struct Fixture {
    root: PathBuf,
    manifest: Manifest,
    http: reqwest::Client,
}

fn isolated_origin(input: &str) -> Option<String> {
    let url = reqwest::Url::parse(input).ok()?;
    (url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port().is_some_and(|p| p != 0)
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none())
    .then(|| url.as_str().trim_end_matches('/').to_owned())
}

fn assert_private(path: &Path, directory: bool) {
    let metadata = std::fs::symlink_metadata(path).expect("private fixture path missing");
    assert!(!metadata.is_symlink(), "fixture paths must not be symlinks");
    assert_eq!(metadata.is_dir(), directory);
    if !directory {
        assert!(metadata.is_file());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(metadata.permissions().mode() & 0o077, 0);
    }
}

impl Fixture {
    async fn from_environment() -> Self {
        let path = PathBuf::from(std::env::var_os("DOING_GO_MYSQL_TEST_MANIFEST").expect(
            "run scripts/exercise-go-mysql.py; this test never accepts a bare API URL or existing DB",
        ));
        assert!(path.is_absolute());
        assert_eq!(path.file_name().unwrap(), "contract-manifest.json");
        let root = path.parent().unwrap().to_path_buf();
        assert_private(&root, true);
        assert_private(&path, false);
        assert_eq!(
            std::fs::read_to_string(root.join("fixture-kind")).unwrap(),
            KIND
        );
        let mut manifest: Manifest = serde_json::from_slice(&std::fs::read(path).unwrap())
            .expect("invalid fixture manifest");
        assert_eq!(manifest.kind, KIND);
        assert!(manifest.nonce.len() >= 32);
        assert_eq!(manifest.access_ttl_seconds, 3);
        assert!(manifest.username.starts_with("Contract_"));
        assert!(manifest.other_username.starts_with("Other_"));
        assert!(manifest.password.len() >= 16);
        manifest.base_url = isolated_origin(&manifest.base_url)
            .expect("integration target must be the runner's loopback-only HTTP fixture");
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let fixture = Self {
            root,
            manifest,
            http,
        };
        let response = fixture
            .http
            .get(fixture.url("/api/v1/health/ready"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(
            response
                .headers()
                .get("X-Doing-Test-Fixture")
                .and_then(|v| v.to_str().ok())
                == Some(fixture.manifest.nonce.as_str()),
            "refusing a server without the isolated fixture proof"
        );
        assert_eq!(fixture.audit().await.migrations, vec![1, 2, 3]);
        fixture
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.manifest.base_url)
    }
    async fn audit(&self) -> Audit {
        self.http
            .get(self.url("/_doing_test/audit"))
            .header("X-Doing-Test-Fixture", &self.manifest.nonce)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn state(&self, name: &str, fresh: bool) -> (SyncShared, Arc<MemoryStore>) {
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        let directory = self.root.join(name);
        if fresh {
            std::fs::create_dir(&directory).expect("new client directory must not exist");
        } else {
            assert!(directory.join("data.json").is_file());
        }
        let credentials = Arc::new(MemoryStore::default());
        let st = Arc::new(AppState::new(directory, None, credentials.clone()));
        let loaded = st.clone();
        let url = self.manifest.base_url.clone();
        tokio::task::spawn_blocking(move || crate::load_persisted(&loaded, &url))
            .await
            .unwrap();
        assert!(!st.auth.lock().await.logged_in);
        if fresh {
            preferences::update(&st, |settings| {
                settings.automatic_sync = false;
                settings.notifications_enabled = false;
                Ok(())
            })
            .await
            .unwrap();
        }
        (st, credentials)
    }
    async fn authenticate(&self, st: &SyncShared, username: &str, register: bool) {
        auth::authenticate(
            st.clone(),
            if register {
                "api/v1/auth/register"
            } else {
                "api/v1/auth/login"
            },
            format!("{}/", self.manifest.base_url),
            username,
            &self.manifest.password,
        )
        .await
        .unwrap();
    }
    fn record<T: Serialize>(&self, name: &str, value: &T) {
        assert!(name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.')));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(self.root.join(name)).unwrap();
        serde_json::to_writer(&file, value).unwrap();
        file.sync_all().unwrap();
    }

    async fn refresh_status(&self, token: &str) -> reqwest::StatusCode {
        self.http
            .post(self.url("/api/v1/auth/refresh"))
            .json(&serde_json::json!({"refresh": token}))
            .send()
            .await
            .unwrap()
            .status()
    }
    async fn logout_and_confirm(&self, st: &SyncShared, credentials: &MemoryStore) {
        let client = client(st).await;
        let tokens = client.lease().unwrap().load().unwrap();
        let before = self.audit().await.count("POST /api/v1/auth/logout 200");
        auth::logout(st).await.unwrap();
        assert!(!st.auth.lock().await.logged_in);
        assert!(credentials.load().is_err());
        assert!(matches!(
            client.get_snapshot().await,
            Err(ApiError::SessionChanged)
        ));
        // Don't probe a still-active refresh token: that would rotate it while logout is in flight.
        for _ in 0..100 {
            if self.audit().await.count("POST /api/v1/auth/logout 200") > before {
                assert_eq!(self.refresh_status(&tokens.refresh).await, 401);
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the original Go logout route did not finish");
    }
}

async fn client(st: &SyncShared) -> ApiClient {
    st.auth
        .lock()
        .await
        .client
        .clone()
        .expect("authenticated fixture client")
}
async fn owner(st: &SyncShared) -> AccountOwner {
    client(st).await.lease().unwrap().identity.owner.clone()
}
async fn mutate<R>(st: &SyncShared, change: impl FnOnce(&mut Store) -> Result<R, CoreError>) -> R {
    let (result, saved) = engine::mutate_core(st, true, |core| change(&mut core.store)).await;
    assert!(
        saved,
        "fixture mutation must commit through the production JSON transaction"
    );
    result.unwrap()
}
async fn rename(st: &SyncShared, id: Uuid, text: &str) {
    mutate(st, |store| store.rename(id, text, SystemClock.now())).await;
}
async fn wait_synced(st: &SyncShared, version: i64) {
    for _ in 0..500 {
        let engine = st.engine.read().await;
        let core = st.core.lock().await;
        if engine.state == SyncStateView::Synced
            && core.meta.known_server_version == Some(version)
            && !core.meta.dirty
            && core.meta.pending_conflict.is_none()
        {
            assert!(!core.save_failed);
            assert!(core.meta.uncertain_upload.is_none());
            return;
        }
        drop(core);
        drop(engine);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "did not reach confirmed version {version}: {:?}",
        st.engine.read().await.state
    );
}
async fn wait_conflict(st: &SyncShared, version: i64) -> ConflictCandidate {
    for _ in 0..500 {
        let (candidate, state) = {
            let engine = st.engine.read().await;
            (engine.conflict.clone(), engine.state)
        };
        if let Some(candidate) = candidate {
            if candidate.snapshot.version == version {
                assert_eq!(state, SyncStateView::Conflict);
                return candidate;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no persisted conflict candidate for version {version}");
}
fn disk(st: &SyncShared) -> DataFile {
    match st.repo.load().unwrap() {
        LoadResult::Loaded(data) => *data,
        _ => panic!("client must have a real versioned data file"),
    }
}
async fn assert_cloud_matches(st: &SyncShared, snapshot: &SnapshotDto) {
    let core = st.core.lock().await;
    let local: Vec<_> = core.store.items().iter().map(ItemDto::from).collect();
    let normalized: Vec<_> = snapshot
        .to_local()
        .unwrap()
        .0
        .iter()
        .map(ItemDto::from)
        .collect();
    assert_eq!(
        local, normalized,
        "UUID/text/done/order/RFC3339 milliseconds must round-trip"
    );
    assert_eq!(core.store.focus_id(), snapshot.focus_id);
}

// Only non-secret synthetic state crosses Rust processes; auth always uses MemoryStore.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Checkpoint {
    owner: AccountOwner,
    candidate_id: Uuid,
    item_id: Uuid,
    local_revision: u64,
    other_snapshot: SnapshotDto,
}

async fn workflows(f: &Fixture) {
    let (a, creds_a) = f.state("client-a", true).await;
    f.authenticate(&a, &format!("  {}  ", f.manifest.username), true)
        .await;
    wait_synced(&a, 0).await;
    assert_eq!(
        a.auth.lock().await.username.as_deref(),
        Some(f.manifest.username.as_str())
    );
    assert_eq!(owner(&a).await.server_url, f.manifest.base_url);
    let created = parse_rfc3339("2026-09-11T08:09:10.123+08:00").unwrap();
    let due = parse_rfc3339("2026-09-15T20:30:40.987+08:00").unwrap();
    let ids = mutate(&a, |store| {
        store.add("中文 / emoji 🦀 / 'quotes'", Some(due), created)?;
        let first = store.items()[0].id;
        store.add("completed", None, created + chrono::Duration::seconds(1))?;
        let completed = store.items()[1].id;
        store.toggle_done(completed, created + chrono::Duration::seconds(2))?;
        store.add("null due", None, created + chrono::Duration::seconds(3))?;
        Ok((first, completed))
    })
    .await;
    {
        let core = a.core.lock().await;
        assert_eq!(core.store.items().last().unwrap().id, ids.1);
        assert_eq!(core.store.focus_id(), Some(ids.0));
        assert!(core.meta.dirty);
    }
    engine::flush(&a).await.unwrap();
    wait_synced(&a, 1).await;
    let cloud = client(&a).await.get_snapshot().await.unwrap();
    assert_eq!(cloud.items[0].created_at, "2026-09-11T00:09:10.123Z");
    assert_eq!(
        cloud.items[0].due_date.as_deref(),
        Some("2026-09-15T12:30:40.987Z")
    );
    assert_cloud_matches(&a, &cloud).await;

    let (b, _creds_b) = f.state("client-b", true).await;
    // MySQL resolves the case-insensitive login; stable identity uses the canonical JWT values.
    f.authenticate(
        &b,
        &format!(" {} ", f.manifest.username.to_lowercase()),
        false,
    )
    .await;
    wait_synced(&b, 1).await;
    assert_eq!(owner(&a).await, owner(&b).await);
    assert_eq!(
        b.auth.lock().await.username.as_deref(),
        Some(f.manifest.username.as_str())
    );
    assert_cloud_matches(&b, &cloud).await;
    assert!(b.core.lock().await.store.undo_title().is_none());

    rename(&a, ids.0, "device A version 2").await;
    engine::flush(&a).await.unwrap();
    wait_synced(&a, 2).await;
    rename(&b, ids.0, "device B offline edit").await;
    engine::flush(&b).await.unwrap();
    let conflict = wait_conflict(&b, 2).await;
    assert_eq!(disk(&b).sync.known_server_version, Some(1));
    assert_eq!(
        disk(&b).sync.pending_conflict.unwrap().candidate_id,
        conflict.id
    );
    assert_eq!(conflict.items[0].text, "device A version 2");
    engine::defer_conflict(&b).await.unwrap();
    engine::auto_flush(&b).await;
    engine::flush(&b).await.unwrap();
    assert_eq!(client(&a).await.get_snapshot().await.unwrap().version, 2);
    engine::choose_local(&b, conflict.id, conflict.version)
        .await
        .unwrap();
    wait_synced(&b, 3).await;
    assert_eq!(
        client(&a).await.get_snapshot().await.unwrap().items[0].text,
        "device B offline edit"
    );

    rename(&a, ids.0, "device A must not overwrite B").await;
    engine::flush(&a).await.unwrap();
    let conflict = wait_conflict(&a, 3).await;
    engine::choose_cloud(&a, conflict.id, conflict.version)
        .await
        .unwrap();
    wait_synced(&a, 3).await;
    assert_cloud_matches(&a, &client(&b).await.get_snapshot().await.unwrap()).await;
    assert!(a.core.lock().await.store.undo_title().is_none());

    let c = client(&b).await;
    let expired = c.lease().unwrap().load().unwrap();
    tokio::time::sleep(Duration::from_secs(f.manifest.access_ttl_seconds + 1)).await;
    assert_eq!(
        f.http
            .get(f.url("/api/v1/snapshot"))
            .bearer_auth(&expired.access)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let before = f.audit().await.count("POST /api/v1/auth/refresh 200");
    let (one, two) = tokio::join!(c.get_snapshot(), c.get_snapshot());
    assert_eq!(one.unwrap().version, 3);
    assert_eq!(two.unwrap().version, 3);
    let fresh = c.lease().unwrap().load().unwrap();
    assert!(
        expired.refresh != fresh.refresh,
        "the real refresh session must rotate"
    );
    assert_eq!(
        f.audit().await.count("POST /api/v1/auth/refresh 200"),
        before + 1
    );
    assert_eq!(f.refresh_status(&expired.refresh).await, 401);
    f.logout_and_confirm(&a, &creds_a).await;

    let (other, _other_creds) = f.state("client-other", true).await;
    f.authenticate(&other, &f.manifest.other_username, true)
        .await;
    wait_synced(&other, 0).await;
    assert_ne!(owner(&other).await, owner(&b).await);
    let (mut same_ids, focus) = client(&b)
        .await
        .get_snapshot()
        .await
        .unwrap()
        .to_local()
        .unwrap();
    for item in &mut same_ids {
        item.text = format!("other account: {}", item.text);
    }
    mutate(&other, |store| {
        store.replace_all(same_ids, focus);
        Ok(())
    })
    .await;
    engine::flush(&other).await.unwrap();
    wait_synced(&other, 1).await;
    let other_snapshot = client(&other).await.get_snapshot().await.unwrap();
    assert_cloud_matches(&other, &other_snapshot).await;
    assert_eq!(
        client(&b).await.get_snapshot().await.unwrap().items[0].text,
        "device B offline edit"
    );
    let audit = f.audit().await;
    assert_eq!(audit.users, 2);
    assert_eq!(audit.items, 6);
    assert_eq!(
        audit.shared_item_ids, 3,
        "real SQL composite UUID ownership, not a mock"
    );
    assert!(audit.revoked_sessions >= 2 && audit.active_sessions >= 2);
    f.record("evidence-uuid-isolation.json", &audit);

    // Empty+dirty is a deletion, not permission to restore old cloud content.
    mutate(&b, |store| {
        let ids: Vec<_> = store.items().iter().map(|item| item.id).collect();
        for id in ids {
            store.delete(id, SystemClock.now())?;
        }
        Ok(())
    })
    .await;
    assert!(disk(&b).sync.dirty && disk(&b).items.is_empty());
    engine::flush(&b).await.unwrap();
    wait_synced(&b, 4).await;
    let empty = client(&b).await.get_snapshot().await.unwrap();
    assert!(empty.items.is_empty() && empty.focus_id.is_none());
    assert_eq!(
        client(&other).await.get_snapshot().await.unwrap(),
        other_snapshot
    );

    mutate(&b, |store| {
        store.add("before process restart", None, SystemClock.now())
    })
    .await;
    let id = b.core.lock().await.store.items()[0].id;
    engine::flush(&b).await.unwrap();
    wait_synced(&b, 5).await;
    f.authenticate(&a, &f.manifest.username, false).await;
    let conflict = wait_conflict(&a, 5).await;
    engine::choose_cloud(&a, conflict.id, conflict.version)
        .await
        .unwrap();
    wait_synced(&a, 5).await;
    rename(&a, id, "cloud persisted through MySQL restart").await;
    engine::flush(&a).await.unwrap();
    wait_synced(&a, 6).await;
    rename(&b, id, "offline edit persisted through Rust restart").await;
    engine::flush(&b).await.unwrap();
    let pending = wait_conflict(&b, 6).await;
    preferences::update(&b, |s| {
        s.automatic_sync = true;
        Ok(())
    })
    .await
    .unwrap();
    let persisted = disk(&b);
    assert_eq!(persisted.sync.known_server_version, Some(5));
    assert!(persisted.sync.dirty);
    assert_eq!(
        persisted.sync.pending_conflict.unwrap().candidate_id,
        pending.id
    );
    let checkpoint = Checkpoint {
        owner: owner(&b).await,
        candidate_id: pending.id,
        item_id: id,
        local_revision: persisted.revision,
        other_snapshot,
    };
    f.record("checkpoint.json", &checkpoint);
    println!("PASS workflows: canonical auth, two clients, 409/choices, refresh/logout, SQL UUID isolation, deletion, durable pending conflict");
}

async fn process_restart(f: &Fixture) {
    let checkpoint: Checkpoint =
        serde_json::from_slice(&std::fs::read(f.root.join("checkpoint.json")).unwrap()).unwrap();
    let (b, creds_b) = f.state("client-b", false).await;
    let loaded = disk(&b);
    assert_eq!(loaded.revision, checkpoint.local_revision);
    assert_eq!(loaded.sync.known_server_version, Some(5));
    assert_eq!(
        loaded.sync.pending_conflict.unwrap().candidate_id,
        checkpoint.candidate_id
    );
    assert_eq!(
        loaded.items[0].text,
        "offline edit persisted through Rust restart"
    );
    assert!(b.core.lock().await.settings.automatic_sync);
    assert!(!b.core.lock().await.settings.notifications_enabled);
    let puts = f.audit().await.count("PUT /api/v1/snapshot 200");
    f.authenticate(&b, &f.manifest.username, false).await;
    let candidate = wait_conflict(&b, 6).await;
    assert_eq!(candidate.id, checkpoint.candidate_id);
    assert_eq!(candidate.owner, checkpoint.owner);
    engine::auto_flush(&b).await;
    engine::retry_tick(&b).await;
    engine::flush(&b).await.unwrap();
    tokio::time::sleep(Duration::from_millis(engine::DEBOUNCE_MS + 250)).await;
    assert_eq!(f.audit().await.count("PUT /api/v1/snapshot 200"), puts);
    let cloud = client(&b).await.get_snapshot().await.unwrap();
    assert_eq!(cloud.version, 6);
    assert_eq!(cloud.items[0].text, "cloud persisted through MySQL restart");
    engine::choose_local(&b, candidate.id, candidate.version)
        .await
        .unwrap();
    wait_synced(&b, 7).await;
    assert_eq!(disk(&b).items[0].id, checkpoint.item_id);
    assert_eq!(
        client(&b).await.get_snapshot().await.unwrap().items[0].text,
        "offline edit persisted through Rust restart"
    );

    f.logout_and_confirm(&b, &creds_b).await;
    let puts = f.audit().await.count("PUT /api/v1/snapshot 200");
    f.authenticate(&b, &f.manifest.other_username, false).await;
    let ownership = wait_conflict(&b, 1).await;
    assert_eq!(ownership.reason, ConflictReason::Ownership);
    assert_ne!(ownership.owner, checkpoint.owner);
    assert_eq!(
        disk(&b).sync.account_owner(),
        Some(checkpoint.owner.clone())
    );
    engine::auto_flush(&b).await;
    engine::flush(&b).await.unwrap();
    assert_eq!(f.audit().await.count("PUT /api/v1/snapshot 200"), puts);
    let archives: Vec<_> = std::fs::read_dir(f.root.join("client-b/archives"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert!(!archives.is_empty());
    assert!(archives.iter().any(|path| {
        serde_json::from_slice::<DataFile>(&std::fs::read(path).unwrap()).is_ok_and(|data| {
            data.sync.account_owner() == Some(checkpoint.owner.clone())
                && data.items.iter().any(|item| {
                    item.id == checkpoint.item_id
                        && item.text == "offline edit persisted through Rust restart"
                })
        })
    }));
    engine::choose_cloud(&b, ownership.id, ownership.version)
        .await
        .unwrap();
    wait_synced(&b, 1).await;
    assert_cloud_matches(&b, &checkpoint.other_snapshot).await;
    assert_eq!(
        client(&b).await.get_snapshot().await.unwrap(),
        checkpoint.other_snapshot
    );
    assert!(b.core.lock().await.store.undo_title().is_none());
    let (reader, _) = f.state("client-reader", true).await;
    f.authenticate(&reader, &f.manifest.username, false).await;
    wait_synced(&reader, 7).await;
    assert_eq!(
        disk(&reader).items[0].text,
        "offline edit persisted through Rust restart"
    );
    assert_eq!(f.audit().await.users, 2);
    println!("PASS process restart: MySQL/Go/Rust persistence, no pending-conflict replay, explicit choice, account-switch archive/isolation");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "only scripts/exercise-go-mysql.py with its NEW private MySQL fixture"]
async fn isolated_go_mysql_contract() {
    let phase =
        std::env::var("DOING_GO_MYSQL_TEST_PHASE").expect("use the isolated fixture runner");
    assert!(matches!(phase.as_str(), "workflows" | "process-restart"));
    let fixture = Fixture::from_environment().await;
    match phase.as_str() {
        "workflows" => workflows(&fixture).await,
        "process-restart" => process_restart(&fixture).await,
        _ => unreachable!(),
    }
}

#[test]
fn integration_guard_rejects_non_fixture_origins() {
    assert_eq!(
        isolated_origin("http://127.0.0.1:38571/"),
        Some("http://127.0.0.1:38571".into())
    );
    for input in [
        "https://api.example.test",
        "http://example.test:38571",
        "http://localhost:38571",
        "http://0.0.0.0:38571",
        "http://127.0.0.1",
        "http://127.0.0.1:0",
        "http://user:password@127.0.0.1:38571",
        "http://127.0.0.1:38571/api",
        "http://127.0.0.1:38571/?token=test",
        "http://127.0.0.1:38571/#fragment",
    ] {
        assert!(
            isolated_origin(input).is_none(),
            "unsafe integration target accepted"
        );
    }
}
