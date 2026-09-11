use super::source::{Environment, PreferenceSnapshot};
use super::*;
#[derive(Default, Clone)]
pub(super) struct FileEnvironment {
    pub preferences_file: Option<std::path::PathBuf>,
    pub running: bool,
    pub available_space: Option<u64>,
}
impl Environment for FileEnvironment {
    fn check_legacy_stopped(&self) -> Result<(), CommandError> {
        if self.running {
            Err(problem("legacyRunning", "旧版仍在运行"))
        } else {
            Ok(())
        }
    }
    fn preferences(&self) -> Result<PreferenceSnapshot, CommandError> {
        let path = self
            .preferences_file
            .as_ref()
            .ok_or_else(|| problem("invalidFixture", "未配置合成偏好夹具"))?;
        PreferenceSnapshot::selected(&std::fs::read(path).unwrap(), Some(path))
    }
    fn free_space(&self, directory: &Path) -> Result<u64, CommandError> {
        self.available_space
            .map_or_else(|| platform::free_space(directory), Ok)
    }
}

use journal::{Observer, Step};
use std::sync::Arc;
const LEGACY: &str = r#"{"items":[
 {"id":"11111111-1111-4111-8111-111111111111","text":"旧任务 🦀","done":false,"createdAt":"2026-09-01T08:00:00.123+08:00","updatedAt":"2026-09-02T00:00:00Z","dueDate":"2026-12-31T18:00:00+08:00"},
 {"id":"22222222-2222-4222-8222-222222222222","text":"完成的","done":true,"createdAt":"2026-09-01T00:00:00Z"}
 ],"focusID":"11111111-1111-4111-8111-111111111111","notifiedDueIDs":["11111111-1111-4111-8111-111111111111"]}"#;
const PREFS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>mode</key><string>panel</string><key>appearance</key><string>dark</string>
<key>showFocusInMenuBar</key><false/><key>menuBarTextLimit</key><integer>31</integer>
<key>notificationsEnabled</key><false/><key>notificationSound</key><false/>
<key>dueSoonEnabled</key><false/><key>dueSoonHours</key><real>2.5</real>
<key>showOverdueInMenuBar</key><false/><key>showOverdueBanner</key><false/>
<key>automaticSync</key><false/><key>panelOrigin</key><string>{60, 200}</string>
<key>tokens</key><string>synthetic-secret-must-not-be-copied</string>
<key>launchAtLogin</key><true/><key>notificationAuthorization</key><true/>
</dict></plist>"#;

