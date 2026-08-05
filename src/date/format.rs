//! Formatting helpers for epoch timestamps and durations.

use crate::date::Epoch;

/// Format seconds as a human-readable duration string (e.g. "1 day", "2 hours").
pub fn format_duration(secs: i64) -> String {
    humantime::format_duration(std::time::Duration::from_secs(secs as u64)).to_string()
}

/// Format an epoch timestamp as `HH:MM`.
pub fn format_time(ts: Epoch) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map(|dt| dt.format("%H:%M").to_string())
        .unwrap_or_else(|| "--:--".to_string())
}

/// Format an epoch timestamp as `YYYY-MM-DD`.
pub fn format_date(ts: Epoch) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "--".to_string())
}

/// Format an epoch timestamp as `YYYY-MM-DD HH:MM`.
pub fn format_date_time(ts: Epoch) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "--".to_string())
}

/// Format an epoch timestamp as `DD-MM-YY` (e.g. `15-03-26`) — the TodayApp
/// title label for anchored days that are neither today nor yesterday.
pub fn format_date_dmy(ts: Epoch) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map(|dt| dt.format("%d-%m-%y").to_string())
        .unwrap_or_else(|| "--".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date::parse;

    #[test]
    fn test_format_duration_roundtrip() {
        let secs = 86400;
        let s = format_duration(secs);
        assert_eq!(s, "1day");
    }

    #[test]
    fn test_format_date_time() {
        let ts = parse::parse_datetime("2024-03-15", crate::date::DateDialect::Uk).unwrap();
        let s = format_date_time(ts);
        assert!(s.starts_with("2024-03-15 00:00"), "got {}", s);
    }

    #[test]
    fn test_format_date_dmy() {
        let ts = parse::parse_datetime("2024-03-15", crate::date::DateDialect::Uk).unwrap();
        assert_eq!(format_date_dmy(ts), "15-03-24");
    }
}
