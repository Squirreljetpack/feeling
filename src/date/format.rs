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

/// Two-letter local weekday abbreviation for an epoch ("Mo".."Su").
pub fn format_weekday(ts: Epoch) -> String {
    use chrono::{Datelike, TimeZone};
    chrono::Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map(|dt| {
            match dt.weekday() {
                chrono::Weekday::Mon => "Mo",
                chrono::Weekday::Tue => "Tu",
                chrono::Weekday::Wed => "We",
                chrono::Weekday::Thu => "Th",
                chrono::Weekday::Fri => "Fr",
                chrono::Weekday::Sat => "Sa",
                chrono::Weekday::Sun => "Su",
            }
            .to_string()
        })
        .unwrap_or_default()
}

/// Format an epoch timestamp as `DD-MM-YY`.
pub fn format_date(ts: Epoch) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map(|dt| dt.format("%d-%m-%y").to_string())
        .unwrap_or_else(|| "--".to_string())
}

/// Format an epoch timestamp as `YYYY-MM-DD HH:MM`.
pub fn format_datetime(ts: Epoch) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "--".to_string())
}

/// Short datetime form for per-entry annotations (e.g. the text-tracker
/// `> value [timestamp]` lines); M-D HH:MM (hour/minute zero-padded)
pub fn format_datetime_short(ts: Epoch) -> String {
    use chrono::{Datelike, TimeZone, Timelike};

    chrono::Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map(|dt| {
            format!(
                "{}-{} {:02}:{:02}",
                dt.month(),
                dt.day(),
                dt.hour(),
                dt.minute()
            )
        })
        .unwrap_or_else(|| "--".to_string())
}

/// DD HH:MM
pub fn format_day_time(ts: Epoch) -> String {
    use chrono::TimeZone;

    chrono::Local
        .timestamp_opt(ts, 0)
        .earliest()
        .map(|dt| dt.format("%d %H:%M").to_string())
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
    fn test_format_datetime() {
        let ts = parse::parse_datetime("2024-03-15", crate::date::DATE_DIALECT).unwrap();
        let s = format_datetime(ts);
        assert!(s.starts_with("2024-03-15 00:00"), "got {}", s);
    }

    #[test]
    fn test_format_datetime_short() {
        let ts = parse::parse_datetime("2024-03-15 14:30", crate::date::DATE_DIALECT).unwrap();
        assert_eq!(format_datetime_short(ts), "3-15 14:30");
        // Hour/minute are zero-padded (9:05 renders as 09:05, not 9:5).
        let ts = parse::parse_datetime("2024-03-15 09:05", crate::date::DATE_DIALECT).unwrap();
        assert_eq!(format_datetime_short(ts), "3-15 09:05");
    }

    #[test]
    fn test_format_weekday() {
        // 2024-03-15 was a Friday.
        let ts = parse::parse_datetime("2024-03-15 12:00", crate::date::DATE_DIALECT).unwrap();
        assert_eq!(format_weekday(ts), "Fr");
    }
}