struct Fixture {
    dir: tempfile::TempDir,
    coordinator: Coordinator,
    source: std::path::PathBuf,
    env: FileEnvironment,
    before: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("new-app");
        std::fs::create_dir(&app).unwrap();
        let source = dir.path().join("legacy-items.json");
        std::fs::write(&source, LEGACY).unwrap();
        let preferences_file = dir.path().join("legacy.plist");
        std::fs::write(&preferences_file, PREFS).unwrap();
        let original = crate::preferences::PreferencesFile {
            revision: 7,
            settings: doing_core::AppSettings {
                automatic_sync: false,
                ..Default::default()
            },
            window_origin: Some(crate::preferences::WindowOrigin { x: 5, y: 10 }),
            ..Default::default()
        };
        let before = serde_json::to_vec_pretty(&original).unwrap();
        std::fs::write(app.join("settings.json"), &before).unwrap();
        Self {
            coordinator: Coordinator::new(app),
            dir,
            source,
            env: FileEnvironment {
                preferences_file: Some(preferences_file),
                ..Default::default()
            },
            before,
        }
    }
    fn app(&self) -> std::path::PathBuf {
        self.dir.path().join("new-app")
    }
    fn begin(&self, observer: &mut dyn Observer) -> Result<Journal, CommandError> {
        self.coordinator
            .begin(&self.source, true, 1, &self.env, &mut || Ok(()), observer)
    }
    fn assert_before(&self) {
        assert!(!self.app().join("data.json").exists());
        assert_eq!(
            std::fs::read(self.app().join("settings.json")).unwrap(),
            self.before
        );
    }
    fn assert_after(&self) {
        let data: DataFile =
            serde_json::from_slice(&std::fs::read(self.app().join("data.json")).unwrap()).unwrap();
        let original = parse_legacy(LEGACY.as_bytes()).unwrap();
        assert_eq!(data.items, original.items);
        assert_eq!(data.focus_id, original.focus_id);
        assert_eq!(data.notified_due_ids, original.notified_due_ids);
        assert!(data.sync.account_owner().is_none() && data.sync.dirty);
        assert_eq!(data.sync.local_edit_revision, 1);
        let preferences =
            crate::preferences::parse(&std::fs::read(self.app().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(preferences.revision, 8);
        assert_eq!(
            preferences.settings.appearance,
            doing_core::settings::AppAppearance::Dark
        );
        assert_eq!(preferences.settings.mode, "panel");
        assert_eq!(preferences.settings.menu_bar_text_limit, 31);
        assert_eq!(preferences.settings.due_soon_hours, 2.5);
        assert!(
            !preferences.settings.notifications_enabled && !preferences.settings.automatic_sync
        );
    }
}
struct FailAt(Step);
impl Observer for FailAt {
    fn reached(&mut self, step: Step) -> Result<(), CommandError> {
        if step == self.0 {
            Err(problem("injectedIO", "合成故障"))
        } else {
            Ok(())
        }
    }
}
const PREPARING: &[Step] = &[
    Step::JournalStarted,
    Step::SourcesRecorded,
    Step::TasksBackedUp,
    Step::PreferencesBackedUp,
    Step::BeforeSettingsBackedUp,
    Step::DataStaged,
    Step::SettingsStaged,
    Step::PreparedRecorded,
];
const PUBLISHING: &[Step] = &[
    Step::ReadyRecorded,
    Step::LoginRequiredRecorded,
    Step::CredentialsRevoked,
    Step::PublishingRecorded,
    Step::DataPublished,
    Step::SettingsPublished,
    Step::CompletedRecorded,
];

#[test]
fn migration_backs_up_sources_validates_fields_and_publishes_both_files() {
    let f = Fixture::new();
    let receipt = f.begin(&mut ()).unwrap();
    f.assert_after();
    assert_eq!(receipt.phase, Phase::Committed);
    assert!(receipt.requires_login);
    let backups = f.coordinator.backup_path(receipt.transaction_id);
    assert_eq!(
        std::fs::read(backups.join("legacy-items.json")).unwrap(),
        LEGACY.as_bytes()
    );
    assert_eq!(
        std::fs::read(backups.join("settings-before.json")).unwrap(),
        f.before
    );
    let selected = std::fs::read_to_string(backups.join("legacy-preferences.plist")).unwrap();
    assert!(
        !selected.contains("synthetic-secret")
            && !selected.contains("launchAtLogin")
            && !selected.contains("notificationAuthorization")
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(backups.join("source.json")).unwrap()).unwrap();
    assert_eq!(
        metadata["task"]["sha256"],
        source::checksum(LEGACY.as_bytes())
    );
    assert!(metadata["task"]["stamp"]["modifiedUnixNanos"]
        .as_str()
        .is_some());
    assert_eq!(std::fs::read(&f.source).unwrap(), LEGACY.as_bytes());
}
#[test]
fn every_preparation_failure_leaves_both_targets_untouched_and_is_retryable() {
    for step in PREPARING {
        let f = Fixture::new();
        assert!(f.begin(&mut FailAt(*step)).is_err(), "{step:?}");
        f.assert_before();
        let record = f.coordinator.read().unwrap().unwrap();
        f.coordinator
            .resume(record.transaction_id, &f.env, &mut || Ok(()), &mut ())
            .unwrap();
        f.assert_before();
        f.begin(&mut ()).unwrap();
        f.assert_after();
    }
}
#[test]
fn every_publication_failure_recovers_same_transaction_without_reimport() {
    for step in PUBLISHING {
        let f = Fixture::new();
        assert!(f.begin(&mut FailAt(*step)).is_err(), "{step:?}");
        let record = f.coordinator.read().unwrap().unwrap();
        let done = f
            .coordinator
            .resume(record.transaction_id, &f.env, &mut || Ok(()), &mut ())
            .unwrap();
        assert_eq!(record.transaction_id, done.transaction_id);
        assert_eq!(done.phase, Phase::Committed);
        f.assert_after();
        let before = std::fs::read(f.app().join("data.json")).unwrap();
        f.coordinator
            .resume(done.transaction_id, &f.env, &mut || Ok(()), &mut ())
            .unwrap();
        assert_eq!(std::fs::read(f.app().join("data.json")).unwrap(), before);
        assert_eq!(
            std::fs::read_dir(f.app().join("migrations"))
                .unwrap()
                .count(),
            1
        );
    }
}
#[test]
fn completion_marker_gap_does_not_replay_over_later_user_edits() {
    let f = Fixture::new();
    assert!(f.begin(&mut FailAt(Step::SettingsPublished)).is_err());
    let record = f.coordinator.read().unwrap().unwrap();
    let path = f.app().join("data.json");
    let mut data: DataFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    data.items[0].text = "subsequent external edit must survive".into();
    data.revision += 1;
    let bytes = serde_json::to_vec_pretty(&data).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(
        f.coordinator
            .resume(record.transaction_id, &f.env, &mut || Ok(()), &mut ())
            .unwrap_err()
            .code,
        "targetChanged"
    );
    assert_eq!(
        f.coordinator
            .cancel(record.transaction_id, &mut ())
            .unwrap_err()
            .code,
        "targetChanged"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let kept = f.coordinator.keep_current(record.transaction_id).unwrap();
    assert_eq!(kept.phase, Phase::KeptCurrent);
    assert!(kept.requires_login);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}
#[test]
fn completed_journal_never_requires_old_sources_or_overwrites_new_work() {
    let f = Fixture::new();
    let record = f.begin(&mut ()).unwrap();
    std::fs::remove_file(&f.source).unwrap();
    let bytes = b"new work is not an old staged snapshot";
    std::fs::write(f.app().join("data.json"), bytes).unwrap();
    let done = f
        .coordinator
        .resume(
            record.transaction_id,
            &f.env,
            &mut || panic!("must not touch credentials"),
            &mut (),
        )
        .unwrap();
    assert_eq!(done.phase, Phase::Committed);
    assert_eq!(std::fs::read(f.app().join("data.json")).unwrap(), bytes);
}
#[test]
fn rollback_is_resumable_at_each_step_and_restores_exact_before_preferences() {
    for step in [
        Step::RollbackRecorded,
        Step::SettingsRolledBack,
        Step::DataRolledBack,
        Step::CancelledRecorded,
    ] {
        let f = Fixture::new();
        assert!(f.begin(&mut FailAt(Step::SettingsPublished)).is_err());
        let record = f.coordinator.read().unwrap().unwrap();
        assert!(f
            .coordinator
            .cancel(record.transaction_id, &mut FailAt(step))
            .is_err());
        let cancelled = f
            .coordinator
            .resume(
                record.transaction_id,
                &f.env,
                &mut || panic!("rollback must not authenticate"),
                &mut (),
            )
            .unwrap();
        assert_eq!(cancelled.phase, Phase::Cancelled);
        f.assert_before();
        f.coordinator
            .cancel(record.transaction_id, &mut ())
            .unwrap();
        f.assert_before();
    }
}
#[test]
fn cancel_refuses_unknown_preference_changes_before_touching_either_target() {
    let f = Fixture::new();
    assert!(f.begin(&mut FailAt(Step::DataPublished)).is_err());
    let record = f.coordinator.read().unwrap().unwrap();
    let data = std::fs::read(f.app().join("data.json")).unwrap();
    let changed = br#"{"schemaVersion":99,"future":"preserve"}"#;
    std::fs::write(f.app().join("settings.json"), changed).unwrap();
    assert_eq!(
        f.coordinator
            .cancel(record.transaction_id, &mut ())
            .unwrap_err()
            .code,
        "targetChanged"
    );
    assert_eq!(std::fs::read(f.app().join("data.json")).unwrap(), data);
    assert_eq!(
        std::fs::read(f.app().join("settings.json")).unwrap(),
        changed
    );
    assert!(
        f.coordinator.keep_current(record.transaction_id).is_err(),
        "future schemas cannot be un-fenced as validated data"
    );
}
#[test]
fn old_process_or_insufficient_space_prevents_any_publication() {
    let mut f = Fixture::new();
    f.env.running = true;
    assert_eq!(f.begin(&mut ()).unwrap_err().code, "legacyRunning");
    f.assert_before();
    assert!(f.coordinator.read().unwrap().is_none());
    f.env.running = false;
    f.env.available_space = Some(0);
    assert_eq!(f.begin(&mut ()).unwrap_err().code, "insufficientSpace");
    f.assert_before();
    assert!(f.coordinator.read().unwrap().is_none());
}
#[test]
fn source_change_during_publication_pauses_and_cancel_does_not_touch_source() {
    struct Change {
        path: std::path::PathBuf,
    }
    impl Observer for Change {
        fn reached(&mut self, step: Step) -> Result<(), CommandError> {
            if step == Step::DataPublished {
                std::fs::write(&self.path, LEGACY.replace("旧任务", "更新的旧任务")).unwrap();
            }
            Ok(())
        }
    }
    let f = Fixture::new();
    assert_eq!(
        f.begin(&mut Change {
            path: f.source.clone()
        })
        .unwrap_err()
        .code,
        "sourceChanged"
    );
    let source = std::fs::read(&f.source).unwrap();
    let record = f.coordinator.read().unwrap().unwrap();
    assert_eq!(record.phase, Phase::Publishing);
    assert_eq!(
        f.coordinator
            .resume(record.transaction_id, &f.env, &mut || Ok(()), &mut ())
            .unwrap_err()
            .code,
        "sourceChanged"
    );
    f.coordinator
        .cancel(record.transaction_id, &mut ())
        .unwrap();
    f.assert_before();
    assert_eq!(std::fs::read(&f.source).unwrap(), source);
}
#[test]
fn source_mtime_change_even_with_identical_bytes_stops_resume() {
    let f = Fixture::new();
    assert!(f.begin(&mut FailAt(Step::ReadyRecorded)).is_err());
    let record = f.coordinator.read().unwrap().unwrap();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&f.source)
        .unwrap();
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(10))
        .unwrap();
    assert_eq!(
        f.coordinator
            .resume(record.transaction_id, &f.env, &mut || Ok(()), &mut ())
            .unwrap_err()
            .code,
        "sourceChanged"
    );
    f.assert_before();
}
#[test]
fn corrupt_backups_or_staged_data_never_publish() {
    for filename in [
        "legacy-items.json",
        "legacy-preferences.plist",
        "settings-before.json",
        "data-staged.json",
        "settings-staged.json",
        "source.json",
        "prepared.json",
    ] {
        let f = Fixture::new();
        assert!(f.begin(&mut FailAt(Step::ReadyRecorded)).is_err());
        let record = f.coordinator.read().unwrap().unwrap();
        std::fs::write(
            f.coordinator
                .backup_path(record.transaction_id)
                .join(filename),
            b"corrupt",
        )
        .unwrap();
        assert!(
            f.coordinator
                .resume(record.transaction_id, &f.env, &mut || Ok(()), &mut ())
                .is_err(),
            "{filename}"
        );
        f.assert_before();
    }
}
#[test]
fn unknown_or_corrupt_journal_is_not_rewritten() {
    for bytes in [
        b"{broken".as_slice(),
        br#"{"schemaVersion":99,"future":"preserve"}"#.as_slice(),
    ] {
        let f = Fixture::new();
        std::fs::write(f.coordinator.state_path(), bytes).unwrap();
        assert!(f.begin(&mut ()).is_err());
        assert_eq!(std::fs::read(f.coordinator.state_path()).unwrap(), bytes);
        f.assert_before();
    }
}
#[test]
fn wrong_transaction_id_cannot_resume_cancel_or_keep_another_import() {
    let f = Fixture::new();
    assert!(f.begin(&mut FailAt(Step::DataPublished)).is_err());
    let id = Uuid::new_v4();
    let before = std::fs::read(f.coordinator.state_path()).unwrap();
    assert_eq!(
        f.coordinator
            .resume(id, &f.env, &mut || Ok(()), &mut ())
            .unwrap_err()
            .code,
        "migrationChanged"
    );
    assert_eq!(
        f.coordinator.cancel(id, &mut ()).unwrap_err().code,
        "migrationChanged"
    );
    assert_eq!(
        f.coordinator.keep_current(id).unwrap_err().code,
        "migrationChanged"
    );
    assert_eq!(std::fs::read(f.coordinator.state_path()).unwrap(), before);
}

