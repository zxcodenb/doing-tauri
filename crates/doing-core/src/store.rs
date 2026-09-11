use chrono::{DateTime, Utc};
use std::collections::HashSet;
use uuid::Uuid;

use crate::data::{DataFile, SyncMeta};
use crate::due::{classify, is_overdue, DueState};
use crate::error::{CoreError, Result};
use crate::item::Item;

/// 操作报告：供 UI 反馈条与历史标题使用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mutation {
    /// 历史标题（“撤销”按钮/菜单显示用）。
    pub title: String,
    /// 即时反馈文案。
    pub message: String,
    /// 相关条目 id（新增时返回新条目）。
    pub id: Option<Uuid>,
    /// 反馈条是否提供“撤销”按钮。
    pub offers_undo: bool,
}

impl Mutation {
    fn new(title: impl Into<String>, message: impl Into<String>, id: Option<Uuid>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            id,
            offers_undo: true,
        }
    }
}

const HISTORY_LIMIT: usize = 30;

/// 撤销/重做栈条目：恢复条目、焦点与提醒资格三个维度。
#[derive(Debug, Clone, PartialEq)]
struct HistoryEntry {
    title: String,
    items: Vec<Item>,
    focus_id: Option<Uuid>,
    notified_due_ids: Vec<Uuid>,
}

