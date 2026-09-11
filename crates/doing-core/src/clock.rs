use chrono::{DateTime, Utc};

/// 可替换时钟：核心逻辑不直接调用 `Utc::now()`，测试注入固定时间。
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// 默认系统时钟。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// 测试用固定时钟。
#[derive(Debug, Clone)]
pub struct FixedClock(pub DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// 可变固定时钟（测试中推进时间）。
#[derive(Debug, Clone)]
pub struct MutableClock(pub std::sync::Arc<std::sync::Mutex<DateTime<Utc>>>);

impl MutableClock {
    pub fn new(at: DateTime<Utc>) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(at)))
    }
    pub fn advance(&self, by: chrono::Duration) {
        *self.0.lock().unwrap() += by;
    }
}

impl Clock for MutableClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

/// RFC 3339 / ISO 8601 解析辅助，兼容旧文件（无小数秒、带 Z / 偏移）。
pub fn parse_rfc3339(input: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    let trimmed = input.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }
    // 兜底：旧文件可能使用无时区的本地时间字符串。
    DateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S").map(|dt| dt.with_timezone(&Utc))
}
