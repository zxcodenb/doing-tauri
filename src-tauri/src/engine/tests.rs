use super::*;
use crate::creds::CredentialStore;
use crate::net::test_server::{json_response, put_ok, snapshot, Resp, Scripted, SNAPSHOT_EMPTY};
use std::future::Future;
use std::time::Duration;

async fn state_of(st: &AppState) -> SyncStateView {
    st.engine.read().await.state
}
async fn known_of(st: &AppState) -> Option<i64> {
    st.engine.read().await.known_version
}

async fn wait_until<F, Fut>(mut pred: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    for _ in 0..500 {
        if pred().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    pred().await
}

fn temp_state() -> (
    Arc<AppState>,
    Arc<crate::creds::memory::MemoryStore>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let creds = Arc::new(crate::creds::memory::MemoryStore::default());
    let st = Arc::new(crate::state::AppState::new(
        dir.path().to_path_buf(),
        None,
        creds.clone() as Arc<dyn CredentialStore>,
    ));
    (st, creds, dir)
}

async fn seed_item(st: &SyncShared, text: &str) {
    // 使用真实事务入口，夹具必须包含落盘、dirty 与代次，不再直插内存绕过守卫。
    let (result, saved) = mutate_core(st, true, |core| {
        core.store.add(text, None, SystemClock.now())
    })
    .await;
    assert!(result.is_ok() && saved);
}

async fn login_like(st: &AppState, url: &str, _creds: Arc<crate::creds::memory::MemoryStore>) {
    crate::auth::install_test_session(
        st,
        url,
        crate::net::test_server::tokens_for(1, "tester", "access"),
    )
    .await;
}
async fn establish_base(st: &SyncShared, version: i64) {
    let mut e = st.engine.write().await;
    let auth = st.auth.lock().await;
    let mut core = st.core.lock().await;
    core.meta.known_server_version = Some(version);
    st.repo.save(&core.to_data_file()).unwrap();
    refresh_view(
        &mut e,
        &core,
        auth.client
            .as_ref()
            .and_then(ApiClient::lease)
            .map(|l| &l.identity.owner),
    );
}
async fn retry_now(st: &SyncShared) {
    assert!(
        st.engine.read().await.retry_at.is_some(),
        "失败必须已排定有上限的退避"
    );
    st.engine.write().await.retry_at = Some(Instant::now());
    retry_tick(st).await;
}

#[tokio::test]
async fn no_baseline_gets_then_puts_with_base0() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|req| {
        if req.method == "GET" {
            json_response(200, SNAPSHOT_EMPTY)
        } else if req.method == "PUT" {
            json_response(200, &put_ok(1))
        } else {
            json_response(404, r#"{"code":"not_found","message":"no"}"#)
        }
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "本地事项").await;

    flush(&st).await.unwrap();
    assert!(
        wait_until(|| async { state_of(&st).await == SyncStateView::Synced }).await,
        "未进入 synced"
    );
    let reqs = server.requests();
    assert_eq!(reqs.iter().filter(|r| r.method == "GET").count(), 1);
    let puts: Vec<_> = reqs.iter().filter(|r| r.method == "PUT").collect();
    assert_eq!(puts.len(), 1);
    assert!(
        puts[0].body.contains("\"baseVersion\":0"),
        "{}",
        puts[0].body
    );
    assert!(puts[0].body.contains("本地事项"));
    assert_eq!(
        puts[0].bearer.as_deref(),
        Some(
            crate::net::test_server::tokens_for(1, "tester", "access")
                .access
                .as_str()
        ),
        "业务请求必须携带访问令牌"
    );
    assert_eq!(known_of(&st).await, Some(1));
    let core = st.core.lock().await;
    assert!(!core.meta.dirty);
    assert_eq!(core.meta.known_server_version, Some(1));
}

#[tokio::test]
async fn put_409_enters_conflict_blocks_auto_then_choose_local_uses_latest_base() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|req| {
        if req.method == "GET" {
            json_response(
                200,
                &snapshot(
                    r#"[{"id":"33333333-3333-3333-3333-333333333333","text":"cloud","done":false,"createdAt":"2026-09-08T00:00:00Z","dueDate":null,"updatedAt":"2026-09-08T00:00:00Z"}]"#,
                    2,
                    None,
                ),
            )
        } else {
            json_response(
                409,
                r#"{"code":"snapshot_conflict","message":"snapshot changed on another device","currentVersion":2}"#,
            )
        }
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "本地").await;
    establish_base(&st, 1).await;

    flush(&st).await.unwrap();
    assert!(
        wait_until(|| async { st.engine.read().await.conflict.is_some() }).await,
        "应进入冲突态"
    );
    assert_eq!(known_of(&st).await, Some(1), "未决候选不能推进已确认基线");
    {
        let e = st.engine.read().await;
        assert!(e.conflict.is_some());
        assert_eq!(e.conflict.as_ref().unwrap().version, 2);
    }

    // 冲突未决时自动上传被阻断：本地再改也不会新增 PUT。
    let puts_before = server.requests_matching(|r| r.method == "PUT");
    assert_eq!(puts_before, 1, "首次上传应已发生（409）");
    seed_item(&st, "冲突期间的本地记录").await;
    on_core_mutated(&st).await;
    tokio::time::sleep(Duration::from_millis(DEBOUNCE_MS + 200)).await;
    assert_eq!(
        server.requests_matching(|r| r.method == "PUT"),
        puts_before,
        "冲突未决不得自动上传"
    );

    // 选择本地：以云端最新版本 2 为基线上传，成功后版本 3。
    server.set_handler(Box::new(|req| {
        if req.method == "GET" {
            json_response(200, &snapshot("[]", 2, None))
        } else if req.method == "PUT" {
            json_response(200, &put_ok(3))
        } else {
            json_response(404, r#"{"code":"not_found","message":"no"}"#)
        }
    }));
    let choice = st.engine.read().await.conflict.clone().unwrap();
    choose_local(&st, choice.id, choice.version).await.unwrap();
    assert!(
        wait_until(|| async { state_of(&st).await == SyncStateView::Synced }).await,
        "选择本地后应 synced"
    );
    let reqs = server.requests();
    let puts: Vec<_> = reqs.iter().filter(|r| r.method == "PUT").collect();
    assert_eq!(puts.len(), puts_before + 1, "只应新增一次上传");
    let last_put = puts.last().unwrap();
    assert!(
        last_put.body.contains("\"baseVersion\":2"),
        "{}",
        last_put.body
    );
    assert_eq!(known_of(&st).await, Some(3));
    assert!(!st.core.lock().await.meta.dirty);
}

/// 计划 P3 门禁：结果未知的 PUT（响应丢失）不得被当作成功，也不得盲目覆盖。
/// 服务器已应用首个 PUT（v2→v3）但断开连接；客户端置 failed、保留 dirty 与基线；
/// 重试沿用原基线 → 409 → 冲突挂起，本地数据完整。
#[tokio::test]
async fn put_result_unknown_gets_latest_then_conflicts_without_second_put() {
    use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};
    let (st, creds, _dir) = temp_state();

    let version = Arc::new(AtomicI64::new(2));
    let v_get = version.clone();
    let v_put = version.clone();
    let server = Scripted::spawn(Box::new(move |req| {
        if req.method == "GET" {
            let v = v_get.load(AtomicOrdering::SeqCst);
            if v >= 3 {
                // 首个 PUT 已在服务端生效：云端出现“本地”内容，版本推进。
                json_response(
                    200,
                    &snapshot(
                        r#"[{"id":"44444444-4444-4444-4444-444444444444","text":"已应用的本地事项","done":false,"createdAt":"2026-09-08T00:00:00Z","dueDate":null,"updatedAt":"2026-09-08T00:00:00Z"}]"#,
                        v,
                        None,
                    ),
                )
            } else {
                json_response(200, &snapshot("[]", v, None))
            }
        } else if req.method == "PUT" {
            let cur = v_put.load(AtomicOrdering::SeqCst);
            if cur == 2 {
                v_put.store(3, AtomicOrdering::SeqCst); // 服务器已应用……
                Resp {
                    status: 0,
                    body: String::new(),
                } // ……但响应丢失
            } else {
                json_response(
                    409,
                    &format!(
                        r#"{{"code":"snapshot_conflict","message":"changed","currentVersion":{cur}}}"#
                    ),
                )
            }
        } else {
            json_response(404, r#"{"code":"not_found","message":"no"}"#)
        }
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "本地-结果未知").await;
    // 保留结果未知时的 dirty；retry driver 只能重试已落盘的变更。
    st.engine.write().await.dirty = true;
    st.core.lock().await.meta.dirty = true;

    flush(&st).await.unwrap();
    assert!(
        wait_until(|| async { state_of(&st).await == SyncStateView::Failed }).await,
        "响应丢失应进入 failed 等待重试"
    );
    {
        let core = st.core.lock().await;
        assert!(core.meta.dirty, "结果未知不得清除 dirty");
        assert_eq!(core.meta.known_server_version, Some(2), "不得假设版本推进");
        assert!(
            core.store.items().iter().any(|i| i.text == "本地-结果未知"),
            "本地数据不得丢失"
        );
    }
    assert_eq!(known_of(&st).await, Some(2), "内存基线保持 2");

    // 先确认服务器基线；已经变化时直接仲裁，不重放结果未知的 PUT。
    retry_now(&st).await;
    assert!(
        wait_until(|| async { state_of(&st).await == SyncStateView::Conflict }).await,
        "重试应转入冲突（不盲写）"
    );
    let reqs = server.requests();
    let put_bodies: Vec<&String> = reqs
        .iter()
        .filter(|r| r.method == "PUT")
        .map(|r| &r.body)
        .collect();
    assert_eq!(put_bodies.len(), 1, "重新确认云端变化后不得再发送 PUT");
    assert_eq!(reqs.iter().filter(|r| r.method == "GET").count(), 2);
    {
        let e = st.engine.read().await;
        assert_eq!(e.conflict.as_ref().map(|c| c.version), Some(3));
    }
    let core = st.core.lock().await;
    assert!(
        core.store.items().iter().any(|i| i.text == "本地-结果未知"),
        "冲突挂起后本地数据仍完整"
    );
}

#[tokio::test]
async fn choose_cloud_replaces_local_and_adopts_version() {
    let (st, creds, _dir) = temp_state();
    // GET 返回云端候选（version 9，含 1 条）；PUT 因基线过期返回 409。
    let server = Scripted::spawn(Box::new(|req| {
        if req.method == "GET" {
            json_response(
                200,
                &snapshot(
                    r#"[{"id":"44444444-4444-4444-4444-444444444444","text":"cloud","done":false,"createdAt":"2026-09-08T00:00:00Z","dueDate":null,"updatedAt":"2026-09-08T00:00:00Z"}]"#,
                    9,
                    None,
                ),
            )
        } else {
            json_response(
                409,
                r#"{"code":"snapshot_conflict","message":"conflict","currentVersion":9}"#,
            )
        }
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "本地要丢的").await;
    flush(&st).await.unwrap();
    let conflicted = wait_until(|| async { state_of(&st).await == SyncStateView::Conflict }).await;
    assert!(conflicted, "应进入冲突态");
    let cloud = { st.engine.read().await.conflict.clone().unwrap() };
    assert_eq!(cloud.version, 9);
    choose_cloud(&st, cloud.id, cloud.version).await.unwrap();
    let core = st.core.lock().await;
    assert_eq!(core.store.items().len(), 1, "本地已被云端替换");
    assert_eq!(core.store.items()[0].text, "cloud");
    assert_eq!(core.meta.known_server_version, Some(9));
    assert_eq!(core.store.undo_title(), None, "云端替换清空历史");
}

#[tokio::test]
async fn auth_failure_clears_credentials_and_enters_unauthorized() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|_req| {
        json_response(
            401,
            r#"{"code":"invalid_token","message":"invalid or expired token"}"#,
        )
    }));
    login_like(&st, &server.url, creds.clone()).await;
    seed_item(&st, "x").await;
    flush(&st).await.unwrap();
    assert!(
        wait_until(|| async { state_of(&st).await == SyncStateView::Unauthorized }).await,
        "鉴权失败应进入 unauthorized"
    );
    assert!(creds.load().is_err(), "凭据应被清除");
    assert!(!st.auth.lock().await.logged_in);
}

#[tokio::test]
async fn failure_enters_failed_and_retry_driver_recovers() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|_req| {
        json_response(503, r#"{"code":"service_unavailable","message":"down"}"#)
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "x").await;
    // 模拟“本地有未上传修改”的真实入口：置 dirty（retry driver 依赖它）。
    st.engine.write().await.dirty = true;
    st.core.lock().await.meta.dirty = true;
    establish_base(&st, 0).await;
    flush(&st).await.unwrap();
    assert!(
        wait_until(|| async { state_of(&st).await == SyncStateView::Failed }).await,
        "应进入 failed"
    );
    server.set_handler(Box::new(|req| {
        if req.method == "GET" {
            json_response(200, &snapshot("[]", 0, None))
        } else {
            json_response(200, &put_ok(5))
        }
    }));
    retry_now(&st).await;
    assert!(
        wait_until(|| async { state_of(&st).await == SyncStateView::Synced }).await,
        "重试驱动应恢复 synced"
    );
    assert_eq!(known_of(&st).await, Some(5));
}

#[tokio::test]
async fn automatic_sync_off_keeps_local_then_manual_flush_works() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|req| {
        if req.method == "GET" {
            json_response(200, SNAPSHOT_EMPTY)
        } else {
            json_response(200, &put_ok(7))
        }
    }));
    login_like(&st, &server.url, creds).await;
    st.core.lock().await.settings.automatic_sync = false;
    seed_item(&st, "仅本地").await;
    on_core_mutated(&st).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        server.requests_matching(|r| r.method == "PUT"),
        0,
        "自动同步关闭不应上传"
    );
    flush(&st).await.unwrap();
    assert!(
        wait_until(|| async { state_of(&st).await == SyncStateView::Synced }).await,
        "手动同步应成功"
    );
    assert_eq!(server.requests_matching(|r| r.method == "PUT"), 1);
}