/// 领域 Store：任务数组顺序、唯一焦点、撤销/重做历史与提醒记录。
/// 不持有 I/O：变更后由上层负责防抖持久化与事件广播。
/// 历史是会话内状态：云端替换 / 登出必须 clear_history。
#[derive(Debug, Clone)]
pub struct Store {
    items: Vec<Item>,
    focus_id: Option<Uuid>,
    notified_due_ids: Vec<Uuid>,
    undo_stack: Vec<HistoryEntry>,
    redo_stack: Vec<HistoryEntry>,
    revision: u64,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            focus_id: None,
            notified_due_ids: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            revision: 0,
        }
    }

    /// 从已校验的数据文件建立 Store（加载路径；焦点非法则清除）。
    pub fn from_data(data: &DataFile) -> Self {
        let mut notified = data.notified_due_ids.clone();
        notified.dedup();
        let valid_focus = data
            .focus_id
            .filter(|id| data.items.iter().any(|it| it.id == *id && !it.done));
        Self {
            items: data.items.clone(),
            focus_id: valid_focus,
            notified_due_ids: notified,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            revision: data.revision,
        }
    }

    // MARK: - 访问

    pub fn items(&self) -> &[Item] {
        &self.items
    }
    pub fn focus_id(&self) -> Option<Uuid> {
        self.focus_id
    }
    pub fn notified_due_ids(&self) -> &[Uuid] {
        &self.notified_due_ids
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn focused_item(&self) -> Option<&Item> {
        self.focus_id
            .and_then(|id| self.items.iter().find(|it| it.id == id && !it.done))
    }
    pub fn focus_text(&self) -> Option<&str> {
        self.focused_item().map(|it| it.text.as_str())
    }
    pub fn active_count(&self) -> usize {
        self.items.iter().filter(|it| !it.done).count()
    }
    pub fn completed_items(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|it| it.done)
    }
    pub fn completed_count(&self) -> usize {
        self.items.iter().filter(|it| it.done).count()
    }
    pub fn has_completed(&self) -> bool {
        self.items.iter().any(|it| it.done)
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Focus first；然后计划内任务（同日稳定），再手工顺序任务。
    pub fn remaining_items(&self) -> Vec<Item> {
        let focused = self.focused_item().map(|it| it.id);
        let active: Vec<(usize, &Item)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| !it.done && Some(it.id) != focused)
            .collect();
        let mut scheduled: Vec<(usize, &Item)> = active
            .iter()
            .copied()
            .filter(|(_, it)| it.due_date.is_some())
            .collect();
        scheduled.sort_by(|a, b| {
            let da = a.1.due_date.unwrap();
            let db = b.1.due_date.unwrap();
            if da == db {
                a.0.cmp(&b.0) // 同日保持稳定（原数组顺序）
            } else {
                da.cmp(&db)
            }
        });
        let manual: Vec<&Item> = active
            .iter()
            .filter(|(_, it)| it.due_date.is_none())
            .map(|(_, it)| *it)
            .collect();
        scheduled
            .into_iter()
            .map(|(_, it)| it.clone())
            .chain(manual.into_iter().cloned())
            .collect()
    }

    /// 展示顺序：焦点卡 → 其余未完成（排序）→ 已完成（原顺序）。
    pub fn ordered(&self) -> Vec<Item> {
        let focus = self.focused_item().cloned();
        let remaining = self.remaining_items();
        let completed: Vec<Item> = self.completed_items().cloned().collect();
        focus
            .into_iter()
            .chain(remaining)
            .chain(completed)
            .collect()
    }

    pub fn has_overdue(&self, now: DateTime<Utc>) -> bool {
        self.overdue_count(now) > 0
    }
    pub fn overdue_count(&self, now: DateTime<Utc>) -> usize {
        self.items
            .iter()
            .filter(|it| is_overdue(it.due_date, it.done, now))
            .count()
    }

    /// UI 分类（完成项恒为 None）。
    pub fn due_state(
        &self,
        item: &Item,
        now: DateTime<Utc>,
        due_soon_enabled: bool,
        due_soon_interval: chrono::Duration,
    ) -> DueState {
        classify(
            item.due_date,
            item.done,
            now,
            due_soon_enabled,
            due_soon_interval,
        )
    }

    pub fn undo_title(&self) -> Option<&str> {
        self.undo_stack.last().map(|e| e.title.as_str())
    }
    pub fn redo_title(&self) -> Option<&str> {
        self.redo_stack.last().map(|e| e.title.as_str())
    }

    // MARK: - 变更

    fn trimmed(text: &str) -> String {
        text.trim().to_string()
    }

    /// 新增：焦点为空时新任务自动成为焦点；插入到第一件已完成之前。
    pub fn add(
        &mut self,
        text: impl AsRef<str>,
        due: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<Mutation> {
        let text = Self::trimmed(text.as_ref());
        if text.is_empty() {
            return Err(CoreError::NoOp);
        }
        self.checkpoint("新增事项");
        let item = Item {
            id: Uuid::new_v4(),
            text,
            done: false,
            created_at: now,
            due_date: due,
            updated_at: now,
        };
        let insert_at = self
            .items
            .iter()
            .position(|it| it.done)
            .unwrap_or(self.items.len());
        let becomes_focus = self.focused_item().is_none();
        let id = item.id;
        self.items.insert(insert_at, item);
        if becomes_focus {
            self.focus_id = Some(id);
        }
        self.did_mutate(now);
        Ok(Mutation::new("新增事项", "已添加事项", Some(id)))
    }

    pub fn set_due(
        &mut self,
        id: Uuid,
        due: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<Mutation> {
        let index = self
            .items
            .iter()
            .position(|it| it.id == id)
            .ok_or(CoreError::NotFound)?;
        if self.items[index].due_date == due {
            return Err(CoreError::NoOp);
        }
        self.checkpoint("修改截止时间");
        self.items[index].due_date = due;
        // 改期后的任务对“新的提醒”重新获得资格。
        self.notified_due_ids.retain(|n| *n != id);
        let message = if due.is_none() {
            "已移除截止时间"
        } else {
            "已更新截止时间"
        };
        self.did_mutate(now);
        Ok(Mutation::new("修改截止时间", message, Some(id)))
    }

    pub fn toggle_done(&mut self, id: Uuid, now: DateTime<Utc>) -> Result<Mutation> {
        let index = self
            .items
            .iter()
            .position(|it| it.id == id)
            .ok_or(CoreError::NotFound)?;
        let was_done = self.items[index].done;
        self.checkpoint(if was_done {
            "恢复事项"
        } else {
            "完成事项"
        });
        self.items[index].done = !was_done;
        if !was_done && self.focus_id == Some(id) {
            self.focus_id = None;
        }
        let message = if was_done {
            "已恢复到待办"
        } else {
            "完成了，又少一件事。"
        };
        self.did_mutate(now);
        Ok(Mutation::new(
            if was_done {
                "恢复事项"
            } else {
                "完成事项"
            },
            message,
            Some(id),
        ))
    }

    pub fn rename(&mut self, id: Uuid, text: &str, now: DateTime<Utc>) -> Result<Mutation> {
        let text = Self::trimmed(text);
        if text.is_empty() {
            return Err(CoreError::NoOp);
        }
        let index = self
            .items
            .iter()
            .position(|it| it.id == id)
            .ok_or(CoreError::NotFound)?;
        if self.items[index].text == text {
            return Err(CoreError::NoOp);
        }
        self.checkpoint("编辑事项");
        self.items[index].text = text;
        self.did_mutate(now);
        Ok(Mutation::new("编辑事项", "已保存修改", Some(id)))
    }

    pub fn delete(&mut self, id: Uuid, now: DateTime<Utc>) -> Result<Mutation> {
        if !self.items.iter().any(|it| it.id == id) {
            return Err(CoreError::NotFound);
        }
        self.checkpoint("删除事项");
        self.items.retain(|it| it.id != id);
        if self.focus_id == Some(id) {
            self.focus_id = None;
        }
        self.notified_due_ids.retain(|n| *n != id);
        self.did_mutate(now);
        Ok(Mutation::new("删除事项", "已删除事项", Some(id)))
    }

    pub fn toggle_focus(&mut self, id: Uuid, now: DateTime<Utc>) -> Result<Mutation> {
        let exists_active = self.items.iter().any(|it| it.id == id && !it.done);
        if !exists_active {
            return Err(CoreError::InvalidFocus);
        }
        self.checkpoint("切换焦点");
        if self.focus_id == Some(id) {
            self.focus_id = None;
        } else {
            self.focus_id = Some(id);
        }
        let message = if self.focus_id == Some(id) {
            "接下来，专注这一件。"
        } else {
            "已取消当前焦点"
        };
        self.did_mutate(now);
        Ok(Mutation::new("切换焦点", message, Some(id)))
    }

    pub fn clear_completed(&mut self, now: DateTime<Utc>) -> Result<Mutation> {
        let count = self.completed_count();
        if count == 0 {
            return Err(CoreError::NoOp);
        }
        self.checkpoint("清除已完成");
        self.items.retain(|it| !it.done);
        self.did_mutate(now);
        Ok(Mutation::new(
            "清除已完成",
            format!("已清除 {count} 件已完成事项"),
            None,
        ))
    }

    /// 移动未完成任务到另一未完成任务之前。
    pub fn move_before(
        &mut self,
        id: Uuid,
        target_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Mutation> {
        if id == target_id {
            return Err(CoreError::NoOp);
        }
        let from = self
            .items
            .iter()
            .position(|it| it.id == id && !it.done)
            .ok_or(CoreError::NotFound)?;
        let to = self
            .items
            .iter()
            .position(|it| it.id == target_id && !it.done)
            .ok_or(CoreError::NotFound)?;
        // from == to - 1（已紧邻目标之前）视为空操作；用 from + 1 避免 usize 下溢。
        if from + 1 == to {
            return Err(CoreError::NoOp);
        }
        self.checkpoint("移动事项");
        let item = self.items.remove(from);
        self.items.insert(if from < to { to - 1 } else { to }, item);
        self.did_mutate(now);
        Ok(Mutation::new("移动事项", "已调整事项顺序", Some(id)))
    }

    pub fn undo(&mut self, now: DateTime<Utc>) -> Result<Mutation> {
        let Some(entry) = self.undo_stack.pop() else {
            return Err(CoreError::NoOp);
        };
        self.redo_stack.push(HistoryEntry {
            title: entry.title.clone(),
            items: std::mem::take(&mut self.items),
            focus_id: self.focus_id,
            notified_due_ids: std::mem::take(&mut self.notified_due_ids),
        });
        self.items = entry.items;
        self.focus_id = entry.focus_id;
        self.notified_due_ids = entry.notified_due_ids;
        let title = entry.title;
        self.did_mutate(now);
        Ok(Mutation {
            title: title.clone(),
            message: format!("已撤销{title}"),
            id: None,
            offers_undo: false,
        })
    }

    pub fn redo(&mut self, now: DateTime<Utc>) -> Result<Mutation> {
        let Some(entry) = self.redo_stack.pop() else {
            return Err(CoreError::NoOp);
        };
        self.undo_stack.push(HistoryEntry {
            title: entry.title.clone(),
            items: std::mem::take(&mut self.items),
            focus_id: self.focus_id,
            notified_due_ids: std::mem::take(&mut self.notified_due_ids),
        });
        self.items = entry.items;
        self.focus_id = entry.focus_id;
        self.notified_due_ids = entry.notified_due_ids;
        let title = entry.title;
        self.did_mutate(now);
        Ok(Mutation {
            title: title.clone(),
            message: format!("已重做{title}"),
            id: None,
            offers_undo: true,
        })
    }

    pub fn clear_history(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    /// 云端替换 / 登出后调用：清除会话历史；提醒记录仅保留“仍存在且截止未变”的条目。
    pub fn replace_all(&mut self, items: Vec<Item>, focus_id: Option<Uuid>) {
        let previous: std::collections::HashMap<Uuid, Option<DateTime<Utc>>> =
            self.items.iter().map(|it| (it.id, it.due_date)).collect();
        self.notified_due_ids.retain(|id| {
            items
                .iter()
                .any(|it| it.id == *id && previous.get(&it.id) == Some(&it.due_date))
        });
        self.items = items;
        self.focus_id = self
            .items
            .iter()
            .find(|it| Some(it.id) == focus_id && !it.done)
            .map(|it| it.id);
        self.clear_history();
        self.revision += 1;
    }

    // MARK: - 内部

    fn checkpoint(&mut self, title: &str) {
        self.undo_stack.push(HistoryEntry {
            title: title.to_string(),
            items: self.items.clone(),
            focus_id: self.focus_id,
            notified_due_ids: self.notified_due_ids.clone(),
        });
        if self.undo_stack.len() > HISTORY_LIMIT {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    fn did_mutate(&mut self, now: DateTime<Utc>) {
        // 与 Swift 行为一致：任何变更把所有条目的 updatedAt 推进到当前时刻。
        for item in &mut self.items {
            item.updated_at = now;
        }
        self.revision += 1;
    }

    // MARK: - 持久化视图

    pub fn to_data_file(&self, sync: SyncMeta) -> DataFile {
        DataFile {
            schema_version: crate::data::SCHEMA_VERSION,
            revision: self.revision,
            items: self.items.clone(),
            focus_id: self.focus_id,
            notified_due_ids: self.notified_due_ids.clone(),
            sync,
        }
    }

    /// 已提醒过且条目仍存在的 id 集合（供外部快速查询）。
    pub fn notified_set(&self) -> HashSet<Uuid> {
        self.notified_due_ids.iter().copied().collect()
    }

    pub fn mark_notified(&mut self, id: Uuid, _now: DateTime<Utc>) {
        if self.items.iter().any(|it| it.id == id) && !self.notified_due_ids.contains(&id) {
            self.notified_due_ids.push(id);
            self.revision += 1;
        }
    }

    pub fn has_item(&self, id: Uuid) -> bool {
        self.items.iter().any(|it| it.id == id)
    }

    pub fn find(&self, id: Uuid) -> Option<&Item> {
        self.items.iter().find(|it| it.id == id)
    }

    /// 通知点击“定位”用：id 是否存在（含已完成）。
    pub fn contains_id(&self, id: Uuid) -> bool {
        self.items.iter().any(|it| it.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::fixed_dt;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn add_trims_and_noops_on_empty() {
        let mut s = Store::new();
        let now = at(1_800_000_000);
        assert!(s.add(" \n ", None, now).is_err());
        let id = s.add("  中文草稿  ", None, now).unwrap().id.unwrap();
        assert_eq!(s.find(id).unwrap().text, "中文草稿");
        assert_eq!(s.focus_id, Some(id));
    }

    #[test]
    fn new_item_goes_before_first_completed() {
        let mut s = Store::new();
        let now = at(1_800_000_000);
        let a = s.add("A", None, now).unwrap().id.unwrap();
        let b = s.add("B", None, now).unwrap().id.unwrap();
        s.toggle_done(a, now).unwrap();
        let c = s.add("C", None, now).unwrap().id.unwrap();
        // 插入点 = 第一件已完成的索引（0），已完成/未完成保持“未完成在前”的不变量。
        assert_eq!(
            s.items().iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![c, a, b]
        );
        // 展示顺序：焦点 C 单独成卡，其余未完成 B，已完成 A 折叠区。
        let ordered: Vec<Uuid> = s.ordered().iter().map(|i| i.id).collect();
        assert_eq!(ordered, vec![c, b, a]);
    }

    #[test]
    fn completing_focus_clears_it_and_cannot_refocus_done() {
        let mut s = Store::new();
        let now = at(1_800_000_000);
        let a = s.add("焦点", None, now).unwrap().id.unwrap();
        s.toggle_done(a, now).unwrap();
        assert_eq!(s.focus_id, None);
        assert!(s.toggle_focus(a, now).is_err());
        assert!(s.toggle_focus(Uuid::new_v4(), now).is_err());
    }

    #[test]
    fn ordering_stable_for_same_dates() {
        let mut s = Store::new();
        let now = at(1_800_000_000);
        let focus = s.add("焦点", None, now).unwrap().id.unwrap();
        let date = fixed_dt(2027, 1, 15, 0, 0, 0);
        let a = s.add("同一时间 A", Some(date), now).unwrap().id.unwrap();
        let b = s.add("同一时间 B", Some(date), now).unwrap().id.unwrap();
        let early = s
            .add("更早", Some(date - chrono::Duration::hours(1)), now)
            .unwrap()
            .id
            .unwrap();
        let manual = s.add("无日期", None, now).unwrap().id.unwrap();
        let ids: Vec<Uuid> = s.ordered().iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![focus, early, a, b, manual]);
    }

    #[test]
    fn undo_restores_focus_and_reminder_eligibility() {
        let mut s = Store::new();
        let now = at(1_800_000_000);
        let old = at(1_700_000_000);
        let id = s.add("事项", Some(old), now).unwrap().id.unwrap();
        s.mark_notified(id, now);
        s.set_due(id, Some(old + chrono::Duration::hours(1)), now)
            .unwrap();
        assert!(!s.notified_due_ids().contains(&id));
        s.undo(now).unwrap();
        assert_eq!(s.find(id).unwrap().due_date, Some(old));
        assert!(s.notified_due_ids().contains(&id));
    }

    #[test]
    fn undo_and_redo_full_flow() {
        let mut s = Store::new();
        let now = at(1_800_000_000);
        let a = s.add("焦点", None, now).unwrap().id.unwrap();
        let _b = s.add("其他", None, now).unwrap().id.unwrap();
        s.toggle_done(a, now).unwrap();
        assert_eq!(s.focus_id, None);
        s.undo(now).unwrap();
        assert_eq!(s.focus_id, Some(a));
        s.delete(a, now).unwrap();
        assert_eq!(s.items().len(), 1);
        s.undo(now).unwrap();
        assert_eq!(s.items().len(), 2);
        s.redo(now).unwrap();
        assert_eq!(s.items().len(), 1);
    }

    #[test]
    fn noop_does_not_create_history() {
        let mut s = Store::new();
        let now = at(1_800_000_000);
        assert!(s.add("  ", None, now).is_err());
        assert!(s.toggle_focus(Uuid::new_v4(), now).is_err());
        assert!(s.clear_completed(now).is_err());
        assert_eq!(s.undo_title(), None);
        let id = s.add("keep", None, now).unwrap().id.unwrap();
        s.clear_history();
        assert!(s.rename(id, "keep", now).is_err());
        assert!(s.set_due(id, None, now).is_err());
        assert_eq!(s.undo_title(), None);
        assert!(s.undo(now).is_err());
    }

    #[test]
    fn history_is_capped_at_30() {
        let mut s = Store::new();
        let now = at(1_800_000_000);
        for i in 0..40 {
            s.add(format!("t{i}"), None, now).unwrap();
        }
        // 栈内最多 30 条历史：最老的 10 次添加无法撤销。
        for _ in 0..30 {
            s.undo(now).unwrap();
        }
        assert_eq!(s.items().len(), 10);
        assert!(s.undo(now).is_err());
    }

    #[test]
    fn replace_all_keeps_only_still_relevant_reminders_and_clears_history() {
        let mut s = Store::new();
        let now = at(1_800_000_000);
        let old = at(1_700_000_000);
        let id = s.add("事项", Some(old), now).unwrap().id.unwrap();
        s.mark_notified(id, now);
        let cloud = Item {
            id,
            text: "新的截止时间".into(),
            done: false,
            created_at: now,
            due_date: Some(old + chrono::Duration::hours(2)),
            updated_at: now,
        };
        s.replace_all(vec![cloud], Some(id));
        assert!(!s.notified_due_ids().contains(&id));
        assert_eq!(s.undo_title(), None);
        s.undo(now).unwrap_err(); // 云端替换后历史为空
        assert_eq!(s.items().len(), 1);
    }
    #[test]
    fn reminder_metadata_does_not_change_task_timestamps_or_create_false_cloud_differences() {
        let mut store = Store::new();
        let now = chrono::Utc::now();
        let id = store
            .add("提醒仅是本机元数据", Some(now), now)
            .unwrap()
            .id
            .unwrap();
        let before = store.items().to_vec();
        let revision = store.revision();
        store.mark_notified(id, now + chrono::Duration::minutes(2));
        assert_eq!(store.items(), before, "提醒记录不能伪造成云端任务编辑");
        assert_eq!(store.revision(), revision + 1);
    }
}
