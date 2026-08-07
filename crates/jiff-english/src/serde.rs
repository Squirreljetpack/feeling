//! Serde support for `jiff-english` types.
//!
//! [`jiff::civil::Weekday`] has no serde impl, so this module carries a
//! serde-friendly [`Weekday`] wrapper: it serializes as `"Monday"` …
//! `"Sunday"` (PascalCase) and deserializes case-insensitively. Convert to
//! the jiff type with `jiff::civil::Weekday::from`.

use serde::{Deserialize, Serialize};

/// The day each week starts on, as configured in a `[grid]` `week_start`
/// key (`"Monday"` … `"Sunday"`, case-insensitive on parse). A serde
/// wrapper around jiff's `Weekday`, which has no serde impl; convert at use
/// sites with `jiff::civil::Weekday::from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl<'de> Deserialize<'de> for Weekday {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.to_ascii_lowercase().as_str() {
            "monday" => Ok(Weekday::Monday),
            "tuesday" => Ok(Weekday::Tuesday),
            "wednesday" => Ok(Weekday::Wednesday),
            "thursday" => Ok(Weekday::Thursday),
            "friday" => Ok(Weekday::Friday),
            "saturday" => Ok(Weekday::Saturday),
            "sunday" => Ok(Weekday::Sunday),
            other => Err(serde::de::Error::custom(format!(
                "unknown weekday '{}' (expected Monday..Sunday)",
                other
            ))),
        }
    }
}

impl From<Weekday> for jiff::civil::Weekday {
    fn from(w: Weekday) -> Self {
        match w {
            Weekday::Monday => jiff::civil::Weekday::Monday,
            Weekday::Tuesday => jiff::civil::Weekday::Tuesday,
            Weekday::Wednesday => jiff::civil::Weekday::Wednesday,
            Weekday::Thursday => jiff::civil::Weekday::Thursday,
            Weekday::Friday => jiff::civil::Weekday::Friday,
            Weekday::Saturday => jiff::civil::Weekday::Saturday,
            Weekday::Sunday => jiff::civil::Weekday::Sunday,
        }
    }
}