#[tokio::test]
async fn session_reset_discards_inflight_result() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|_req| {
        std::thread::sleep(Duration::from_millis(600));
        json_response(200, &put_ok(11))
    }));
    login_like(&st, &server.url, creds.clone()).await;
    seed_item(&st, "x").await;
    establish_base(&st, 0).await;
    flush(&st).await.unwrap();
    let started =
        wait_until(|| async { server.requests_matching(|r| r.method == "PUT") == 1 }).await;
    assert!(started, "PUT 应已发出");
    session_reset(&st).await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(creds.load().is_err());
    let e = st.engine.read().await;
    assert_eq!(e.state, SyncStateView::Idle);
    assert_eq!(e.known_version, None);
    assert!(e.conflict.is_none());
    drop(e);
    assert_eq!(st.core.lock().await.meta.known_server_version, Some(0));
    assert!(
        st.core.lock().await.meta.dirty,
        "登出保留旧账号的未同步修改"
    );
}

#[tokio::test]
async fn restart_resume_same_owner_dirty_uploads() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|req| {
        if req.method == "GET" {
            json_response(200, &snapshot("[]", 4, None))
        } else {
            json_response(200, &put_ok(5))
        }
    }));
    login_like(&st, &server.url, creds).await;
    // 用实际任务提交取得稳定账号归属，再保存已确认基线。
    seed_item(&st, "重启前的修改").await;
    establish_base(&st, 4).await;
    crate::auth::resume_after_restart(&st).await;
    assert!(
        wait_until(|| async { state_of(&st).await == SyncStateView::Synced }).await,
        "同归属重启应自动续传"
    );
    assert_eq!(server.requests_matching(|r| r.method == "PUT"), 1);
    assert_eq!(known_of(&st).await, Some(5));
}