#[test]
fn failed_completion_marker_must_not_advance_the_durable_phase_in_error_handling() {
    let f = Fixture::new();
    assert!(f
        .begin(&mut FailAt(Step::CompletionMarkerWillWrite))
        .is_err());
    assert_eq!(
        f.coordinator.read().unwrap().unwrap().phase,
        Phase::Publishing,
        "错误处理不能把尚未完成的内存阶段当成已经落盘的阶段"
    );
}
#[test]
fn cancelled_reauthentication_fence_survives_a_later_invalid_import() {
    let f = Fixture::new();
    assert!(f.begin(&mut FailAt(Step::LoginRequiredRecorded)).is_err());
    let record = f.coordinator.read().unwrap().unwrap();
    assert!(record.requires_login);
    f.coordinator
        .cancel(record.transaction_id, &mut ())
        .unwrap();
    std::fs::write(&f.source, b"invalid legacy input").unwrap();
    assert!(f.begin(&mut ()).is_err());
    assert!(
        f.coordinator.read().unwrap().unwrap().requires_login,
        "无效的新尝试不能解开之前持久化的重新登录要求"
    );
}

fn state_for(f: &Fixture, credentials: Arc<crate::creds::memory::MemoryStore>) -> SyncShared {
    Arc::new(AppState::new(f.app(), None, credentials))
}

