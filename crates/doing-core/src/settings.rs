use serde::{Deserialize, Serialize};

pub const DEFAULT_MENU_BAR_TEXT_LIMIT: i64 = 18;
pub const DEFAULT_DUE_SOON_HOURS: f64 = 24.0;
pub const MODE_POPOVER: &str = "popover";
pub const MODE_PANEL: &str = "panel";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppAppearance {
    System,
    Light,
    Dark,
}

impl AppAppearance {
    pub fn title(self) -> &'static str {
        match self {
            AppAppearance::System => "跟随系统",
            AppAppearance::Light => "浅色",
            AppAppearance::Dark => "深色",
        }
    }
}

/// 本机偏好。任务数据和凭证分别由 Store 与凭据存储管理，不放在这里。
/// 所有字段在读取时收敛到允许范围；未知 JSON 键忽略。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    pub appearance: AppAppearance,
    /// "popover" | "panel"
    pub mode: String,
    pub show_focus_in_menu_bar: bool,
    pub menu_bar_text_limit: i64,
    pub notifications_enabled: bool,
    pub notification_sound: bool,
    pub due_soon_enabled: bool,
    pub due_soon_hours: f64,
    pub show_overdue_in_menu_bar: bool,
    pub show_overdue_banner: bool,
    pub automatic_sync: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            appearance: AppAppearance::System,
            mode: MODE_POPOVER.into(),
            show_focus_in_menu_bar: true,
            menu_bar_text_limit: DEFAULT_MENU_BAR_TEXT_LIMIT,
            notifications_enabled: true,
            notification_sound: true,
            due_soon_enabled: true,
            due_soon_hours: DEFAULT_DUE_SOON_HOURS,
            show_overdue_in_menu_bar: true,
            show_overdue_banner: true,
            automatic_sync: true,
        }
    }
}

impl AppSettings {
    pub fn mode_is_panel(&self) -> bool {
        self.mode == MODE_PANEL
    }
    pub fn due_soon_interval(&self) -> chrono::Duration {
        chrono::Duration::milliseconds((self.due_soon_hours * 3600.0 * 1000.0) as i64)
    }
    /// 读取/写入统一收敛，返回是否发生了收敛（值被修改）。
    pub fn normalize(&mut self) -> bool {
        let mut changed = false;
        if self.mode != MODE_POPOVER && self.mode != MODE_PANEL {
            self.mode = MODE_POPOVER.into();
            changed = true;
        }
        if !(1..=60).contains(&self.menu_bar_text_limit) {
            self.menu_bar_text_limit = self.menu_bar_text_limit.clamp(1, 60);
            changed = true;
        }
        if !self.due_soon_hours.is_finite() {
            self.due_soon_hours = DEFAULT_DUE_SOON_HOURS;
            changed = true;
        }
        if !(0.25..=168.0).contains(&self.due_soon_hours) {
            self.due_soon_hours = self.due_soon_hours.clamp(0.25, 168.0);
            changed = true;
        }
        changed
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// 设置合法化后的副本。
    pub fn sanitized(mut self) -> Self {
        self.normalize();
        self
    }
}

/// 偏好变更点：Tauri 层据此触发系统行为（外观/模式/提醒调度/自动同步）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsChange {
    Appearance,
    Mode,
    MenuBar,
    Reminders,
    DueSoon,
    Sync,
    None_,
}

impl AppSettings {
    pub fn diff(&self, before: &AppSettings) -> Vec<SettingsChange> {
        let mut out = Vec::new();
        if self.appearance != before.appearance {
            out.push(SettingsChange::Appearance);
        }
        if self.mode != before.mode {
            out.push(SettingsChange::Mode);
        }
        if self.show_focus_in_menu_bar != before.show_focus_in_menu_bar
            || self.menu_bar_text_limit != before.menu_bar_text_limit
            || self.show_overdue_in_menu_bar != before.show_overdue_in_menu_bar
        {
            out.push(SettingsChange::MenuBar);
        }
        if self.notifications_enabled != before.notifications_enabled
            || self.notification_sound != before.notification_sound
        {
            out.push(SettingsChange::Reminders);
        }
        if self.due_soon_enabled != before.due_soon_enabled
            || self.due_soon_hours != before.due_soon_hours
        {
            out.push(SettingsChange::DueSoon);
        }
        if self.automatic_sync != before.automatic_sync {
            out.push(SettingsChange::Sync);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_swift_registration() {
        let s = AppSettings::default();
        assert_eq!(s.appearance, AppAppearance::System);
        assert_eq!(s.mode, "popover");
        assert!(s.show_focus_in_menu_bar);
        assert_eq!(s.menu_bar_text_limit, 18);
        assert!(s.notifications_enabled);
        assert!(s.notification_sound);
        assert!(s.due_soon_enabled);
        assert_eq!(s.due_soon_hours, 24.0);
        assert!(s.show_overdue_in_menu_bar);
        assert!(s.show_overdue_banner);
        assert!(s.automatic_sync);
    }

    #[test]
    fn clamps_out_of_range_values() {
        let mut s = AppSettings {
            menu_bar_text_limit: 100,
            due_soon_hours: 0.0,
            ..AppSettings::default()
        };
        assert!(s.normalize());
        assert_eq!(s.menu_bar_text_limit, 60);
        assert_eq!(s.due_soon_hours, 0.25);
        s.due_soon_hours = 999.0;
        s.menu_bar_text_limit = -3;
        s.normalize();
        assert_eq!(s.menu_bar_text_limit, 1);
        assert_eq!(s.due_soon_hours, 168.0);
    }

    #[test]
    fn decodes_unknown_keys_and_keeps_known() {
        let json = r#"{"appearance":"dark","mode":"panel","futureKey":1}"#;
        let s: AppSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.appearance, AppAppearance::Dark);
        assert_eq!(s.mode, "panel");
        assert_eq!(s.menu_bar_text_limit, 18);
    }
}