#[tokio::test]
async fn restart_resume_unowned_local_enters_conflict_without_upload() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|req| {
        if req.method == "GET" {
            json_response(
                200,
                &snapshot(
                    r#"[{"id":"55555555-5555-5555-5555-555555555555","text":"cloud","done":false,"createdAt":"2026-09-08T00:00:00Z","dueDate":null,"updatedAt":"2026-09-08T00:00:00Z"}]"#,
                    6,
                    None,
                ),
            )
        } else {
            json_response(200, &put_ok(7))
        }
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "无归属本地数据").await;
    st.core.lock().await.meta.account_id = None;
    assert!(persist_all(&st).await);
    // meta 无归属（username/server_url 为空）→ 必须交用户选择。
    crate::auth::resume_after_restart(&st).await;
    assert!(
        wait_until(|| async { state_of(&st).await == SyncStateView::Conflict }).await,
        "无归属恢复应转冲突"
    );
    assert_eq!(
        server.requests_matching(|r| r.method == "PUT"),
        0,
        "无归属数据不得自动上传"
    );
}

#[tokio::test]
async fn restore_with_midflight_edits_becomes_conflict_candidate() {
    let (st, creds, _dir) = temp_state();
    // 云端恢复的 GET 故意放慢，期间本地发生编辑。
    let server = Scripted::spawn(Box::new(|req| {
        if req.method == "GET" {
            std::thread::sleep(Duration::from_millis(350));
            json_response(
                200,
                &snapshot(
                    r#"[{"id":"66666666-6666-6666-6666-666666666666","text":"cloud","done":false,"createdAt":"2026-09-08T00:00:00Z","dueDate":null,"updatedAt":"2026-09-08T00:00:00Z"}]"#,
                    9,
                    None,
                ),
            )
        } else {
            json_response(200, &put_ok(10))
        }
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "本地原始").await;
    let st2 = st.clone();
    let restore = tokio::spawn(async move { restore_from_cloud(&st2).await });
    // 等待 GET 已发出（恢复进行中）。
    assert!(
        wait_until(|| async { server.requests_matching(|r| r.method == "GET") >= 1 }).await,
        "恢复 GET 应已发出"
    );
    // 必须走实际事务入口（含代次推进），不能只改 Store 来绕过并发守卫。
    let (result, saved) = mutate_core(&st, true, |core| {
        core.store
            .add("恢复期间的本地编辑", None, SystemClock.now())
    })
    .await;
    assert!(result.is_ok() && saved);
    on_core_mutated(&st).await;
    let _ = restore.await;
    let e = st.engine.read().await;
    assert_eq!(e.state, SyncStateView::Conflict, "恢复期间编辑应转入冲突");
    assert_eq!(e.conflict.as_ref().map(|c| c.version), Some(9));
    drop(e);
    let core = st.core.lock().await;
    assert_eq!(core.store.items().len(), 2, "本地新工作必须被保留");
}

#[tokio::test]
async fn resume_keeps_pending_conflict_without_any_request() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|_req| {
        json_response(
            500,
            r#"{"code":"internal_error","message":"should not be called"}"#,
        )
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "本地").await;
    let cloud = serde_json::from_str(&snapshot("[]", 6, None)).unwrap();
    prepare_conflict(&st, cloud).await;
    crate::auth::resume_after_restart(&st).await;
    assert_eq!(
        server.requests().len(),
        0,
        "未决冲突在重启后不得发起任何同步请求（更不得自动覆盖）"
    );
    assert_eq!(st.engine.read().await.state, SyncStateView::Conflict);
}
#[tokio::test]
async fn automatic_sync_disabled_blocks_retry_driver() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|req| {
        if req.method == "GET" {
            json_response(200, SNAPSHOT_EMPTY)
        } else {
            json_response(200, &put_ok(1))
        }
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "仅保存在本地").await;
    {
        let mut core = st.core.lock().await;
        core.settings.automatic_sync = false;
        core.meta.dirty = true;
    }
    {
        let mut e = st.engine.write().await;
        e.dirty = true;
        e.state = SyncStateView::Failed;
        e.retry_at = Some(Instant::now());
        e.retry_action = Some(RetryAction::Sync);
    }
    retry_tick(&st).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        server.requests().is_empty(),
        "关闭自动同步后，重试路径也不能上传"
    );
}