#[tokio::test]
async fn incomplete_startup_does_not_load_half_a_pair_or_restore_credentials_and_fences_writers() {
    use crate::creds::CredentialStore;
    let f = Fixture::new();
    assert!(f.begin(&mut FailAt(Step::DataPublished)).is_err());
    let record = f.coordinator.read().unwrap().unwrap();
    let staged = f
        .coordinator
        .backup_path(record.transaction_id)
        .join("settings-staged.json");
    let valid_staged = std::fs::read(&staged).unwrap();
    std::fs::write(&staged, b"broken").unwrap();
    let credentials = Arc::new(crate::creds::memory::MemoryStore::default());
    credentials
        .save(
            &crate::creds::SavedSession::new(
                "http://127.0.0.1:9",
                crate::net::test_server::tokens_for(9, "old", "old-access"),
            )
            .unwrap(),
        )
        .unwrap();
    let st = state_for(&f, credentials.clone());
    let loaded = st.clone();
    let env = f.env.clone();
    tokio::task::spawn_blocking(move || {
        assert!(recover_with_environment(&loaded, &env).is_err());
        crate::load_after_migration(&loaded, "http://127.0.0.1:9");
    })
    .await
    .unwrap();
    assert!(!st.auth.lock().await.logged_in);
    assert!(
        st.core.lock().await.store.is_empty(),
        "partial data is not editable UI authority"
    );
    assert!(st.core.lock().await.migration.blocks_writes());
    assert!(st.core.lock().await.save_failed);
    let data_before = std::fs::read(f.app().join("data.json")).unwrap();
    let settings_before = std::fs::read(f.app().join("settings.json")).unwrap();
    let (result, saved) = engine::mutate_core(&st, false, |c| {
        c.store.add("cannot publish", None, SystemClock.now())
    })
    .await;
    assert!(result.is_ok() && !saved);
    assert!(st.core.lock().await.store.is_empty());
    assert_eq!(
        crate::preferences::update(&st, |s| {
            s.mode = "panel".into();
            Ok(())
        })
        .await
        .unwrap_err()
        .code,
        "migrationPending"
    );
    assert_eq!(
        crate::auth::authenticate(
            st.clone(),
            "api/v1/auth/login",
            "http://127.0.0.1:9".into(),
            "user",
            "password123"
        )
        .await
        .unwrap_err()
        .code,
        "migrationPending"
    );
    assert_eq!(
        std::fs::read(f.app().join("data.json")).unwrap(),
        data_before
    );
    assert_eq!(
        std::fs::read(f.app().join("settings.json")).unwrap(),
        settings_before
    );
    // Repair only the synthetic damaged artifact, then restart through the same production loading path.
    std::fs::write(staged, valid_staged).unwrap();
    let loaded = st.clone();
    let env = f.env.clone();
    tokio::task::spawn_blocking(move || {
        recover_with_environment(&loaded, &env).unwrap();
        crate::load_after_migration(&loaded, "http://127.0.0.1:9");
    })
    .await
    .unwrap();
    assert!(!st.core.lock().await.migration.blocks_writes());
    assert!(st.core.lock().await.migration.requires_login());
    assert!(!st.auth.lock().await.logged_in);
    assert_eq!(st.core.lock().await.store.items().len(), 2);
    assert!(credentials.load().is_err());
    f.assert_after();
}

