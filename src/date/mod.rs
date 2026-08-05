//! Unified date/time utilities — module root.
//!
//! All wrappers return [`Epoch`] for timestamps and seconds (i64) for durations.
//! No chrono or humantime types leak to callers — all formatting and parsing is encapsulated here.
//!
//! Sub-modules:
//! - [`parse`] — date & datetime string parsing
//! - [`parse_duration`] — human-readable duration parsing
//! - [`format`] — epoch/duration formatting

pub mod deserialize;
pub mod format;
pub mod parse;
pub mod parse_duration;

/// Type alias for Unix epoch seconds.
pub type Epoch = i64;

// Re-export sub-module functions at the crate::date level.
pub use format::{
    format_date, format_date_dmy, format_date_time, format_datetime_short, format_duration,
    format_time,
};
pub use parse::{parse_date, parse_datetime, DateDialect};
pub use parse_duration::parse_duration_secs;

/// Current Unix epoch timestamp (seconds).
pub fn now() -> Epoch {
    chrono::Local::now().timestamp()
}

/// Epoch seconds for start of today (midnight local time).
pub fn today_start() -> Epoch {
    let now = chrono::Local::now();
    let naive = now.date_naive().and_hms_opt(0, 0, 0).unwrap();
    local_timestamp(naive).unwrap_or(now.timestamp())
}

/// Epoch seconds for end of today (23:59:59 local time).
pub fn today_end() -> Epoch {
    let now = chrono::Local::now();
    let naive = now.date_naive().and_hms_opt(23, 59, 59).unwrap();
    local_timestamp(naive).unwrap_or(now.timestamp())
}

/// Epoch seconds for the Monday of the current week (midnight).
pub fn week_monday() -> Epoch {
    week_start(chrono::Weekday::Mon)
}

/// Epoch seconds for the start of the current week (midnight), where
/// the week begins on `weekday` (config.grid.week_start).
pub fn week_start(weekday: chrono::Weekday) -> Epoch {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let today_offset = now.weekday().num_days_from_monday() as i64;
    let target_offset = weekday.num_days_from_monday() as i64;
    let back = (today_offset - target_offset).rem_euclid(7);
    let start = now - chrono::Duration::days(back);
    let naive = start.date_naive().and_hms_opt(0, 0, 0).unwrap();
    local_timestamp(naive).unwrap_or(now.timestamp())
}

/// Epoch seconds for the start of the rolling month window (the subrepo's
/// "last 4 weeks" view): `today - 27` days advanced to `weekday`.
pub fn rolling_month_start(weekday: chrono::Weekday) -> Epoch {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let mut start = now - chrono::Duration::days(27);
    while start.weekday() != weekday {
        start += chrono::Duration::days(1);
    }
    let naive = start.date_naive().and_hms_opt(0, 0, 0).unwrap();
    local_timestamp(naive).unwrap_or(now.timestamp())
}

/// Epoch seconds for the start of the rolling year window (the subrepo's
/// year view): `today - 364` days walked back to `weekday`, so the window
/// opens on a full week (no leading blanks).
pub fn rolling_year_start(weekday: chrono::Weekday) -> Epoch {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let mut start = now - chrono::Duration::days(364);
    while start.weekday() != weekday {
        start -= chrono::Duration::days(1);
    }
    let naive = start.date_naive().and_hms_opt(0, 0, 0).unwrap();
    local_timestamp(naive).unwrap_or(now.timestamp())
}

/// Epoch seconds for the `weekday` on or before January 1 of the current year.
/// Used for year grids aligned to a full week start (so the grid never opens
/// with blank cells in the first column).
pub fn aligned_year_start(weekday: chrono::Weekday) -> Epoch {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let jan1 = chrono::Local
        .with_ymd_and_hms(now.year(), 1, 1, 0, 0, 0)
        .earliest()
        .unwrap_or(now);
    let mut start = jan1;
    while start.weekday() != weekday {
        start -= chrono::Duration::days(1);
    }
    let naive = start.date_naive().and_hms_opt(0, 0, 0).unwrap();
    local_timestamp(naive).unwrap_or(now.timestamp())
}

/// Epoch seconds for the first day of the current month (midnight).
pub fn month_start() -> Epoch {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let dt = chrono::Local
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .earliest()
        .unwrap_or(now);
    dt.timestamp()
}

/// Epoch seconds for the last day of the current month (23:59:59).
pub fn month_end() -> Epoch {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let dt = if now.month() == 12 {
        chrono::Local
            .with_ymd_and_hms(now.year() + 1, 1, 1, 0, 0, 0)
            .earliest()
            .unwrap_or(now)
    } else {
        chrono::Local
            .with_ymd_and_hms(now.year(), now.month() + 1, 1, 0, 0, 0)
            .earliest()
            .unwrap_or(now)
    };
    (dt - chrono::Duration::seconds(1)).timestamp()
}

/// Epoch seconds for January 1 of the current year (midnight).
pub fn year_start() -> Epoch {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let dt = chrono::Local
        .with_ymd_and_hms(now.year(), 1, 1, 0, 0, 0)
        .earliest()
        .unwrap_or(now);
    dt.timestamp()
}

/// Epoch seconds for December 31 of the current year (23:59:59).
pub fn year_end() -> Epoch {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let dt = chrono::Local
        .with_ymd_and_hms(now.year() + 1, 1, 1, 0, 0, 0)
        .earliest()
        .unwrap_or(now);
    (dt - chrono::Duration::seconds(1)).timestamp()
}

/// Get the epoch seconds for start of the day containing the given timestamp.
pub fn day_start(ts: Epoch) -> Epoch {
    use chrono::TimeZone;
    let dt = chrono::Local
        .timestamp_opt(ts, 0)
        .earliest()
        .unwrap_or_else(chrono::Local::now);
    let naive = dt.date_naive().and_hms_opt(0, 0, 0).unwrap();
    local_timestamp(naive).unwrap_or(ts)
}

/// Get the epoch seconds for end of the day (23:59:59) containing the given timestamp.
pub fn day_end(ts: Epoch) -> Epoch {
    use chrono::TimeZone;
    let dt = chrono::Local
        .timestamp_opt(ts, 0)
        .earliest()
        .unwrap_or_else(chrono::Local::now);
    let naive = dt.date_naive().and_hms_opt(23, 59, 59).unwrap();
    local_timestamp(naive).unwrap_or(ts)
}

// ── internal helpers ─────────────────────────────────────────────────

use chrono::TimeZone as _;

fn local_timestamp(naive: chrono::NaiveDateTime) -> Option<Epoch> {
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.timestamp())
}

// ── tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_now_nonzero() {
        assert!(now() > 1_700_000_000);
    }

    #[test]
    fn test_today_bounds() {
        let start = today_start();
        let end = today_end();
        assert!(start <= end);
        assert_eq!(end - start, 86399);
    }
}