#[tokio::test]
async fn automatic_sync_disabled_during_debounce_does_not_upload() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|req| {
        if req.method == "GET" {
            json_response(200, SNAPSHOT_EMPTY)
        } else {
            json_response(200, &put_ok(1))
        }
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "防抖期间关闭").await;
    st.core.lock().await.meta.dirty = true;
    let before = st.engine.read().await.epoch;
    maybe_auto_sync(&st).await;
    assert!(
        st.engine.read().await.epoch > before,
        "必须实际排定防抖任务"
    );
    st.core.lock().await.settings.automatic_sync = false;
    tokio::time::sleep(Duration::from_millis(DEBOUNCE_MS + 150)).await;
    assert!(
        server.requests().is_empty(),
        "执行后台任务前必须重新读取自动同步开关"
    );
}

#[tokio::test]
async fn automatic_sync_disabled_blocks_restart_upload() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|req| {
        if req.method == "GET" {
            json_response(200, &snapshot("[]", 4, None))
        } else {
            json_response(200, &put_ok(5))
        }
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "重启仍只保存在本地").await;
    {
        let mut core = st.core.lock().await;
        core.settings.automatic_sync = false;
        core.meta.dirty = true;
        core.meta.server_url = Some(server.url.clone());
        core.meta.username = Some("tester".into());
        core.meta.known_server_version = Some(4);
    }
    crate::auth::resume_after_restart(&st).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        server.requests_matching(|r| r.method == "PUT"),
        0,
        "自动同步开关必须约束重启恢复路径"
    );
}

#[tokio::test]
async fn put_ack_save_failure_cannot_clear_dirty_or_report_synced() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|_req| {
        std::thread::sleep(Duration::from_millis(250));
        json_response(200, &put_ok(5))
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "服务端成功，但本地确认写入失败").await;
    {
        let mut core = st.core.lock().await;
        core.meta.dirty = true;
        core.meta.known_server_version = Some(4);
        core.settings.automatic_sync = false;
    }
    establish_base(&st, 4).await;
    assert!(persist_all(&st).await);
    flush(&st).await.unwrap();
    assert!(wait_until(|| async { server.requests_matching(|r| r.method == "PUT") == 1 }).await);
    // PUT 发出前已持久化不确定上传标记；确认失败必须保留此标记，不能退回无标记状态。
    let before = std::fs::read(st.repo.path()).unwrap();
    let doing_core::repo::LoadResult::Loaded(marked) = st.repo.load().unwrap() else {
        panic!("已提交文件")
    };
    assert!(marked.sync.uncertain_upload.is_some());
    // 仅使后续确认提交的备份步骤失败；请求里的任务已经成功持久化。
    let backup_path = st.repo.path().with_extension("json.bak");
    if backup_path.is_file() {
        std::fs::remove_file(&backup_path).unwrap();
    }
    std::fs::create_dir(&backup_path).unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        state_of(&st).await,
        SyncStateView::Failed,
        "本地确认落盘失败，不能报告已同步"
    );
    assert!(st.core.lock().await.meta.dirty, "必须保留未确认的 dirty");
    assert_eq!(std::fs::read(st.repo.path()).unwrap(), before);
}

#[tokio::test]
async fn older_put_ack_preserves_newer_committed_edit() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|_req| {
        std::thread::sleep(Duration::from_millis(250));
        json_response(200, &put_ok(5))
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "上传中的旧修订").await;
    {
        let mut core = st.core.lock().await;
        core.meta.dirty = true;
        core.meta.known_server_version = Some(4);
        core.settings.automatic_sync = false;
    }
    establish_base(&st, 4).await;
    assert!(persist_all(&st).await);
    flush(&st).await.unwrap();
    assert!(wait_until(|| async { server.requests_matching(|r| r.method == "PUT") == 1 }).await);
    let (mutation, saved) = mutate_core(&st, true, |core| {
        core.store.add("刚保存的新修订", None, SystemClock.now())
    })
    .await;
    assert!(mutation.is_ok() && saved);
    on_core_mutated(&st).await;
    assert!(wait_until(|| async { known_of(&st).await == Some(5) }).await);
    let core = st.core.lock().await;
    assert!(core.meta.dirty);
    assert_eq!(core.store.items().len(), 2);
    let doing_core::repo::LoadResult::Loaded(data) = st.repo.load().unwrap() else {
        panic!("已提交文件")
    };
    assert_eq!(data.revision, core.store.revision());
    assert!(data.sync.dirty);
    assert_eq!(data.sync.known_server_version, Some(5));
    drop(core);
    assert_eq!(state_of(&st).await, SyncStateView::Pending);
}

