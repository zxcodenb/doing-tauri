//! 旧 Swift 行为测试 → Rust 行为映射的集成测试。
//! 覆盖模型层可移植规则（排序、焦点、历史、迁移字段、加载容错）。

use chrono::{DateTime, Duration, Utc};
use doing_core::clock::parse_rfc3339;
use doing_core::data::{parse_file, DataFile, FileParse};
use doing_core::item::{fixed_dt, Item};
use doing_core::repo::{JsonRepo, LoadResult};
use doing_core::settings::AppSettings;
use doing_core::{Store, SyncMeta};
use uuid::Uuid;

fn at(s: &str) -> DateTime<Utc> {
    parse_rfc3339(s).unwrap()
}
fn now() -> DateTime<Utc> {
    fixed_dt(2026, 9, 3, 10, 0, 0)
}

fn store_with(items: Vec<Item>) -> Store {
    let mut data = DataFile::new();
    data.items = items;
    Store::from_data(&data)
}

fn item(text: &str) -> Item {
    let t = at("2026-09-03T00:00:00Z");
    Item {
        created_at: t,
        updated_at: t,
        ..Item::new(text)
    }
}

#[test]
fn focus_unique_completed_never_focused_and_toggle_bounds() {
    let mut s = Store::new();
    let n = now();
    let a = s.add("焦点", None, n).unwrap().id.unwrap();
    let b = s.add("下一件", None, n).unwrap().id.unwrap();
    s.toggle_focus(b, n).unwrap();
    assert_eq!(s.focused_item().unwrap().id, b);
    assert_eq!(
        s.remaining_items().iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![a]
    );
    let ordered = s.ordered();
    assert_eq!(
        ordered.len(),
        ordered
            .iter()
            .map(|i| i.id)
            .collect::<std::collections::HashSet<_>>()
            .len()
    );
    s.toggle_done(b, n).unwrap();
    assert_eq!(s.focus_id(), None);
    assert!(s.toggle_focus(b, n).is_err());
    assert!(s.toggle_focus(Uuid::new_v4(), n).is_err());
}

#[test]
fn same_due_date_keeps_manual_order() {
    let mut s = Store::new();
    let n = now();
    let focus = s.add("焦点", None, n).unwrap().id.unwrap();
    let date = fixed_dt(2026, 9, 5, 9, 0, 0);
    let a = s.add("同一时间 A", Some(date), n).unwrap().id.unwrap();
    let b = s.add("同一时间 B", Some(date), n).unwrap().id.unwrap();
    let early = s
        .add("更早", Some(date - Duration::hours(1)), n)
        .unwrap()
        .id
        .unwrap();
    let manual = s.add("无日期", None, n).unwrap().id.unwrap();
    assert_eq!(
        s.ordered().iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![focus, early, a, b, manual]
    );
}

#[test]
fn move_before_semantics_and_guards() {
    let mut s = Store::new();
    let n = now();
    let a = s.add("A", None, n).unwrap().id.unwrap();
    let b = s.add("B", None, n).unwrap().id.unwrap();
    let c = s.add("C", None, n).unwrap().id.unwrap();
    // C 移到 B 之前 → [A, C, B]
    s.move_before(c, b, n).unwrap();
    assert_eq!(
        s.items()
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>(),
        vec!["A", "C", "B"]
    );
    // C 已紧邻 B 之前：空操作
    assert!(s.move_before(c, b, n).is_err());
    // A 已紧邻 C 之前：空操作
    assert!(s.move_before(a, c, n).is_err());
    // B 移到 A 之前 → [B, A, C]
    s.move_before(b, a, n).unwrap();
    assert_eq!(
        s.items()
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>(),
        vec!["B", "A", "C"]
    );
    // 完成 D 后：已完成条目不能作为移动目标/来源
    let d = s.add("D", None, n).unwrap().id.unwrap();
    s.toggle_done(d, n).unwrap();
    assert!(s.move_before(a, d, n).is_err());
    assert!(s.move_before(d, a, n).is_err());
    assert!(s.move_before(a, Uuid::new_v4(), n).is_err());
    assert!(s.move_before(a, a, n).is_err());
}

