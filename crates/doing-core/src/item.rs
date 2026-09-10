use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::clock::parse_rfc3339;

/// RFC 3339 序列化：UTC 输出 `Z`；无小数秒时省略小数部分，
/// 有小数时截断到毫秒（与旧 Swift JSONEncoder .iso8601 输出形态兼容）。
pub fn serialize_dt<S>(dt: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let base = dt.format("%Y-%m-%dT%H:%M:%S").to_string();
    let nanos = dt.timestamp_subsec_nanos();
    let text = if nanos == 0 {
        format!("{base}Z")
    } else {
        format!("{base}.{:03}Z", nanos / 1_000_000)
    };
    serializer.serialize_str(&text)
}

pub fn deserialize_dt<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse_rfc3339(&s).map_err(serde::de::Error::custom)
}

pub fn serialize_opt_dt<S>(opt: &Option<DateTime<Utc>>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match opt {
        Some(dt) => serialize_dt(dt, serializer),
        None => serializer.serialize_none(),
    }
}

pub fn deserialize_opt_dt<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    match opt {
        Some(s) => parse_rfc3339(&s).map(Some).map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

/// 一条记录。字段刻意保持最少：文本、完成态、创建时间、可选截止时间。
/// dueDate 为 nil 表示无截止；可选字段保证旧数据文件无需迁移即可解码。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: Uuid,
    pub text: String,
    pub done: bool,
    #[serde(serialize_with = "serialize_dt", deserialize_with = "deserialize_dt")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "serialize_opt_dt", deserialize_with = "deserialize_opt_dt")]
    pub due_date: Option<DateTime<Utc>>,
    #[serde(serialize_with = "serialize_dt", deserialize_with = "deserialize_dt")]
    pub updated_at: DateTime<Utc>,
}

impl Item {
    pub fn new(text: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            text: text.into(),
            done: false,
            created_at: now,
            due_date: None,
            updated_at: now,
        }
    }
}

/// 解码兼容旧数据：缺失 id / done / createdAt 时取默认值（与 Swift 版一致）。
/// 重复/缺失标识等异常由迁移层严格校验，此处只负责字段兜底。
impl<'de> Deserialize<'de> for Item {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            id: Option<Uuid>,
            text: String,
            done: Option<bool>,
            #[serde(default, deserialize_with = "deserialize_opt_dt")]
            created_at: Option<DateTime<Utc>>,
            #[serde(default, deserialize_with = "deserialize_opt_dt")]
            due_date: Option<DateTime<Utc>>,
            #[serde(default, deserialize_with = "deserialize_opt_dt")]
            updated_at: Option<DateTime<Utc>>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let created = raw.created_at.unwrap_or_else(Utc::now);
        Ok(Self {
            id: raw.id.unwrap_or_else(Uuid::new_v4),
            text: raw.text,
            done: raw.done.unwrap_or(false),
            created_at: created,
            due_date: raw.due_date,
            updated_at: raw.updated_at.unwrap_or(created),
        })
    }
}

/// 统一的“整毫秒”规范化（测试用），保证可预期的时间文本。
pub fn fixed_dt(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    NaiveDate::from_ymd_opt(y, mo, d)
        .and_then(|n| n.and_hms_opt(h, mi, s))
        .map(|n| n.and_utc())
        .expect("valid date")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_with_defaults_when_fields_missing() {
        let json = r#"{"text":"旧版事项"}"#;
        let item: Item = serde_json::from_str(json).unwrap();
        assert!(item.text == "旧版事项");
        assert!(!item.done);
        assert_eq!(item.due_date, None);
        assert!(item.created_at <= Utc::now());
        assert_eq!(item.updated_at, item.created_at);
    }

    #[test]
    fn roundtrip_uses_camel_case_keys_and_iso8601() {
        let now = fixed_dt(2026, 9, 3, 0, 0, 0);
        let item = Item {
            id: Uuid::new_v4(),
            text: "中文事项".into(),
            done: false,
            created_at: now,
            due_date: Some(now),
            updated_at: now,
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("\"createdAt\":\"2026-09-03T00:00:00Z\""), "{json}");
        assert!(json.contains("\"dueDate\":\"2026-09-03T00:00:00Z\""), "{json}");
        let back: Item = serde_json::from_str(&json).unwrap();
        assert_eq!(back, item);
    }

    #[test]
    fn whole_seconds_serialize_without_fractional_part() {
        let now = fixed_dt(2026, 9, 3, 0, 0, 0);
        let item = Item {
            created_at: now,
            updated_at: now,
            ..Item::new("x")
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("2026-09-03T00:00:00Z"), "{json}");
        assert!(!json.contains('+'), "{json}");
    }

    #[test]
    fn fractional_seconds_truncate_to_millis() {
        let now = DateTime::parse_from_rfc3339("2026-09-03T00:00:00.123456789Z")
            .unwrap()
            .with_timezone(&Utc);
        let item = Item {
            created_at: now,
            updated_at: now,
            ..Item::new("x")
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("2026-09-03T00:00:00.123Z"), "{json}");
    }
}