#[tokio::test]
async fn cloud_replacement_save_failure_keeps_local_history_and_candidate() {
    let (st, creds, _dir) = temp_state();
    login_like(&st, "http://127.0.0.1:9", creds).await;
    seed_item(&st, "不能被失败的云端恢复覆盖").await;
    let cloud: SnapshotDto = serde_json::from_str(&snapshot("[]", 9, None)).unwrap();
    prepare_conflict(&st, cloud.clone()).await;
    let before = std::fs::read(st.repo.path()).unwrap();
    let before_revision = st.core.lock().await.store.revision();
    let backup_path = st.repo.path().with_extension("json.bak");
    std::fs::remove_file(&backup_path).unwrap();
    std::fs::create_dir(&backup_path).unwrap();
    let id = st.engine.read().await.conflict.as_ref().unwrap().id;
    assert!(choose_cloud(&st, id, cloud.version).await.is_err());
    let core = st.core.lock().await;
    assert_eq!(core.store.items().len(), 1);
    assert_eq!(core.store.revision(), before_revision);
    assert!(core.store.undo_title().is_some());
    assert!(core.meta.dirty);
    assert!(core.save_failed);
    drop(core);
    assert!(st.engine.read().await.conflict.is_some());
    assert_eq!(std::fs::read(st.repo.path()).unwrap(), before);
}
#[tokio::test]
async fn persisted_conflict_survives_a_real_state_reload() {
    let (st, creds, dir) = temp_state();
    let server = Scripted::spawn(Box::new(|req| {
        if req.method == "GET" {
            json_response(200, &snapshot("[]", 9, None))
        } else {
            json_response(200, &put_ok(10))
        }
    }));
    login_like(&st, &server.url, creds.clone()).await;
    seed_item(&st, "必须保留到用户确认的本地任务").await;
    {
        let mut core = st.core.lock().await;
        core.meta.username = Some("tester".into());
        core.meta.server_url = Some(server.url.clone());
        core.meta.known_server_version = Some(4);
    }
    let cloud: SnapshotDto = serde_json::from_str(&snapshot("[]", 9, None)).unwrap();
    prepare_conflict(&st, cloud).await;
    drop(st);
    let restarted = Arc::new(AppState::new(dir.path().into(), None, creds));
    {
        let st = restarted.clone();
        let url = server.url.clone();
        tokio::task::spawn_blocking(move || crate::load_persisted(&st, &url))
            .await
            .unwrap();
    }
    crate::auth::resume_after_restart(&restarted).await;
    flush(&restarted).await.unwrap();
    assert!(
        wait_until(|| async {
            matches!(
                state_of(&restarted).await,
                SyncStateView::Conflict | SyncStateView::Synced
            )
        })
        .await
    );
    assert_eq!(
        state_of(&restarted).await,
        SyncStateView::Conflict,
        "必须从文件恢复冲突而不是直接置内存标志"
    );
    assert_eq!(
        server.requests_matching(|r| r.method == "PUT"),
        0,
        "重启和手动同步都不能绕过未决冲突"
    );
    assert_eq!(restarted.core.lock().await.store.items().len(), 1);
}
fn cloud_fixture(version: i64, text: &str) -> SnapshotDto {
    SnapshotDto {
        items: vec![ItemDto {
            id: Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap(),
            text: text.into(),
            done: false,
            created_at: "2026-09-08T00:00:00Z".into(),
            due_date: None,
            updated_at: "2026-09-08T00:00:00Z".into(),
        }],
        focus_id: None,
        version,
        updated_at: None,
    }
}
async fn real_reload(
    dir: &tempfile::TempDir,
    creds: Arc<crate::creds::memory::MemoryStore>,
    url: &str,
) -> SyncShared {
    let restarted = Arc::new(AppState::new(dir.path().into(), None, creds));
    let cloned = restarted.clone();
    let url = url.to_owned();
    tokio::task::spawn_blocking(move || crate::load_persisted(&cloned, &url))
        .await
        .unwrap();
    restarted
}

#[tokio::test]
async fn initial_owned_nonempty_cloud_nonempty_is_a_candidate_before_any_put() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|_| {
        json_response(
            200,
            &serde_json::to_string(&cloud_fixture(8, "云端")).unwrap(),
        )
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "本地").await;
    let ctx = context(&st).await.unwrap();
    run_flow(&st, ctx, Intent::Manual, None).await.unwrap();
    assert_eq!(server.requests_matching(|r| r.method == "PUT"), 0);
    let core = st.core.lock().await;
    assert_eq!(core.meta.known_server_version, None);
    assert_eq!(
        core.meta.pending_conflict.as_ref().unwrap().reason,
        ConflictReason::Initial
    );
    assert_eq!(core.store.items()[0].text, "本地");
}

#[tokio::test]
async fn foreign_or_unowned_data_never_uploads_even_when_cloud_is_empty() {
    for foreign in [false, true] {
        let (st, creds, _dir) = temp_state();
        let server = Scripted::spawn(Box::new(|_| json_response(200, SNAPSHOT_EMPTY)));
        login_like(&st, &server.url, creds).await;
        seed_item(&st, "需要明确确认归属").await;
        {
            let mut core = st.core.lock().await;
            core.meta.account_id = foreign.then(|| "previous-account".into());
            st.repo.save(&core.to_data_file()).unwrap();
        }
        let ctx = context(&st).await.unwrap();
        run_flow(&st, ctx, Intent::Manual, None).await.unwrap();
        assert_eq!(server.requests_matching(|r| r.method == "PUT"), 0);
        let core = st.core.lock().await;
        assert_eq!(
            core.meta.account_id,
            foreign.then(|| "previous-account".into())
        );
        assert_eq!(
            core.meta.pending_conflict.as_ref().unwrap().reason,
            ConflictReason::Ownership
        );
    }
}

