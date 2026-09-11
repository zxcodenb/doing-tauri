//! Swift UserDefaults allowlist and Cocoa-bottom-left → matched Tauri display coordinates.
//! OS notification authorization and login-item state are deliberately NOT preferences to import.
use crate::error::CommandError;
use crate::preferences::WindowOrigin;
use doing_core::settings::AppAppearance;
use doing_core::AppSettings;
use plist::{Dictionary, Value};
use serde::{Deserialize, Serialize};

pub const DOMAIN: &str = "local.chmod777.Doing";
pub const KEYS: &[&str] = &[
    "mode",
    "appearance",
    "showFocusInMenuBar",
    "menuBarTextLimit",
    "notificationsEnabled",
    "notificationSound",
    "dueSoonEnabled",
    "dueSoonHours",
    "showOverdueInMenuBar",
    "showOverdueBanner",
    "automaticSync",
    "panelOrigin",
];

/// Serialize only the reviewed legacy preference keys; never back up an entire defaults domain.
pub fn selected_snapshot(bytes: &[u8]) -> Result<Vec<u8>, CommandError> {
    let value = Value::from_reader(std::io::Cursor::new(bytes))
        .map_err(|_| super::problem("invalidLegacyPreferences", "旧偏好不是有效的属性列表"))?;
    let dict = value
        .as_dictionary()
        .ok_or_else(|| super::problem("invalidLegacyPreferences", "旧偏好必须是属性字典"))?;
    let selected: Dictionary = KEYS
        .iter()
        .filter_map(|key| {
            dict.get(key)
                .map(|value| ((*key).to_owned(), value.clone()))
        })
        .collect();
    let mut output = Vec::new();
    Value::Dictionary(selected)
        .to_writer_xml(&mut output)
        .map_err(|_| super::problem("invalidLegacyPreferences", "无法暂存旧偏好"))?;
    if output.len() > 256 * 1024 {
        return Err(super::problem(
            "invalidLegacyPreferences",
            "旧偏好超出安全导入大小",
        ));
    }
    Ok(output)
}

