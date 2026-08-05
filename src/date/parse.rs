//! Date and datetime string parsing.
//!
//! Returns [`crate::date::Epoch`] (Unix seconds) directly — callers never
//! need to touch chrono types.

use anyhow::{Context, Result};
use chrono_english::{parse_date_string, Dialect};
use serde::{Deserialize, Serialize};

use crate::date::Epoch;

/// User-facing selector for the [`chrono_english::Dialect`] used by
/// [`parse_datetime`]; configured via `[date] dialect` in config.toml.
///
/// Only matters for ambiguous slash forms like `3/5/2024` (UK: 5 March,
/// US: March 5); ISO dates and relative phrases ("yesterday", "3 days
/// ago") parse identically under both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DateDialect {
    /// Day-first: `3/5/2024` is 5 March.
    #[default]
    Uk,
    /// Month-first: `3/5/2024` is March 5.
    Us,
}

impl From<DateDialect> for Dialect {
    fn from(d: DateDialect) -> Self {
        match d {
            DateDialect::Uk => Dialect::Uk,
            DateDialect::Us => Dialect::Us,
        }
    }
}

/// Parse a datetime string (dates, datetimes, and relative forms like
/// "yesterday" / "tomorrow 9am" / "3 days ago") to epoch seconds.
///
/// This is the single shared parsing method for every place that requests a
/// timestamp from user input (oneshot `@<time>` start time), so all of them
/// accept the same formats.
pub fn parse_datetime(s: &str, dialect: DateDialect) -> Result<Epoch> {
    let dt = parse_date_string(s, chrono::Local::now(), dialect.into())
        .with_context(|| format!("Failed to parse datetime: '{}'", s))?;
    Ok(dt.timestamp())
}

/// Parse a date string and align to the start of that day (for the
/// `feeling @<date>` today view). Defers to [`parse_datetime`] for now.
pub fn parse_date(s: &str, dialect: DateDialect) -> Result<Epoch> {
    Ok(crate::date::day_start(parse_datetime(s, dialect)?))
}

/// Parse a date string and align to the end of that day if a time is not specified.
pub fn parse_datetime_end(s: &str, dialect: DateDialect) -> Result<Epoch> {
    Ok(crate::date::day_end(parse_datetime(s, dialect)?))
}

#[cfg(test)]
mod tests {
    use crate::date::format;

    use super::*;

    #[test]
    fn test_parse_datetime() {
        let ts = parse_datetime("2024-03-15", DateDialect::Uk).unwrap();
        let formatted = format::format_date_time(ts);
        assert!(formatted.starts_with("2024-03-15"), "got {}", formatted);
    }

    #[test]
    fn test_parse_date_aligns_to_day_start() {
        // A datetime mid-day aligns to that day's start (the @<date>
        // today-view anchor).
        let ts = parse_date("2024-03-15 14:30", DateDialect::Uk).unwrap();
        assert_eq!(ts, crate::date::day_start(ts));
        assert_eq!(format::format_date_time(ts), "2024-03-15 00:00");

        // A bare date is already day-aligned; garbage still fails.
        assert!(parse_date("bogus", DateDialect::Uk).is_err());
    }

    #[test]
    fn test_parse_datetime_english() {
        assert!(parse_datetime("2024-03-15 14:30:00", DateDialect::Uk).is_ok());
        assert!(parse_datetime("yesterday", DateDialect::Uk).is_ok());
        assert!(parse_datetime("tomorrow 9am", DateDialect::Uk).is_ok());
        assert!(parse_datetime("3 days ago", DateDialect::Uk).is_ok());
        assert!(parse_datetime("invalid date text 12345", DateDialect::Uk).is_err());
    }

    #[test]
    fn test_parse_datetime_uk_vs_us() {
        // Same instant under both dialects for unambiguous forms.
        let uk = parse_datetime("2024-03-15 14:30:00", DateDialect::Uk).unwrap();
        let us = parse_datetime("2024-03-15 14:30:00", DateDialect::Us).unwrap();
        assert_eq!(uk, us);
    }
}
