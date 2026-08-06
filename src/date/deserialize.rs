//! Serde helpers for the `Epoch` (i64) duration type used in tracker
//! config.  Durations are stored as human-readable strings in TOML (e.g.
//! `"1 day"`, `"2 hours"`, `"1 week"`) and deserialized to seconds via
//! [`crate::date::parse_duration_secs`].  The reverse direction uses
//! [`crate::date::format_duration`] to turn seconds back into a readable
//! string.

use serde::{Deserialize, Deserializer, Serializer};

use crate::date::{format_duration, parse_duration_secs};

/// Deserialize a human-readable duration string (e.g. `"1 day"`, `"2h"`)
/// into an `i64` representing seconds.  Returns `None` for absent values.
pub fn deserialize_duration<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(deserializer)?;
    match s {
        Some(ref str) => parse_duration_secs(str)
            .map(Some)
            .map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

/// Serialize an `i64` duration (seconds) back into a human-readable string
/// (e.g. `"1 day"`, `"2h"`).  Returns an empty string for `None`.
pub fn serialize_duration<S>(secs: &Option<i64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match secs {
        Some(s) => serializer.serialize_str(&format_duration(*s)),
        None => serializer.serialize_none(),
    }
}