#[cfg(test)]
pub fn empty_snapshot() -> Vec<u8> {
    let mut output = Vec::new();
    Value::Dictionary(Dictionary::new())
        .to_writer_xml(&mut output)
        .expect("empty plist");
    output
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
impl Rect {
    fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.width > 0.0
            && self.height > 0.0
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Display {
    pub name: String,
    pub cocoa: Rect,
    pub cocoa_work_area: Rect,
    pub scale: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Monitor {
    pub name: String,
    pub physical: Rect,
    pub scale: f64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Geometry {
    pub displays: Vec<Display>,
    pub monitors: Vec<Monitor>,
    pub window_logical_width: f64,
    pub window_logical_height: f64,
}

/// Reject ambiguous display matching rather than guess in a mixed-DPI global coordinate space.
fn convert_origin(value: &str, geometry: &Geometry) -> Option<WindowOrigin> {
    let value = value.trim().strip_prefix('{')?.strip_suffix('}')?;
    let (x, y) = value.split_once(',')?;
    let x: f64 = x.trim().parse().ok()?;
    let y: f64 = y.trim().parse().ok()?;
    let width = geometry.window_logical_width;
    let height = geometry.window_logical_height;
    if ![x, y, width, height].iter().all(|v| v.is_finite()) || width <= 0.0 || height <= 0.0 {
        return None;
    }
    // AppKit's primary screen has the global Cocoa origin (0, 0); its top edge is
    // the Quartz/Tao top-left reference. Mirrors/duplicate origins are ambiguous.
    let mut primary = geometry
        .displays
        .iter()
        .filter(|d| d.cocoa.valid() && d.cocoa.x == 0.0 && d.cocoa.y == 0.0);
    let primary_top = primary.next()?.cocoa.height;
    if primary.next().is_some() {
        return None;
    }
    let mut candidates = Vec::new();
    for display in &geometry.displays {
        if !display.cocoa.valid()
            || !display.cocoa_work_area.valid()
            || !display.scale.is_finite()
            || !(0.25..=8.0).contains(&display.scale)
        {
            continue;
        }
        let matches: Vec<_> = geometry
            .monitors
            .iter()
            .filter(|monitor| {
                monitor.physical.valid()
                    // Tao exposes "Monitor #<model>", not NSScreen.localizedName.
                    // Match the full expected frame in that monitor's physical space instead.
                    && (monitor.physical.x - display.cocoa.x * display.scale).abs() < 1.0
                    && (monitor.physical.y - (primary_top - display.cocoa.y - display.cocoa.height) * display.scale).abs() < 1.0
                    && (monitor.scale - display.scale).abs() < 0.001
                    && (monitor.physical.width - display.cocoa.width * display.scale).abs() < 1.0
                    && (monitor.physical.height - display.cocoa.height * display.scale).abs() < 1.0
            })
            .collect();
        if matches.len() != 1 {
            continue;
        }
        let monitor = matches[0];
        let frame = display.cocoa;
        let scale = display.scale;
        // The new window will adopt the destination monitor's scale, not its current monitor's pixels.
        let width = width * scale;
        let height = height * scale;
        let px = monitor.physical.x + (x - frame.x) * scale;
        let py = monitor.physical.y + (frame.y + frame.height - y) * scale - height;
        let area = display.cocoa_work_area;
        let work = Rect {
            x: monitor.physical.x + (area.x - frame.x) * scale,
            y: monitor.physical.y + (frame.y + frame.height - area.y - area.height) * scale,
            width: area.width * scale,
            height: area.height * scale,
        };
        let intersection_w = (px + width).min(work.x + work.width) - px.max(work.x);
        let intersection_h = (py + height).min(work.y + work.height) - py.max(work.y);
        if intersection_w < width * 0.6 || intersection_h < height * 0.6 {
            continue;
        }
        let x = px
            .clamp(work.x, (work.x + work.width - width).max(work.x))
            .round();
        let y = py
            .clamp(work.y, (work.y + work.height - height).max(work.y))
            .round();
        if [x, y]
            .iter()
            .all(|v| v.is_finite() && *v >= i32::MIN as f64 && *v <= i32::MAX as f64)
        {
            candidates.push(WindowOrigin {
                x: x as i32,
                y: y as i32,
            });
        }
    }
    (candidates.len() == 1).then(|| candidates[0])
}

pub struct Converted {
    pub settings: AppSettings,
    pub origin: Option<WindowOrigin>,
    pub warnings: Vec<String>,
}

pub fn convert(bytes: &[u8], geometry: &Geometry) -> Result<Converted, CommandError> {
    let selected = selected_snapshot(bytes)?;
    let value = Value::from_reader(std::io::Cursor::new(selected)).expect("selected plist");
    let values = value.as_dictionary().expect("selected dictionary");
    let mut settings = AppSettings::default();
    let mut warnings = Vec::new();
    if let Some(value) = values.get("mode") {
        match value.as_string() {
            Some("popover" | "panel") => settings.mode = value.as_string().unwrap().to_owned(),
            _ => warnings.push("旧显示模式无效，使用菜单栏模式".into()),
        }
    }
    if let Some(value) = values.get("appearance") {
        settings.appearance = match value.as_string() {
            Some("dark") => AppAppearance::Dark,
            Some("light") => AppAppearance::Light,
            Some("system") => AppAppearance::System,
            _ => {
                warnings.push("旧外观值无效，使用跟随系统".into());
                AppAppearance::System
            }
        };
    }
    for (key, target) in [
        ("showFocusInMenuBar", &mut settings.show_focus_in_menu_bar),
        ("notificationsEnabled", &mut settings.notifications_enabled),
        ("notificationSound", &mut settings.notification_sound),
        ("dueSoonEnabled", &mut settings.due_soon_enabled),
        (
            "showOverdueInMenuBar",
            &mut settings.show_overdue_in_menu_bar,
        ),
        ("showOverdueBanner", &mut settings.show_overdue_banner),
        ("automaticSync", &mut settings.automatic_sync),
    ] {
        if let Some(value) = values.get(key) {
            if let Some(value) = value.as_boolean() {
                *target = value;
            } else {
                warnings.push(format!("旧偏好 {key} 类型无效，使用默认值"));
            }
        }
    }
    if let Some(value) = values.get("menuBarTextLimit") {
        if let Some(value) = value.as_signed_integer() {
            settings.menu_bar_text_limit = value;
        } else if let Some(value) = value.as_unsigned_integer() {
            settings.menu_bar_text_limit = i64::try_from(value).unwrap_or(i64::MAX);
        } else {
            warnings.push("旧菜单栏字数类型无效，使用默认值".into());
        }
    }
    if let Some(value) = values.get("dueSoonHours") {
        if let Some(value) = value
            .as_real()
            .or_else(|| value.as_signed_integer().map(|n| n as f64))
        {
            settings.due_soon_hours = value;
        } else {
            warnings.push("旧临期小时数类型无效，使用默认值".into());
        }
    }
    if settings.normalize() {
        warnings.push("旧偏好超出范围，已按字数 1–60、临期小时 0.25–168 校正".into());
    }
    let origin = values
        .get("panelOrigin")
        .and_then(Value::as_string)
        .and_then(|value| convert_origin(value, geometry));
    if values.contains_key("panelOrigin") && origin.is_none() {
        warnings.push("旧窗口坐标无法可靠匹配当前屏幕，使用安全默认位置".into());
    }
    warnings.push("系统通知授权和登录项不继承；请在新版设置中重新查询和确认".into());
    Ok(Converted {
        settings,
        origin,
        warnings,
    })
}