#[tokio::test]
async fn empty_dirty_is_a_deletion_not_permission_to_restore_cloud() {
    for remote_version in [4, 5] {
        let (st, creds, _dir) = temp_state();
        let server = Scripted::spawn(Box::new(move |req| {
            if req.method == "GET" {
                json_response(
                    200,
                    &serde_json::to_string(&cloud_fixture(remote_version, "已在本地删除")).unwrap(),
                )
            } else {
                json_response(200, &put_ok(5))
            }
        }));
        login_like(&st, &server.url, creds).await;
        seed_item(&st, "稍后删除").await;
        establish_base(&st, 4).await;
        let id = st.core.lock().await.store.items()[0].id;
        let (result, saved) =
            mutate_core(&st, true, |core| core.store.delete(id, SystemClock.now())).await;
        assert!(result.is_ok() && saved);
        let ctx = context(&st).await.unwrap();
        run_flow(&st, ctx, Intent::Bootstrap, None).await.unwrap();
        let core = st.core.lock().await;
        assert!(core.store.is_empty(), "未确认不能把删除的任务恢复回来");
        if remote_version == 4 {
            let requests = server.requests();
            let put = requests
                .iter()
                .find(|r| r.method == "PUT")
                .expect("相同基线上传删除");
            let body: serde_json::Value = serde_json::from_str(&put.body).unwrap();
            assert!(body["items"].as_array().unwrap().is_empty());
            assert_eq!(body["baseVersion"], 4);
            assert!(!core.meta.dirty);
        } else {
            assert_eq!(server.requests_matching(|r| r.method == "PUT"), 0);
            assert!(core.meta.dirty);
            assert_eq!(core.meta.known_server_version, Some(4));
            assert_eq!(
                core.meta.pending_conflict.as_ref().unwrap().reason,
                ConflictReason::RemoteChanged
            );
        }
    }
}

#[tokio::test]
async fn slow_first_get_does_not_discard_new_local_work() {
    use crate::net::test_server::Gate;
    let (st, creds, _dir) = temp_state();
    let gate = Gate::new();
    let response_gate = gate.clone();
    let server = Scripted::spawn(Box::new(move |_| {
        response_gate.wait();
        json_response(
            200,
            &serde_json::to_string(&cloud_fixture(8, "云端")).unwrap(),
        )
    }));
    login_like(&st, &server.url, creds).await;
    let task = {
        let st = st.clone();
        tokio::spawn(async move {
            let ctx = context(&st).await.unwrap();
            run_flow(&st, ctx, Intent::Bootstrap, None).await
        })
    };
    server.wait_for(|r| r.method == "GET", 1).await;
    seed_item(&st, "慢请求期间的新工作").await;
    gate.release();
    task.await.unwrap().unwrap();
    let core = st.core.lock().await;
    assert_eq!(core.store.items()[0].text, "慢请求期间的新工作");
    assert_eq!(
        core.meta.pending_conflict.as_ref().unwrap().reason,
        ConflictReason::LocalChanged
    );
    assert_eq!(server.requests_matching(|r| r.method == "PUT"), 0);
}

#[tokio::test]
async fn fresh_empty_start_records_cloud_version_without_blind_put() {
    let (st, creds, _dir) = temp_state();
    let server = Scripted::spawn(Box::new(|_| json_response(200, &snapshot("[]", 6, None))));
    login_like(&st, &server.url, creds).await;
    let ctx = context(&st).await.unwrap();
    run_flow(&st, ctx, Intent::Bootstrap, None).await.unwrap();
    assert_eq!(server.requests().len(), 1);
    assert_eq!(known_of(&st).await, Some(6));
    let doing_core::repo::LoadResult::Loaded(data) = st.repo.load().unwrap() else {
        panic!("已持久化基线")
    };
    assert_eq!(data.sync.known_server_version, Some(6));
    assert!(!data.sync.dirty);
}

#[tokio::test]
async fn changed_remote_version_on_real_restart_cannot_become_upload_base() {
    let (st, creds, dir) = temp_state();
    let server = Scripted::spawn(Box::new(|_| {
        json_response(
            200,
            &serde_json::to_string(&cloud_fixture(9, "其他设备更新")).unwrap(),
        )
    }));
    login_like(&st, &server.url, creds.clone()).await;
    seed_item(&st, "离线编辑").await;
    establish_base(&st, 4).await;
    drop(st);
    let restarted = real_reload(&dir, creds, &server.url).await;
    crate::auth::resume_after_restart(&restarted).await;
    assert_eq!(server.requests_matching(|r| r.method == "PUT"), 0);
    assert_eq!(known_of(&restarted).await, Some(4));
    let core = restarted.core.lock().await;
    assert_eq!(core.store.items()[0].text, "离线编辑");
    assert_eq!(
        core.meta
            .pending_conflict
            .as_ref()
            .unwrap()
            .cloud
            .as_ref()
            .unwrap()
            .version,
        9
    );
}

#[tokio::test]
async fn unknown_put_survives_real_restart_without_replaying_the_write() {
    let (st, creds, dir) = temp_state();
    let server = Scripted::spawn(Box::new(|req| {
        if req.method == "PUT" {
            Resp {
                status: 0,
                body: String::new(),
            }
        } else {
            json_response(
                200,
                &serde_json::to_string(&cloud_fixture(5, "服务器已接受但响应丢失")).unwrap(),
            )
        }
    }));
    login_like(&st, &server.url, creds.clone()).await;
    seed_item(&st, "上传结果未知").await;
    establish_base(&st, 4).await;
    let ctx = context(&st).await.unwrap();
    assert!(run_flow(&st, ctx, Intent::Manual, None).await.is_err());
    assert!(st.core.lock().await.meta.uncertain_upload.is_some());
    drop(st);
    let restarted = real_reload(&dir, creds, &server.url).await;
    crate::auth::resume_after_restart(&restarted).await;
    let ctx = context(&restarted).await.unwrap();
    run_flow(&restarted, ctx, Intent::Manual, None)
        .await
        .unwrap();
    assert_eq!(
        server.requests_matching(|r| r.method == "PUT"),
        1,
        "重启后的确认 GET 不能引发第二次 PUT"
    );
    let core = restarted.core.lock().await;
    assert_eq!(core.meta.known_server_version, Some(4));
    assert_eq!(
        core.meta.pending_conflict.as_ref().unwrap().reason,
        ConflictReason::UploadUncertain
    );
    assert!(core.meta.dirty);
}