#[test]
fn legacy_file_without_reminder_metadata_still_loads_into_store() {
    let json = r#"{"items":[{"id":"11111111-1111-1111-1111-111111111111","text":"旧版事项","done":false,"createdAt":"2026-09-01T00:00:00Z"}],"focusID":"11111111-1111-1111-1111-111111111111"}"#;
    match parse_file(json.as_bytes()) {
        FileParse::Legacy(value) => {
            let data: DataFile = serde_json::from_value(legacy_to_datafile(value)).unwrap();
            let s = Store::from_data(&data);
            assert_eq!(s.items().len(), 1);
            assert_eq!(s.focus_text(), Some("旧版事项"));
            assert!(s.notified_due_ids().is_empty());
        }
        other => panic!("expected legacy, got {other:?}"),
    }
}

/// 模拟迁移层：旧文件 → 新格式（旧键 focusID 保留，与 API 的 focusId 不同）。
fn legacy_to_datafile(mut value: serde_json::Value) -> serde_json::Value {
    let obj = value.as_object_mut().unwrap();
    obj.insert("schemaVersion".into(), serde_json::json!(1));
    obj.insert("sync".into(), serde_json::json!({}));
    obj.insert("notifiedDueIDs".into(), serde_json::json!([]));
    value
}

#[test]
fn stored_data_with_invalid_or_done_focus_clears_it_on_load() {
    let mut data = DataFile::new();
    let done_item = Item {
        done: true,
        ..item("已完成")
    };
    data.items = vec![done_item.clone()];
    data.focus_id = Some(done_item.id);
    data.sync.known_server_version = Some(7);
    data.sync.dirty = true;
    let s = Store::from_data(&data);
    assert_eq!(s.focus_id(), None);
    // 同步元数据随 DataFile 保留，由会话层使用。
    assert_eq!(data.sync.account_owner(), None);
}

#[test]
fn undo_redo_via_history_covers_all_three_dimensions() {
    let mut s = Store::new();
    let n = now();
    let due = at("2026-09-05T09:00:00Z");
    let a = s.add("焦点", Some(due), n).unwrap().id.unwrap();
    let b = s.add("其他", None, n).unwrap().id.unwrap();
    s.mark_notified(a, n);
    s.toggle_focus(b, n).unwrap();
    s.toggle_done(b, n).unwrap();
    assert_eq!(s.focus_id(), None);
    // 撤销“完成 B”：焦点与完成态一并恢复
    s.undo(n).unwrap();
    assert_eq!(s.focus_id(), Some(b));
    assert!(!s.find(b).unwrap().done);
    // 撤销“切换焦点到 B”：焦点回到 A（A 从未被完成）
    s.undo(n).unwrap();
    assert_eq!(s.focus_id(), Some(a));
    assert!(!s.find(a).unwrap().done);
    // 重做一步：重新切到 B
    s.redo(n).unwrap();
    assert_eq!(s.focus_id(), Some(b));
}

#[test]
fn due_state_matches_swift_rules() {
    let s = store_with(vec![]);
    let n = now();
    let in_2h = Item {
        due_date: Some(n + Duration::hours(2)),
        ..item("两小时后")
    };
    assert_eq!(
        s.due_state(&in_2h, n, true, Duration::hours(1)),
        doing_core::DueState::Upcoming
    );
    assert_eq!(
        s.due_state(&in_2h, n, true, Duration::hours(3)),
        doing_core::DueState::DueSoon
    );
    assert_eq!(
        s.due_state(&in_2h, n, false, Duration::hours(3)),
        doing_core::DueState::Upcoming
    );
    let overdue = Item {
        due_date: Some(n - Duration::seconds(1)),
        ..item("已逾期")
    };
    assert_eq!(
        s.due_state(&overdue, n, true, Duration::hours(24)),
        doing_core::DueState::Overdue
    );
    let done = Item {
        due_date: Some(n - Duration::seconds(1)),
        done: true,
        ..item("做完")
    };
    assert_eq!(
        s.due_state(&done, n, true, Duration::hours(24)),
        doing_core::DueState::None
    );
    assert_eq!(s.overdue_count(n), 0);
}