#[tokio::test]
async fn fresh_authentication_clears_login_fence_but_imported_data_still_needs_ownership_confirmation(
) {
    use crate::net::test_server::{json_response, tokens_for, Scripted, SNAPSHOT_EMPTY};
    let server = Scripted::spawn(Box::new(|request| {
        if request.method == "POST" {
            json_response(
                200,
                &serde_json::to_string(&tokens_for(2, "new-account", "new-access")).unwrap(),
            )
        } else {
            json_response(200, SNAPSHOT_EMPTY)
        }
    }));
    let f = Fixture::new();
    f.begin(&mut ()).unwrap();
    let st = state_for(&f, Arc::new(crate::creds::memory::MemoryStore::default()));
    let loaded = st.clone();
    let env = f.env.clone();
    let url = server.url.clone();
    tokio::task::spawn_blocking(move || {
        recover_with_environment(&loaded, &env).unwrap();
        crate::load_after_migration(&loaded, &url);
    })
    .await
    .unwrap();
    assert!(!st.auth.lock().await.logged_in);
    crate::preferences::update(&st, |s| {
        s.automatic_sync = true;
        Ok(())
    })
    .await
    .unwrap();
    crate::auth::authenticate(
        st.clone(),
        "api/v1/auth/login",
        server.url.clone(),
        "new-account",
        "password123",
    )
    .await
    .unwrap();
    for _ in 0..200 {
        if st.engine.read().await.conflict.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let conflict = st
        .engine
        .read()
        .await
        .conflict
        .clone()
        .expect("imported ownership must be confirmed");
    assert_eq!(conflict.reason, doing_core::data::ConflictReason::Ownership);
    assert!(st.core.lock().await.meta.account_owner().is_none());
    assert!(!st.core.lock().await.migration.requires_login());
    assert!(!f.coordinator.read().unwrap().unwrap().requires_login);
    engine::flush(&st).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|r| r.method == "PUT")
            .count(),
        0
    );
}

#[test]
fn preference_defaults_types_ranges_and_system_state_allowlist_match_the_legacy_contract() {
    let defaults =
        legacy_preferences::convert(&legacy_preferences::empty_snapshot(), &Default::default())
            .unwrap();
    assert_eq!(defaults.settings, doing_core::AppSettings::default());
    let raw = PREFS
        .replace("<integer>31</integer>", "<integer>900</integer>")
        .replace("<real>2.5</real>", "<real>0.1</real>");
    let selected = legacy_preferences::selected_snapshot(raw.as_bytes()).unwrap();
    assert!(!String::from_utf8_lossy(&selected).contains("synthetic-secret"));
    let converted = legacy_preferences::convert(&selected, &Default::default()).unwrap();
    assert_eq!(converted.settings.menu_bar_text_limit, 60);
    assert_eq!(converted.settings.due_soon_hours, 0.25);
    assert!(
        !converted.settings.show_focus_in_menu_bar
            && !converted.settings.notification_sound
            && !converted.settings.due_soon_enabled
    );
    assert!(
        !converted.settings.show_overdue_in_menu_bar && !converted.settings.show_overdue_banner
    );
    assert!(converted.origin.is_none());
    assert!(converted.warnings.iter().any(|w| w.contains("范围")));
    assert!(converted.warnings.iter().any(|w| w.contains("坐标")));
}