#[tokio::test]
async fn failed_409_candidate_fetch_still_blocks_put_after_restart() {
    let (st, creds, dir) = temp_state();
    let server = Scripted::spawn(Box::new(|req| {
        if req.method == "PUT" {
            json_response(
                409,
                r#"{"code":"snapshot_conflict","message":"conflict","currentVersion":8}"#,
            )
        } else {
            json_response(503, r#"{"code":"unavailable","message":"down"}"#)
        }
    }));
    login_like(&st, &server.url, creds.clone()).await;
    seed_item(&st, "不能越过尚未取得候选的冲突").await;
    establish_base(&st, 4).await;
    let ctx = context(&st).await.unwrap();
    assert!(run_flow(&st, ctx, Intent::Manual, None).await.is_err());
    assert!(st
        .core
        .lock()
        .await
        .meta
        .pending_conflict
        .as_ref()
        .unwrap()
        .cloud
        .is_none());
    drop(st);
    let restarted = real_reload(&dir, creds, &server.url).await;
    crate::auth::resume_after_restart(&restarted).await;
    let ctx = context(&restarted).await.unwrap();
    assert!(run_flow(&restarted, ctx, Intent::Manual, None)
        .await
        .is_err());
    assert_eq!(server.requests_matching(|r| r.method == "PUT"), 1);
    server.set_handler(Box::new(|_| json_response(200, &snapshot("[]", 8, None))));
    let ctx = context(&restarted).await.unwrap();
    run_flow(&restarted, ctx, Intent::Manual, None)
        .await
        .unwrap();
    assert!(restarted.engine.read().await.conflict.is_some());
    assert_eq!(server.requests_matching(|r| r.method == "PUT"), 1);
    assert_eq!(known_of(&restarted).await, Some(4));
}

#[tokio::test]
async fn candidate_uuid_and_full_i64_version_are_verified_in_the_commit() {
    let (st, creds, _dir) = temp_state();
    login_like(&st, "http://127.0.0.1:9", creds).await;
    seed_item(&st, "不能被旧确认删除").await;
    establish_base(&st, 4).await;
    let version = 9_007_199_254_740_993;
    prepare_conflict(&st, cloud_fixture(version, "旧候选")).await;
    let old_id = st.engine.read().await.conflict.as_ref().unwrap().id;
    prepare_conflict(&st, cloud_fixture(version, "同版本另一个候选")).await;
    let new_id = st.engine.read().await.conflict.as_ref().unwrap().id;
    let before = std::fs::read(st.repo.path()).unwrap();
    assert_ne!(old_id, new_id);
    assert!(choose_local(&st, old_id, version).await.is_err());
    assert!(choose_cloud(&st, old_id, version).await.is_err());
    assert!(choose_cloud(&st, new_id, version - 1).await.is_err());
    assert_eq!(std::fs::read(st.repo.path()).unwrap(), before);
    let view = ConflictView::from(st.engine.read().await.conflict.as_ref().unwrap(), 1, 1);
    assert_eq!(
        serde_json::to_value(view).unwrap()["cloudVersion"],
        "9007199254740993"
    );
    choose_cloud(&st, new_id, version).await.unwrap();
    assert_eq!(known_of(&st).await, Some(version));
    assert_eq!(
        st.core.lock().await.store.items()[0].text,
        "同版本另一个候选"
    );
}

#[tokio::test]
async fn old_get_put_401_409_and_unknown_results_cannot_touch_account_b() {
    use crate::net::test_server::{tokens_for, Gate};
    for (method, status) in [
        ("GET", 200),
        ("GET", 401),
        ("PUT", 200),
        ("PUT", 401),
        ("PUT", 409),
        ("PUT", 0),
    ] {
        let (st, creds, _dir) = temp_state();
        let gate = Gate::new();
        let response_gate = gate.clone();
        let server = Scripted::spawn(Box::new(move |_| {
            response_gate.wait();
            let body = match (method, status) {
                ("GET", 200) => serde_json::to_string(&cloud_fixture(5, "A 的迟到快照")).unwrap(),
                ("PUT", 200) => put_ok(5),
                (_, 409) => {
                    r#"{"code":"snapshot_conflict","message":"conflict","currentVersion":5}"#.into()
                }
                _ => r#"{"code":"invalid_token","message":"invalid"}"#.into(),
            };
            json_response(status, &body)
        }));
        login_like(&st, &server.url, creds.clone()).await;
        seed_item(&st, "A 本地数据").await;
        establish_base(&st, 4).await;
        let ctx = context(&st).await.unwrap();
        let old = {
            let st = st.clone();
            tokio::spawn(async move {
                run_flow(
                    &st,
                    ctx,
                    if method == "GET" {
                        Intent::Bootstrap
                    } else {
                        Intent::Manual
                    },
                    None,
                )
                .await
            })
        };
        server.wait_for(|r| r.method == method, 1).await;
        session_reset(&st).await;
        crate::auth::install_test_session(&st, &server.url, tokens_for(2, "account-b", "b")).await;
        prepare_conflict(&st, cloud_fixture(8, "B 的新数据")).await;
        let choice = st.engine.read().await.conflict.clone().unwrap();
        choose_cloud(&st, choice.id, choice.version).await.unwrap();
        gate.release();
        let _ = old.await.unwrap();
        assert_eq!(creds.load().unwrap().identity.owner.account_id, "2");
        assert_eq!(st.auth.lock().await.username.as_deref(), Some("account-b"));
        let core = st.core.lock().await;
        assert_eq!(core.store.items()[0].text, "B 的新数据");
        assert_eq!(core.meta.account_id.as_deref(), Some("2"));
        assert_eq!(core.meta.known_server_version, Some(8));
        assert!(core.meta.pending_conflict.is_none());
        assert!(!core.meta.dirty);
        assert_eq!(
            server.requests().len(),
            1,
            "A 的迟到结果不能追加刷新或拉取请求"
        );
    }
}

#[tokio::test]
async fn archive_failure_during_restore_keeps_local_and_exits_syncing() {
    let (st, creds, dir) = temp_state();
    let server = Scripted::spawn(Box::new(|_| json_response(200, &snapshot("[]", 6, None))));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "原账号的离线数据").await;
    crate::auth::install_test_session(
        &st,
        &server.url,
        crate::net::test_server::tokens_for(2, "b", "b"),
    )
    .await;
    std::fs::write(
        dir.path().join("archives"),
        b"fault injection: not a directory",
    )
    .unwrap();
    let before = std::fs::read(st.repo.path()).unwrap();
    assert!(restore_from_cloud(&st).await.is_err());
    assert_eq!(
        state_of(&st).await,
        SyncStateView::Failed,
        "归档失败不能永久显示同步中"
    );
    assert_eq!(
        st.core.lock().await.store.items()[0].text,
        "原账号的离线数据"
    );
    assert_eq!(std::fs::read(st.repo.path()).unwrap(), before);
}

