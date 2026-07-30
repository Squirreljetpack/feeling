//! Human-readable duration parsing.
//!
//! Returns seconds (i64) directly — callers never need to touch std::time::Duration.

use anyhow::{Context, Result};

/// Parse a human-readable duration (e.g. "1 day", "2 hours") to seconds.
pub fn parse_duration_secs(s: &str) -> Result<i64> {
    let dur = humantime::parse_duration(s)
        .with_context(|| format!("Failed to parse duration: '{}'", s))?;
    Ok(dur.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_secs() {
        assert_eq!(parse_duration_secs("1 day").unwrap(), 86400);
        assert_eq!(parse_duration_secs("2 hours").unwrap(), 7200);
        assert_eq!(parse_duration_secs("30 minutes").unwrap(), 1800);
        assert_eq!(parse_duration_secs("1d").unwrap(), 86400);
        assert_eq!(parse_duration_secs("2h").unwrap(), 7200);
    }
}