#[test]
fn cocoa_points_convert_to_destination_monitor_pixels_and_ambiguous_geometry_falls_back() {
    use legacy_preferences::{Display, Geometry, Monitor, Rect};
    let internal = Display {
        name: "internal".into(),
        cocoa: Rect {
            x: 0.0,
            y: 0.0,
            width: 1200.0,
            height: 800.0,
        },
        cocoa_work_area: Rect {
            x: 0.0,
            y: 0.0,
            width: 1200.0,
            height: 775.0,
        },
        scale: 2.0,
    };
    let external = Display {
        name: "external".into(),
        cocoa: Rect {
            x: -1600.0,
            y: 0.0,
            width: 1600.0,
            height: 900.0,
        },
        cocoa_work_area: Rect {
            x: -1600.0,
            y: 0.0,
            width: 1600.0,
            height: 875.0,
        },
        scale: 1.0,
    };
    let mut geometry = Geometry {
        displays: vec![internal, external],
        monitors: vec![
            Monitor {
                name: "internal".into(),
                physical: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 2400.0,
                    height: 1600.0,
                },
                scale: 2.0,
            },
            Monitor {
                name: "external".into(),
                physical: Rect {
                    x: -1600.0,
                    y: -100.0,
                    width: 1600.0,
                    height: 900.0,
                },
                scale: 1.0,
            },
        ],
        window_logical_width: 400.0,
        window_logical_height: 500.0,
    };
    let one = PREFS.replace("{60, 200}", "{100, 250}");
    let converted = legacy_preferences::convert(one.as_bytes(), &geometry).unwrap();
    assert_eq!(
        converted.origin,
        Some(crate::preferences::WindowOrigin { x: 200, y: 100 })
    );
    let two = PREFS.replace("{60, 200}", "{-1550, 150}");
    let converted = legacy_preferences::convert(two.as_bytes(), &geometry).unwrap();
    assert_eq!(
        converted.origin,
        Some(crate::preferences::WindowOrigin { x: -1550, y: 150 }),
        "use destination scale, not current-window pixels"
    );
    // Exercise the same selector and Tauri Position used by desktop::position_window.
    let restored = crate::desktop::placement::restore(
        converted.origin.unwrap(),
        &[
            crate::desktop::placement::WorkArea {
                origin: (0, 50),
                size: (2400, 1550),
                scale: 2.0,
            },
            crate::desktop::placement::WorkArea {
                origin: (-1600, -75),
                size: (1600, 875),
                scale: 1.0,
            },
        ],
        (400.0, 500.0),
    )
    .unwrap();
    assert_eq!(restored.origin, converted.origin.unwrap());
    if cfg!(target_os = "macos") {
        assert_eq!(
            restored.position().to_logical::<f64>(2.0),
            tauri::LogicalPosition::new(-1550.0, 150.0)
        );
    }
    geometry.monitors.push(geometry.monitors[0].clone());
    assert!(legacy_preferences::convert(one.as_bytes(), &geometry)
        .unwrap()
        .origin
        .is_none());
    let invalid = PREFS.replace("{60, 200}", "{NaN, infinity}");
    assert!(legacy_preferences::convert(invalid.as_bytes(), &geometry)
        .unwrap()
        .origin
        .is_none());
}

#[test]
fn reminder_import_removes_nonadjacent_duplicates_and_unknown_ids_without_losing_valid_records() {
    let mut value: serde_json::Value = serde_json::from_str(LEGACY).unwrap();
    value["notifiedDueIDs"] = serde_json::json!([
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
        "11111111-1111-4111-8111-111111111111",
        "33333333-3333-4333-8333-333333333333",
    ]);
    let data = parse_legacy(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        data.notified_due_ids,
        data.items.iter().map(|item| item.id).collect::<Vec<_>>()
    );
    value["notifiedDueIDs"] = serde_json::json!(["not-a-uuid"]);
    assert!(parse_legacy(&serde_json::to_vec(&value).unwrap()).is_err());
}

/// Only the parent test creates this fixture/marker. This helper is not an app command or release feature.
#[test]
#[ignore = "private subprocess helper for killed_migration_processes_recover_and_rollback"]
fn migration_kill_child() {
    let root = std::path::PathBuf::from(
        std::env::var_os("DOING_MIGRATION_KILL_FIXTURE").expect("parent fixture required"),
    );
    assert!(root.is_absolute() && !std::fs::symlink_metadata(&root).unwrap().is_symlink());
    assert_eq!(
        std::fs::read(root.join(".migration-kill-fixture")).unwrap(),
        b"doing-migration-private-kill-v1"
    );
    let step = std::env::var("DOING_MIGRATION_KILL_STEP").unwrap();
    let rollback = std::env::var("DOING_MIGRATION_KILL_ACTION").unwrap() == "rollback";
    let coordinator = Coordinator::new(root.join("new-app"));
    let environment = FileEnvironment {
        preferences_file: Some(root.join("legacy.plist")),
        ..Default::default()
    };
    struct Park {
        root: std::path::PathBuf,
        step: String,
    }
    impl Observer for Park {
        fn reached(&mut self, step: Step) -> Result<(), CommandError> {
            if format!("{step:?}") == self.step {
                let file =
                    std::fs::File::create(self.root.join("kill-checkpoint-reached")).unwrap();
                file.sync_all().unwrap();
                loop {
                    std::thread::park();
                }
            }
            Ok(())
        }
    }
    let mut observer = Park {
        root: root.clone(),
        step,
    };
    if rollback {
        assert!(coordinator
            .begin(
                &root.join("legacy-items.json"),
                true,
                1,
                &environment,
                &mut || Ok(()),
                &mut FailAt(Step::SettingsPublished)
            )
            .is_err());
        let id = coordinator.read().unwrap().unwrap().transaction_id;
        coordinator.cancel(id, &mut observer).unwrap();
    } else {
        coordinator
            .begin(
                &root.join("legacy-items.json"),
                true,
                1,
                &environment,
                &mut || Ok(()),
                &mut observer,
            )
            .unwrap();
    }
    panic!("requested kill checkpoint was not reached");
}