#[tokio::test]
async fn import_during_restore_invalidates_the_old_result_and_increases_revision() {
    use crate::net::test_server::Gate;
    let (st, creds, dir) = temp_state();
    let gate = Gate::new();
    let response_gate = gate.clone();
    let server = Scripted::spawn(Box::new(move |_| {
        response_gate.wait();
        json_response(
            200,
            &serde_json::to_string(&cloud_fixture(8, "恢复开始时的云端")).unwrap(),
        )
    }));
    login_like(&st, &server.url, creds).await;
    st.core.lock().await.settings.automatic_sync = false;
    let task = {
        let st = st.clone();
        tokio::spawn(async move { restore_from_cloud(&st).await })
    };
    server.wait_for(|r| r.method == "GET", 1).await;
    let source = dir.path().join("legacy-source.json");
    std::fs::write(&source, r#"{"items":[{"id":"11111111-1111-4111-8111-111111111111","text":"刚导入的旧版数据","done":false,"createdAt":"2026-09-08T00:00:00Z"}],"focusID":null,"notifiedDueIDs":[]}"#).unwrap();
    crate::migrate::import_fixture(&st, source.display().to_string())
        .await
        .unwrap();
    gate.release();
    assert_eq!(task.await.unwrap().unwrap_err().code, "sessionChanged");
    assert!(!st.auth.lock().await.logged_in);
    let core = st.core.lock().await;
    assert_eq!(
        core.store.items()[0].text,
        "刚导入的旧版数据",
        "导入是新的权威提交，不能被早先恢复请求覆盖"
    );
    assert!(core.migration.requires_login());
    assert!(core.store.revision() > 0);
    assert!(core.meta.account_id.is_none());
    assert!(
        core.meta.pending_conflict.is_none(),
        "旧操作失效后也不能创建迟到候选"
    );
    let doing_core::repo::LoadResult::Loaded(data) = st.repo.load().unwrap() else {
        panic!("已提交导入")
    };
    assert_eq!(data.revision, core.store.revision());
}
#[tokio::test]
async fn revoked_lease_rejects_task_edits_before_auth_loss_event_is_published() {
    let (st, creds, _dir) = temp_state();
    login_like(&st, "http://127.0.0.1:9", creds).await;
    seed_item(&st, "仍需保留").await;
    let before = std::fs::read(st.repo.path()).unwrap();
    st.auth.lock().await.vault.revoke().unwrap();
    let (result, _) = mutate_core(&st, true, |core| {
        core.store
            .add("已失效会话不能提交", None, SystemClock.now())
    })
    .await;
    assert!(matches!(
        result,
        Err(doing_core::CoreError::AuthenticationRequired)
    ));
    assert_eq!(std::fs::read(st.repo.path()).unwrap(), before);
}

#[tokio::test]
async fn lease_revoked_while_choice_waits_for_core_cannot_commit() {
    let (st, creds, _dir) = temp_state();
    login_like(&st, "http://127.0.0.1:9", creds).await;
    seed_item(&st, "不能被失效确认覆盖").await;
    prepare_conflict(&st, cloud_fixture(9, "另一份")).await;
    let id = st.engine.read().await.conflict.as_ref().unwrap().id;
    let vault = st.auth.lock().await.vault.clone();
    let before = std::fs::read(st.repo.path()).unwrap();
    let core_guard = st.core.lock().await;
    let task = {
        let st = st.clone();
        tokio::spawn(async move { choose_cloud(&st, id, 9).await })
    };
    assert!(
        wait_until(|| async { st.engine.try_read().is_err() }).await,
        "确认已取得会话，正在等待核心提交锁"
    );
    vault.revoke().unwrap();
    drop(core_guard);
    assert!(task.await.unwrap().is_err());
    assert_eq!(std::fs::read(st.repo.path()).unwrap(), before);
}

#[test]
fn automatic_retry_backoff_is_exponential_and_bounded() {
    assert_eq!(retry_delay(1), Duration::from_secs(2));
    assert_eq!(retry_delay(2), Duration::from_secs(4));
    assert_eq!(retry_delay(5), Duration::from_secs(32));
    assert_eq!(retry_delay(6), Duration::from_secs(60));
    assert_eq!(retry_delay(u32::MAX), Duration::from_secs(60));
}

#[tokio::test]
async fn reminder_only_revision_during_put_does_not_leave_tasks_dirty() {
    use crate::net::test_server::Gate;
    let (st, creds, _dir) = temp_state();
    let gate = Gate::new();
    let response_gate = gate.clone();
    let server = Scripted::spawn(Box::new(move |_| {
        response_gate.wait();
        json_response(200, &put_ok(5))
    }));
    login_like(&st, &server.url, creds).await;
    seed_item(&st, "提醒记录不是新任务修改").await;
    establish_base(&st, 4).await;
    let ctx = context(&st).await.unwrap();
    let task = {
        let st = st.clone();
        tokio::spawn(async move { run_flow(&st, ctx, Intent::Manual, None).await })
    };
    server.wait_for(|r| r.method == "PUT", 1).await;
    let before = st.core.lock().await.store.items()[0].clone();
    let (marked, saved) = mutate_core(&st, false, |core| {
        core.store.mark_notified(before.id, SystemClock.now());
        Ok(())
    })
    .await;
    assert!(marked.is_ok() && saved);
    gate.release();
    task.await.unwrap().unwrap();
    let core = st.core.lock().await;
    assert_eq!(core.store.items()[0], before);
    assert!(core.store.notified_due_ids().contains(&before.id));
    assert!(!core.meta.dirty);
    assert_eq!(core.meta.known_server_version, Some(5));
}
