use super::*;
use crate::net::test_server::Gate;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

#[derive(Default)]
struct FakeSink {
    ids: Mutex<Vec<Uuid>>,
    fail: AtomicBool,
    gate: Option<Arc<Gate>>,
    started: tokio::sync::Notify,
}
impl NotificationSink for FakeSink {
    fn submit(&self, notification: &DueNotification) -> Result<(), DeliveryError> {
        self.ids.lock().unwrap().push(notification.id);
        self.started.notify_one();
        if let Some(gate) = &self.gate {
            gate.wait();
        }
        if self.fail.load(Ordering::SeqCst) {
            Err(DeliveryError::Unavailable)
        } else {
            Ok(())
        }
    }
}
async fn state() -> (SyncShared, Uuid, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(crate::state::AppState::new(
        dir.path().into(),
        None,
        Arc::new(crate::creds::memory::MemoryStore::default()),
    ));
    crate::auth::install_test_session(
        &st,
        "http://127.0.0.1:9",
        crate::net::test_server::tokens_for(1, "tester", "reminder-test"),
    )
    .await;
    st.core.lock().await.settings.automatic_sync = false;
    let (added, saved) = engine::mutate_core(&st, true, |core| {
        core.store.add(
            "已到期任务",
            Some(SystemClock.now() - chrono::Duration::minutes(5)),
            SystemClock.now(),
        )
    })
    .await;
    assert!(saved);
    (st, added.unwrap().id.unwrap(), dir)
}

