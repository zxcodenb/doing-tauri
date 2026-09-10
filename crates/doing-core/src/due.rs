use chrono::{DateTime, Duration, Utc};

/// 截止时间的三态展示分类。已完成的条目固定视为 None（做完就清静）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueState {
    None,
    Upcoming,
    DueSoon,
    Overdue,
}

pub fn classify(
    due: Option<DateTime<Utc>>,
    done: bool,
    now: DateTime<Utc>,
    due_soon_enabled: bool,
    due_soon_interval: Duration,
) -> DueState {
    let Some(due) = due else { return DueState::None };
    if done {
        return DueState::None;
    }
    if due <= now {
        return DueState::Overdue;
    }
    if due_soon_enabled && due - now <= due_soon_interval {
        return DueState::DueSoon;
    }
    DueState::Upcoming
}

/// 到期判定：未完成且 due <= now。
pub fn is_overdue(due: Option<DateTime<Utc>>, done: bool, now: DateTime<Utc>) -> bool {
    !done && matches!(due, Some(d) if d <= now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn completed_is_never_classified() {
        let now = at(1_800_000_000);
        assert_eq!(classify(Some(now - Duration::hours(2)), true, now, true, Duration::hours(24)), DueState::None);
        assert_eq!(classify(None, false, now, true, Duration::hours(24)), DueState::None);
    }

    #[test]
    fn overdue_takes_priority() {
        let now = at(1_800_000_000);
        assert_eq!(classify(Some(now), false, now, true, Duration::hours(24)), DueState::Overdue);
        assert_eq!(classify(Some(now - Duration::seconds(1)), false, now, true, Duration::hours(24)), DueState::Overdue);
    }

    #[test]
    fn due_soon_uses_configured_threshold() {
        let now = at(1_800_000_000);
        let in2h = Some(now + Duration::hours(2));
        assert_eq!(classify(in2h, false, now, true, Duration::hours(1)), DueState::Upcoming);
        assert_eq!(classify(in2h, false, now, true, Duration::hours(3)), DueState::DueSoon);
        assert_eq!(classify(in2h, false, now, false, Duration::hours(3)), DueState::Upcoming);
    }
}