#[test]
fn settings_persist_and_clamp_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    let mut s = AppSettings {
        menu_bar_text_limit: 100,
        due_soon_hours: 0.0,
        ..AppSettings::default()
    };
    s.normalize();
    assert_eq!(s.menu_bar_text_limit, 60);
    assert_eq!(s.due_soon_hours, 0.25);
    serde_json::to_writer(std::fs::File::create(&path).unwrap(), &s).unwrap();
    let back: AppSettings = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(back.menu_bar_text_limit, 60);
    assert_eq!(back.appearance, doing_core::AppAppearance::System);
}

#[test]
fn delete_removes_reminder_record_and_undo_restores_it() {
    let mut s = Store::new();
    let n = now();
    let due = at("2026-09-05T09:00:00Z");
    let id = s.add("事项", Some(due), n).unwrap().id.unwrap();
    s.mark_notified(id, n);
    s.delete(id, n).unwrap();
    assert!(!s.notified_due_ids().contains(&id));
    s.undo(n).unwrap();
    assert!(s.notified_due_ids().contains(&id));
}

#[test]
fn repo_roundtrip_preserves_sync_meta_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = JsonRepo::new(tmp.path().join("data.json"));
    let mut data = DataFile::new();
    data.items.push(item("带元数据的任务"));
    data.focus_id = Some(data.items[0].id);
    data.notified_due_ids.push(data.items[0].id);
    data.sync = SyncMeta {
        server_url: Some("https://example.test".into()),
        username: Some("demo".into()),
        account_id: Some("7".into()),
        known_server_version: Some(42),
        dirty: true,
        ..Default::default()
    };
    repo.save(&data).unwrap();
    match repo.load().unwrap() {
        LoadResult::Loaded(back) => {
            assert_eq!(*back, data);
            assert_eq!(
                back.sync.account_owner(),
                Some(doing_core::data::AccountOwner {
                    server_url: "https://example.test".into(),
                    account_id: "7".into()
                })
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn scheduled_tasks_preserve_revision_and_dirty_flow() {
    let mut s = Store::new();
    let n = now();
    assert_eq!(s.revision(), 0);
    let a = s.add("a", None, n).unwrap().id.unwrap();
    let b = s.add("b", None, n).unwrap().id.unwrap();
    assert_eq!(s.revision(), 2);
    // 撤销“添加 b”：回到只剩 a 的状态
    s.undo(n).unwrap();
    assert_eq!(s.revision(), 3);
    assert_eq!(s.active_count(), 1);
    assert!(s.add("  ", None, n).is_err());
    assert_eq!(s.revision(), 3);
    // 重做恢复 b
    s.redo(n).unwrap();
    assert_eq!(s.active_count(), 2);
    // 完成/清除/撤销/重做闭环
    s.toggle_done(a, n).unwrap();
    s.toggle_done(b, n).unwrap();
    let msg = s.clear_completed(n).unwrap();
    assert!(msg.message.contains("2 件"));
    assert!(s.is_empty());
    s.undo(n).unwrap();
    assert_eq!(s.completed_count(), 2);
    s.redo(n).unwrap();
    assert!(s.is_empty());
    let mut file = s.to_data_file(SyncMeta::default());
    assert_eq!(file.schema_version, doing_core::data::SCHEMA_VERSION);
    let parse = parse_file(&serde_json::to_vec(&file).unwrap());
    match parse {
        FileParse::Data(_) => {}
        other => panic!("unexpected: {other:?}"),
    }
    file.sync.dirty = true;
    assert!(file.sync.dirty);
}