#[tokio::test]
async fn no_native_delivery_must_not_consume_reminder_eligibility() {
    let (st, id, _dir) = state().await;
    refresh(&st).await;
    assert!(!st.core.lock().await.store.notified_due_ids().contains(&id));
}
#[tokio::test]
async fn failed_submission_keeps_memory_disk_revision_and_eligibility() {
    let (st, id, _dir) = state().await;
    let before = std::fs::read(st.repo.path()).unwrap();
    let revision = st.core.lock().await.store.revision();
    let sink = Arc::new(FakeSink::default());
    sink.fail.store(true, Ordering::SeqCst);
    refresh_with(&st, sink.clone()).await;
    assert_eq!(sink.ids.lock().unwrap().len(), 1);
    assert!(!st.core.lock().await.store.notified_due_ids().contains(&id));
    assert_eq!(st.core.lock().await.store.revision(), revision);
    assert_eq!(std::fs::read(st.repo.path()).unwrap(), before);
    sink.fail.store(false, Ordering::SeqCst);
    refresh_with(&st, sink.clone()).await;
    assert!(st.core.lock().await.store.notified_due_ids().contains(&id));
    let ids = sink.ids.lock().unwrap();
    assert_eq!(ids[0], ids[1]);
}
#[tokio::test]
async fn enabled_refresh_records_after_successful_submission_only_once() {
    let (st, id, _dir) = state().await;
    let sink = Arc::new(FakeSink::default());
    refresh_with(&st, sink.clone()).await;
    refresh_with(&st, sink.clone()).await;
    assert_eq!(sink.ids.lock().unwrap().len(), 1);
    assert!(st.core.lock().await.store.notified_due_ids().contains(&id));
    let doing_core::repo::LoadResult::Loaded(data) = st.repo.load().unwrap() else {
        panic!("已持久化")
    };
    assert!(data.notified_due_ids.contains(&id));
}
#[tokio::test]
async fn disabled_or_foreign_account_reminders_neither_mark_nor_fire() {
    let (st, id, _dir) = state().await;
    let sink = Arc::new(FakeSink::default());
    st.core.lock().await.settings.notifications_enabled = false;
    refresh_with(&st, sink.clone()).await;
    st.core.lock().await.settings.notifications_enabled = true;
    crate::auth::install_test_session(
        &st,
        "http://127.0.0.1:9",
        crate::net::test_server::tokens_for(2, "another", "b"),
    )
    .await;
    refresh_with(&st, sink.clone()).await;
    assert!(!st.core.lock().await.store.notified_due_ids().contains(&id));
    assert!(sink.ids.lock().unwrap().is_empty());
}
#[tokio::test]
async fn save_failure_after_submission_retries_same_id_without_losing_eligibility() {
    let (st, id, _dir) = state().await;
    let sink = Arc::new(FakeSink::default());
    let before = std::fs::read(st.repo.path()).unwrap();
    let backup = st.repo.path().with_extension("json.bak");
    std::fs::create_dir(&backup).unwrap();
    refresh_with(&st, sink.clone()).await;
    assert!(!st.core.lock().await.store.notified_due_ids().contains(&id));
    assert!(st.core.lock().await.save_failed);
    assert_eq!(std::fs::read(st.repo.path()).unwrap(), before);
    std::fs::remove_dir(backup).unwrap();
    refresh_with(&st, sink.clone()).await;
    assert!(st.core.lock().await.store.notified_due_ids().contains(&id));
    assert!(!st.core.lock().await.save_failed);
    let ids = sink.ids.lock().unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], ids[1]);
}
#[tokio::test]
async fn concurrent_refreshes_share_one_delivery_and_one_committed_record() {
    let (st, id, _dir) = state().await;
    let sink = Arc::new(FakeSink::default());
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let st = st.clone();
        let sink = sink.clone();
        tasks.push(tokio::spawn(async move { refresh_with(&st, sink).await }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(sink.ids.lock().unwrap().len(), 1);
    assert_eq!(st.core.lock().await.store.notified_due_ids(), &[id]);
}
#[tokio::test]
async fn deadline_edit_or_logout_during_submission_does_not_mark_the_new_state() {
    for logout in [false, true] {
        let (st, id, _dir) = state().await;
        let gate = Gate::new();
        let sink = Arc::new(FakeSink {
            gate: Some(gate.clone()),
            ..FakeSink::default()
        });
        let task = {
            let st = st.clone();
            let sink = sink.clone();
            tokio::spawn(async move { refresh_with(&st, sink).await })
        };
        tokio::time::timeout(Duration::from_secs(3), sink.started.notified())
            .await
            .unwrap();
        if logout {
            crate::auth::reset_for_test(&st).await;
        } else {
            let (changed, saved) = engine::mutate_core(&st, true, |core| {
                core.store.set_due(
                    id,
                    Some(SystemClock.now() + chrono::Duration::days(1)),
                    SystemClock.now(),
                )
            })
            .await;
            assert!(changed.is_ok() && saved);
        }
        gate.release();
        task.await.unwrap();
        assert!(!st.core.lock().await.store.notified_due_ids().contains(&id));
    }
}
#[tokio::test]
async fn notification_click_is_scoped_to_the_current_account_and_consumed_once() {
    let (st, id, _dir) = state().await;
    let sink = Arc::new(FakeSink::default());
    refresh_with(&st, sink.clone()).await;
    let numeric = sink.ids.lock().unwrap()[0];
    assert_eq!(
        take_notification_target(&st, numeric)
            .await
            .unwrap()
            .item_id,
        id
    );
    assert!(take_notification_target(&st, numeric).await.is_none());
    let owner = st.core.lock().await.meta.account_owner().unwrap();
    let target = NotificationTarget {
        item_id: id,
        owner,
        due_millis: 42,
    };
    let numeric = reserve_id(&st, &target).unwrap();
    crate::auth::install_test_session(
        &st,
        "http://127.0.0.1:9",
        crate::net::test_server::tokens_for(2, "another", "b"),
    )
    .await;
    assert!(take_notification_target(&st, numeric).await.is_none());
}
#[test]
fn durable_ids_are_stable_for_retries_and_scoped_to_account_item_and_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let routes = routes::Routes::new(dir.path().join("notification-routes.json"));
    let target = NotificationTarget {
        item_id: Uuid::new_v4(),
        owner: AccountOwner {
            server_url: "https://api.example.test".into(),
            account_id: "1".into(),
        },
        due_millis: 1000,
    };
    let id = routes.reserve(&target).unwrap();
    assert_eq!(id.get_version_num(), 4);
    assert_eq!(routes.reserve(&target).unwrap(), id);
    let restarted = routes::Routes::new(dir.path().join("notification-routes.json"));
    assert_eq!(restarted.reserve(&target).unwrap(), id);
    let mut changed = target.clone();
    changed.due_millis += 1;
    assert_ne!(restarted.reserve(&changed).unwrap(), id);
    changed = target;
    changed.owner.account_id = "2".into();
    assert_ne!(restarted.reserve(&changed).unwrap(), id);
}

#[tokio::test]
async fn notification_target_survives_restarting_the_app_with_the_same_account() {
    let (st, item_id, dir) = state().await;
    let sink = Arc::new(FakeSink::default());
    refresh_with(&st, sink.clone()).await;
    let notification_id = sink.ids.lock().unwrap()[0];
    drop(st);
    let restarted = Arc::new(crate::state::AppState::new(
        dir.path().into(),
        None,
        Arc::new(crate::creds::memory::MemoryStore::default()),
    ));
    let loading = restarted.clone();
    tokio::task::spawn_blocking(move || {
        crate::load_after_migration(&loading, "http://127.0.0.1:9")
    })
    .await
    .unwrap();
    crate::auth::install_test_session(
        &restarted,
        "http://127.0.0.1:9",
        crate::net::test_server::tokens_for(1, "tester", "new-session"),
    )
    .await;
    assert!(restarted
        .core
        .lock()
        .await
        .store
        .notified_due_ids()
        .contains(&item_id));
    assert_eq!(
        take_notification_target(&restarted, notification_id)
            .await
            .map(|target| target.item_id),
        Some(item_id),
        "OS notifications outlive the process; their account-scoped route must too"
    );
}

#[tokio::test]
async fn activation_before_loading_or_login_is_durable_and_foreign_account_cannot_ack_it() {
    let (st, item_id, dir) = state().await;
    let sink = Arc::new(FakeSink::default());
    refresh_with(&st, sink.clone()).await;
    let id = sink.ids.lock().unwrap()[0];
    drop(st);
    let restarted = Arc::new(crate::state::AppState::new(
        dir.path().into(),
        None,
        Arc::new(crate::creds::memory::MemoryStore::default()),
    ));
    receive_activation(&restarted, doing_notifications::Activation::Route(id)); // same native callback path, before any loading or credential restoration
    assert!(restarted.notification_routes.has_pending().unwrap());
    assert!(next_notification(&restarted).await.unwrap().is_none());
    let loaded = restarted.clone();
    tokio::task::spawn_blocking(move || crate::load_after_migration(&loaded, "http://127.0.0.1:9"))
        .await
        .unwrap();
    assert!(next_notification(&restarted).await.unwrap().is_none());
    crate::auth::install_test_session(
        &restarted,
        "http://127.0.0.1:9",
        crate::net::test_server::tokens_for(2, "account-b", "b"),
    )
    .await;
    {
        // B has the same task UUID too. Neither a route ID nor UUID equality grants ownership.
        let mut core = restarted.core.lock().await;
        core.meta.account_id = Some("2".into());
    }
    assert!(next_notification(&restarted).await.unwrap().is_none());
    let generation = restarted.engine.read().await.session_generation;
    assert_eq!(
        acknowledge_notification(&restarted, id, generation)
            .await
            .unwrap_err()
            .code,
        "sessionChanged"
    );
    assert!(restarted.notification_routes.has_pending().unwrap());
    {
        restarted.core.lock().await.meta.account_id = Some("1".into());
    }
    crate::auth::install_test_session(
        &restarted,
        "http://127.0.0.1:9",
        crate::net::test_server::tokens_for(1, "tester", "new-a"),
    )
    .await;
    let view = next_notification(&restarted).await.unwrap().unwrap();
    assert_eq!((view.notification_id, view.item_id), (id, item_id));
    assert_eq!(
        acknowledge_notification(&restarted, id, generation)
            .await
            .unwrap_err()
            .code,
        "sessionChanged"
    );
    // Reading repeatedly is not consuming. Only a current-session UI acknowledgment retires it.
    assert!(next_notification(&restarted).await.unwrap().is_some());
    acknowledge_notification(&restarted, id, view.session_generation)
        .await
        .unwrap();
    assert!(!restarted.notification_routes.activate(id).unwrap());
    assert!(next_notification(&restarted).await.unwrap().is_none());
    let after_restart = routes::Routes::new(dir.path().join("notification-routes.json"));
    assert!(!after_restart.has_pending().unwrap());
}

#[tokio::test]
async fn missing_route_storage_prevents_os_submission_and_preserves_task_eligibility() {
    for bad in [b"{bad".as_slice(), br#"{"schemaVersion":99,"routes":[]}"#] {
        let (st, item_id, _dir) = state().await;
        let before = std::fs::read(st.repo.path()).unwrap();
        std::fs::write(st.notification_routes.path(), bad).unwrap();
        let sink = Arc::new(FakeSink::default());
        refresh_with(&st, sink.clone()).await;
        assert!(
            sink.ids.lock().unwrap().is_empty(),
            "must persist an interpretable route before calling any OS API"
        );
        assert_eq!(std::fs::read(st.notification_routes.path()).unwrap(), bad);
        assert_eq!(std::fs::read(st.repo.path()).unwrap(), before);
        assert!(!st
            .core
            .lock()
            .await
            .store
            .notified_due_ids()
            .contains(&item_id));
    }
}

#[tokio::test]
async fn completed_or_rescheduled_task_still_navigates_but_deleted_task_only_opens_workspace() {
    for change in ["complete", "reschedule", "delete"] {
        let (st, item_id, _dir) = state().await;
        let sink = Arc::new(FakeSink::default());
        refresh_with(&st, sink.clone()).await;
        let id = sink.ids.lock().unwrap()[0];
        assert!(st.notification_routes.activate(id).unwrap());
        let (result, saved) = engine::mutate_core(&st, true, |core| match change {
            "complete" => core.store.toggle_done(item_id, SystemClock.now()),
            "reschedule" => core.store.set_due(
                item_id,
                Some(SystemClock.now() + chrono::Duration::days(1)),
                SystemClock.now(),
            ),
            _ => core.store.delete(item_id, SystemClock.now()),
        })
        .await;
        assert!(result.is_ok() && saved);
        let target = next_notification(&st).await.unwrap();
        if change == "delete" {
            assert!(target.is_none() && !st.notification_routes.has_pending().unwrap());
        } else {
            assert_eq!(target.unwrap().item_id, item_id);
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn failed_ack_preserves_pending_activation_and_does_not_rewrite_task_files() {
    use std::os::unix::fs::PermissionsExt;
    let (st, _item_id, dir) = state().await;
    let sink = Arc::new(FakeSink::default());
    refresh_with(&st, sink.clone()).await;
    let id = sink.ids.lock().unwrap()[0];
    st.notification_routes.activate(id).unwrap();
    let view = next_notification(&st).await.unwrap().unwrap();
    let before = std::fs::read(st.notification_routes.path()).unwrap();
    let tasks = std::fs::read(st.repo.path()).unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = acknowledge_notification(&st, id, view.session_generation).await;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(st.notification_routes.path()).unwrap(),
        before
    );
    assert_eq!(std::fs::read(st.repo.path()).unwrap(), tasks);
    assert!(st.notification_routes.has_pending().unwrap());
    acknowledge_notification(&st, id, view.session_generation)
        .await
        .unwrap();
    assert!(!st.notification_routes.has_pending().unwrap());
}

#[test]
fn independent_route_repositories_reserve_only_one_identity_for_a_retry() {
    let dir = tempfile::tempdir().unwrap();
    let target = NotificationTarget {
        item_id: Uuid::new_v4(),
        owner: AccountOwner {
            server_url: "https://example.test".into(),
            account_id: "fixture".into(),
        },
        due_millis: 123,
    };
    let mut workers = Vec::new();
    for _ in 0..12 {
        let target = target.clone();
        let path = dir.path().join("notification-routes.json");
        workers.push(std::thread::spawn(move || {
            routes::Routes::new(path).reserve(&target).unwrap()
        }));
    }
    let ids: std::collections::HashSet<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(ids.len(), 1);
    let content = std::fs::read_to_string(dir.path().join("notification-routes.json")).unwrap();
    assert!(
        !content.contains("text") && !content.contains("token") && !content.contains("username")
    );
}

#[tokio::test]
async fn unavailable_native_backend_reports_unknown_not_fictitious_granted_permission() {
    let (st, _, _dir) = state().await;
    let result = permission_view(&st, false).await.unwrap();
    assert!(matches!(result.status, NotificationPermission::Unavailable));
    assert!(result.error.is_some());
    assert_eq!(
        permission_view(&st, true).await.unwrap_err().code,
        "notificationUnavailable"
    );
}

#[test]
#[ignore = "private helper for notification_routes_recover_after_real_process_kills"]
fn notification_route_kill_child() {
    let root = std::path::PathBuf::from(
        std::env::var_os("DOING_NOTIFICATION_KILL_FIXTURE").expect("private fixture required"),
    );
    assert_eq!(
        std::fs::read(root.join(".doing-notification-fixture")).unwrap(),
        b"doing-notification-private-v1"
    );
    assert!(root.is_absolute() && std::fs::symlink_metadata(&root).unwrap().is_dir());
    assert!(
        !root.join("notification-routes.json").exists(),
        "helper must start in a fresh synthetic directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    let phase = std::env::var("DOING_NOTIFICATION_KILL_PHASE").unwrap();
    assert!(["reserved", "pending", "consumed"].contains(&phase.as_str()));
    let routes = routes::Routes::new(root.join("notification-routes.json"));
    let target = NotificationTarget {
        item_id: Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
        owner: AccountOwner {
            server_url: "https://fixture.example.test".into(),
            account_id: "fixture-account".into(),
        },
        due_millis: 123,
    };
    let id = routes.reserve(&target).unwrap();
    if phase != "reserved" {
        routes.activate(id).unwrap();
    }
    if phase == "consumed" {
        routes.acknowledge(id).unwrap();
    }
    doing_core::repo::write_new_atomic(&root.join("checkpoint"), id.to_string().as_bytes())
        .unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
fn notification_routes_recover_after_real_process_kills() {
    struct Owned(std::process::Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for phase in ["reserved", "pending", "consumed"] {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(
            dir.path().join(".doing-notification-fixture"),
            b"doing-notification-private-v1",
        )
        .unwrap();
        let stderr_path = dir.path().join("child.stderr");
        let mut child = Owned(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "reminder::tests::notification_route_kill_child",
                    "--exact",
                    "--ignored",
                    "--nocapture",
                ])
                .env("DOING_NOTIFICATION_KILL_FIXTURE", dir.path())
                .env("DOING_NOTIFICATION_KILL_PHASE", phase)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::fs::File::create(&stderr_path).unwrap())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let checkpoint = dir.path().join("checkpoint");
        while !checkpoint.exists() {
            if let Some(status) = child.0.try_wait().unwrap() {
                panic!(
                    "owned child exited before {phase}: {status}; {}",
                    std::fs::read_to_string(&stderr_path).unwrap()
                );
            }
            assert!(
                std::time::Instant::now() < deadline,
                "owned child never reached checkpoint"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let id = Uuid::parse_str(&std::fs::read_to_string(checkpoint).unwrap()).unwrap();
        child.0.kill().unwrap();
        let status = child.0.wait().unwrap();
        assert!(!status.success());
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(libc::SIGKILL));
        }
        let restored = routes::Routes::new(dir.path().join("notification-routes.json"));
        if phase == "consumed" {
            assert!(!restored.activate(id).unwrap());
            assert!(!restored.has_pending().unwrap());
        } else {
            assert_eq!(restored.has_pending().unwrap(), phase == "pending");
            assert!(restored.activate(id).unwrap());
            assert_eq!(restored.pending().unwrap()[0].id, id);
            restored.acknowledge(id).unwrap();
            assert!(!restored.has_pending().unwrap());
        }
    }
    eprintln!("notification route process-kill matrix: 3 owned child processes; durable reservation/activation/ACK survived");
}

#[tokio::test]
async fn valid_native_clicks_present_for_unknown_consumed_and_legacy_routes() {
    use doing_notifications::Activation;
    let (st, item_id, _dir) = state().await;
    let target = NotificationTarget {
        item_id,
        owner: st.core.lock().await.meta.account_owner().unwrap(),
        due_millis: 123,
    };
    let id = st.notification_routes.reserve(&target).unwrap();
    st.notification_routes.activate(id).unwrap();
    st.notification_routes.acknowledge(id).unwrap();
    let before = std::fs::read(st.notification_routes.path()).unwrap();
    let tasks_before = std::fs::read(st.repo.path()).unwrap();
    for activation in [
        Activation::Route(Uuid::new_v4()),
        Activation::Route(id),
        Activation::OpenWorkspace,
    ] {
        let presented = Arc::new(AtomicBool::new(false));
        let observed = presented.clone();
        dispatch_activation(&st, activation, move |_| async move {
            observed.store(true, Ordering::SeqCst);
        })
        .await
        .unwrap();
        assert!(
            presented.load(Ordering::SeqCst),
            "native click was discarded: {activation:?}"
        );
        assert!(
            next_notification(&st).await.unwrap().is_none(),
            "must not guess a task"
        );
        assert_eq!(
            std::fs::read(st.notification_routes.path()).unwrap(),
            before
        );
        assert_eq!(std::fs::read(st.repo.path()).unwrap(), tasks_before);
    }
}

#[tokio::test]
async fn damaged_route_storage_still_presents_and_is_never_overwritten_by_a_click() {
    use doing_notifications::Activation;
    let (st, _item_id, _dir) = state().await;
    let damaged = b"preserve this synthetic damaged route file";
    std::fs::write(st.notification_routes.path(), damaged).unwrap();
    let before = std::fs::read(st.repo.path()).unwrap();
    for activation in [Activation::OpenWorkspace, Activation::Route(Uuid::new_v4())] {
        let presented = Arc::new(AtomicBool::new(false));
        let observed = presented.clone();
        dispatch_activation(&st, activation, move |_| async move {
            observed.store(true, Ordering::SeqCst);
        })
        .await
        .unwrap();
        assert!(presented.load(Ordering::SeqCst));
        assert_eq!(
            std::fs::read(st.notification_routes.path()).unwrap(),
            damaged
        );
        assert_eq!(std::fs::read(st.repo.path()).unwrap(), before);
    }
    assert_eq!(
        next_notification(&st).await.unwrap_err().code,
        "notificationStorageUnavailable"
    );
}