#[test]
fn killed_migration_processes_recover_and_rollback() {
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let import_steps = PREPARING
        .iter()
        .chain(PUBLISHING.iter())
        .copied()
        .chain(std::iter::once(Step::CompletionMarkerWillWrite))
        .map(|step| (step, false));
    let rollback_steps = [
        Step::RollbackRecorded,
        Step::SettingsRolledBack,
        Step::DataRolledBack,
        Step::CancelledRecorded,
    ]
    .into_iter()
    .map(|step| (step, true));
    let mut count = 0;
    for (step, rollback) in import_steps.chain(rollback_steps) {
        let f = Fixture::new();
        std::fs::write(
            f.dir.path().join(".migration-kill-fixture"),
            b"doing-migration-private-kill-v1",
        )
        .unwrap();
        let stderr_path = f.dir.path().join("child.stderr");
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "migrate::transaction_tests::migration_kill_child",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env("DOING_MIGRATION_KILL_FIXTURE", f.dir.path())
            .env("DOING_MIGRATION_KILL_STEP", format!("{step:?}"))
            .env(
                "DOING_MIGRATION_KILL_ACTION",
                if rollback { "rollback" } else { "import" },
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(&stderr_path).unwrap())
            .spawn()
            .unwrap();
        let mut child = OwnedChild(child);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !f.dir.path().join("kill-checkpoint-reached").exists() {
            if let Some(status) = child.0.try_wait().unwrap() {
                panic!(
                    "child exited before {step:?}: {status}; {}",
                    std::fs::read_to_string(stderr_path).unwrap()
                );
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child did not reach {step:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Kill the exact owned child, not a process name; no Rust destructors or unlock callbacks run.
        child.0.kill().unwrap();
        let status = child.0.wait().unwrap();
        assert!(!status.success());
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(
                status.signal(),
                Some(libc::SIGKILL),
                "must prove abrupt termination, not a test panic or orderly exit"
            );
        }
        let record = f.coordinator.read().unwrap().unwrap();
        let restored = f
            .coordinator
            .resume(record.transaction_id, &f.env, &mut || Ok(()), &mut ())
            .unwrap();
        if rollback {
            assert_eq!(restored.phase, Phase::Cancelled);
            f.assert_before();
        } else if PREPARING.contains(&step) {
            assert_eq!(restored.phase, Phase::Rejected);
            f.assert_before();
            f.begin(&mut ()).unwrap();
            f.assert_after();
        } else {
            assert_eq!(restored.phase, Phase::Committed);
            f.assert_after();
        }
        count += 1;
    }
    assert_eq!(count, 20);
    eprintln!("migration process-kill matrix: {count} owned child processes terminated and recovered; no real defaults/credentials used");
}

async fn state_with_edit_history(f: &Fixture, imported: bool) -> SyncShared {
    let st = state_for(f, Arc::new(crate::creds::memory::MemoryStore::default()));
    if imported {
        run_action(
            &st,
            Action::Import {
                source: f.source.clone(),
                preferences: true,
            },
            &f.env,
        )
        .await
        .unwrap();
        acknowledge_new_login(&st, &mut *st.core.lock().await).unwrap();
    } else {
        let loaded = st.clone();
        tokio::task::spawn_blocking(move || {
            crate::load_after_migration(&loaded, "http://127.0.0.1:9")
        })
        .await
        .unwrap();
    }
    crate::auth::install_test_session(
        &st,
        "http://127.0.0.1:9",
        crate::net::test_server::tokens_for(4, "new-user", "synthetic"),
    )
    .await;
    let (result, saved) = engine::mutate_core(&st, true, |core| {
        core.store.add("迁移之后的新工作", None, SystemClock.now())
    })
    .await;
    assert!(result.is_ok() && saved);
    assert!(st.core.lock().await.store.undo_title().is_some());
    st
}

async fn action_fingerprint(st: &SyncShared) -> serde_json::Value {
    let e = st.engine.read().await;
    let auth = st.auth.lock().await;
    let core = st.core.lock().await;
    let directory = st.repo.path().parent().unwrap();
    let files: Vec<_> = [
        "data.json",
        "data.json.bak",
        "settings.json",
        "migration-state.json",
        "auth-session.json",
    ]
    .iter()
    .map(|name| {
        (
            name,
            std::fs::read(directory.join(name))
                .ok()
                .map(|bytes| source::checksum(&bytes)),
        )
    })
    .collect();
    serde_json::json!({
        "data": core.to_data_file(), "undo": core.store.undo_title(), "redo": core.store.redo_title(),
        "settings": core.settings, "preferencesRevision": core.preferences_revision, "origin": core.window_origin,
        "preferencesError": core.preferences_error, "saveFailed": core.save_failed,
        "migration": current_status(st, &core, 0),
        "engine": [e.operation, e.generation, e.session_generation, e.epoch],
        "sync": e.state, "conflict": e.conflict.as_ref().map(|c| c.id),
        "auth": [auth.logged_in, auth.is_authenticating, auth.client.as_ref().and_then(|c| c.lease()).is_some_and(|l| l.is_current())],
        "username": auth.username, "authError": auth.error, "files": files,
    })
}

#[tokio::test]
async fn wrong_id_migration_commands_preserve_history_state_and_session() {
    let f = Fixture::new();
    let st = state_with_edit_history(&f, true).await;
    for action in [
        Action::Resume(Uuid::new_v4()),
        Action::Cancel(Uuid::new_v4()),
        Action::KeepCurrent(Uuid::new_v4()),
    ] {
        let before = action_fingerprint(&st).await;
        assert_eq!(
            run_action(&st, action, &f.env).await.unwrap_err().code,
            "migrationChanged"
        );
        assert_eq!(
            action_fingerprint(&st).await,
            before,
            "wrong ID must not clear history, invalidate a lease, or mutate current state"
        );
    }
}

#[tokio::test]
async fn terminal_migration_commands_preserve_history_state_and_session() {
    let f = Fixture::new();
    let st = state_with_edit_history(&f, true).await;
    let id = f.coordinator.read().unwrap().unwrap().transaction_id;
    for action in [Action::Resume(id), Action::KeepCurrent(id)] {
        let before = action_fingerprint(&st).await;
        assert!(run_action(&st, action, &f.env).await.unwrap().imported);
        assert_eq!(
            action_fingerprint(&st).await,
            before,
            "terminal idempotence includes in-memory history and the current session"
        );
    }
    let before = action_fingerprint(&st).await;
    assert_eq!(
        run_action(&st, Action::Cancel(id), &f.env)
            .await
            .unwrap_err()
            .code,
        "migrationChanged"
    );
    assert_eq!(action_fingerprint(&st).await, before);
}

#[tokio::test]
async fn initialized_empty_target_rejects_import_without_invalidating_history_or_session() {
    let f = Fixture::new();
    let st = state_with_edit_history(&f, false).await;
    let (result, saved) =
        engine::mutate_core(&st, true, |core| core.store.undo(SystemClock.now())).await;
    assert!(result.is_ok() && saved);
    {
        // A completed sync can leave an initialized empty file with redo history and dirty=false.
        let mut core = st.core.lock().await;
        assert!(core.store.is_empty() && core.store.redo_title().is_some());
        core.meta.dirty = false;
        st.repo.save(&core.to_data_file()).unwrap();
    }
    let before = action_fingerprint(&st).await;
    assert_eq!(
        run_action(
            &st,
            Action::Import {
                source: f.source.clone(),
                preferences: true
            },
            &f.env
        )
        .await
        .unwrap_err()
        .code,
        "targetInitialized"
    );
    assert_eq!(
        action_fingerprint(&st).await,
        before,
        "a rejected first import must be read-only even when the initialized file is empty"
    );
}

#[tokio::test]
async fn error_after_durable_completion_still_installs_committed_files_before_unlocking() {
    let f = Fixture::new();
    let st = state_for(&f, Arc::new(crate::creds::memory::MemoryStore::default()));
    let result = run_action_observed(
        &st,
        Action::Import {
            source: f.source.clone(),
            preferences: true,
        },
        &f.env,
        &mut FailAt(Step::CompletedRecorded),
    )
    .await;
    assert_eq!(result.unwrap_err().code, "injectedIO");
    assert_eq!(
        f.coordinator.read().unwrap().unwrap().phase,
        Phase::Committed
    );
    f.assert_after();
    let core = st.core.lock().await;
    assert_eq!(
        core.to_data_file(),
        serde_json::from_slice::<DataFile>(&std::fs::read(st.repo.path()).unwrap()).unwrap()
    );
    assert_eq!(core.preferences_revision, 8);
    assert_eq!(core.settings.mode, "panel");
    assert!(!core.migration.blocks_writes() && core.migration.requires_login());
    drop(core);
    assert!(!st.auth.lock().await.logged_in);
}

#[test]
fn native_display_names_need_not_match_tao_model_names() {
    use legacy_preferences::{Display, Geometry, Monitor, Rect};
    let geometry = Geometry {
        displays: vec![Display {
            name: "Built-in Retina Display".into(),
            cocoa: Rect {
                x: 0.0,
                y: 0.0,
                width: 1200.0,
                height: 800.0,
            },
            cocoa_work_area: Rect {
                x: 0.0,
                y: 0.0,
                width: 1200.0,
                height: 775.0,
            },
            scale: 2.0,
        }],
        monitors: vec![Monitor {
            // Tao 0.35.3 returns "Monitor #<CGDisplayModelNumber>", not localizedName.
            name: "Monitor #44511".into(),
            physical: Rect {
                x: 0.0,
                y: 0.0,
                width: 2400.0,
                height: 1600.0,
            },
            scale: 2.0,
        }],
        window_logical_width: 400.0,
        window_logical_height: 500.0,
    };
    assert_eq!(
        legacy_preferences::convert(PREFS.as_bytes(), &geometry)
            .unwrap()
            .origin,
        Some(crate::preferences::WindowOrigin { x: 120, y: 200 })
    );
}

#[test]
fn migration_lock_rejects_another_coordinator_before_creating_a_transaction() {
    let f = Fixture::new();
    let lock = std::fs::File::create(f.app().join(".migration.lock")).unwrap();
    lock.try_lock().unwrap();
    assert_eq!(f.begin(&mut ()).unwrap_err().code, "migrationBusy");
    assert!(f.coordinator.read().unwrap().is_none());
    f.assert_before();
    drop(lock);
    f.begin(&mut ()).unwrap();
    f.assert_after();
}
