//! Date and datetime string parsing.
//!
//! Returns [`crate::date::Epoch`] (Unix seconds) directly — callers never
//! need to touch chrono types.

use anyhow::{Context, Result};
use chrono_english::{parse_date_string, Dialect};

use crate::date::Epoch;

/// Parse a datetime string (dates, datetimes, and relative forms like
/// "yesterday" / "tomorrow 9am" / "3 days ago") to epoch seconds.
///
/// This is the single shared parsing method for every place that requests a
/// timestamp from user input (oneshot `@<time>` start time), so all of them
/// accept the same formats.
///
/// The dialect is the fixed [`crate::date::DATE_DIALECT`] constant; it only
/// matters for ambiguous slash forms like `3/5/2024` (UK: 5 March, US:
/// March 5) — ISO dates and relative phrases ("yesterday", "3 days ago")
/// parse identically under both.
pub fn parse_datetime(s: &str, dialect: Dialect) -> Result<Epoch> {
    let dt = parse_date_string(s, chrono::Local::now(), dialect)
        .with_context(|| format!("Failed to parse datetime: '{}'", s))?;
    Ok(dt.timestamp())
}

/// Parse a date string and align to the start of that day (for the
/// `feeling @<date>` today view). Defers to [`parse_datetime`] for now.
pub fn parse_date(s: &str, dialect: Dialect) -> Result<Epoch> {
    Ok(crate::date::day_start(parse_datetime(s, dialect)?))
}

/// Parse a date string and align to the end of that day if a time is not specified.
pub fn parse_datetime_end(s: &str, dialect: Dialect) -> Result<Epoch> {
    Ok(crate::date::day_end(parse_datetime(s, dialect)?))
}

#[cfg(test)]
mod tests {
    use crate::date::format;

    use super::*;

    #[test]
    fn test_parse_datetime() {
        let ts = parse_datetime("2024-03-15", crate::date::DATE_DIALECT).unwrap();
        let formatted = format::format_datetime(ts);
        assert!(formatted.starts_with("2024-03-15"), "got {}", formatted);
    }

    #[test]
    fn test_parse_date_aligns_to_day_start() {
        // A datetime mid-day aligns to that day's start (the @<date>
        // today-view anchor).
        let ts = parse_date("2024-03-15 14:30", crate::date::DATE_DIALECT).unwrap();
        assert_eq!(ts, crate::date::day_start(ts));
        assert_eq!(format::format_datetime(ts), "2024-03-15 00:00");

        // A bare date is already day-aligned; garbage still fails.
        assert!(parse_date("bogus", crate::date::DATE_DIALECT).is_err());
    }

    #[test]
    fn test_parse_datetime_english() {
        assert!(parse_datetime("2024-03-15 14:30:00", crate::date::DATE_DIALECT).is_ok());
        assert!(parse_datetime("yesterday", crate::date::DATE_DIALECT).is_ok());
        assert!(parse_datetime("tomorrow 9am", crate::date::DATE_DIALECT).is_ok());
        assert!(parse_datetime("3 days ago", crate::date::DATE_DIALECT).is_ok());
        assert!(parse_datetime("invalid date text 12345", crate::date::DATE_DIALECT).is_err());
    }

    #[test]
    fn test_parse_datetime_dialects_agree_on_unambiguous_forms() {
        // Same instant under both dialects for unambiguous forms.
        let uk = parse_datetime("2024-03-15 14:30:00", Dialect::Uk).unwrap();
        let us = parse_datetime("2024-03-15 14:30:00", Dialect::Us).unwrap();
        assert_eq!(uk, us);
    }
}
