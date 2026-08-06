use serde::{Deserialize, Serialize};

use super::types::{ColorBins, TrackerKind};

/// `[tracker.<name>]` section — a user-defined tracker. The table key is the
/// tracker's name, used as `-<name> <value>` when logging an entry (e.g.
/// `-sleep 8` for a tracker named `sleep`).
#[derive(Debug, Clone, Deserialize, Default, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TrackerSetting {
    /// How often the tracker is expected to be logged, e.g. `"1 day"` or
    /// `"1 week"`. With an interval, re-logging the same tracker within the
    /// same period replaces the previous entry; without one, every log adds
    /// a new entry. Must be positive; a non-positive value is cleared to
    /// `None` at `Config::init`.
    #[serde(
        default,
        deserialize_with = "crate::date::deserialize::deserialize_duration"
    )]
    pub interval: Option<i64>,
    /// What kind of value the tracker stores: `text`, `number`, or `float`.
    pub kind: TrackerKind,
    /// Upper bound for the tracker's values, used to pick the entry's color
    /// in tracker grids (`number`/`float` trackers only).
    pub max: Option<f64>,
    /// Lower bound for the tracker's values, used to pick the entry's color
    /// in tracker grids (`number`/`float` trackers only).
    pub min: Option<f64>,
    /// Override color palette for this tracker's binning in grid/today views.
    /// When `Some`, takes precedence over `config.tasks.colors`.
    /// Must have more than 2 entries; otherwise cleared to `None` at init.
    pub colors: Option<ColorBins>,
}

impl TrackerSetting {
    /// Create a tracker setting for the given value kind; all optional
    /// fields (`interval`, `min`/`max`, `colors`) default to `None`.
    pub fn new(kind: TrackerKind) -> Self {
        Self {
            interval: None,
            kind,
            max: None,
            min: None,
            colors: None,
        }
    }

    /// Set the expected logging interval, e.g. `86_400` for `"1 day"`.
    pub fn with_interval(mut self, interval: i64) -> Self {
        self.interval = Some(interval);
        self
    }

    /// Set the upper bound for values (`number`/`float` trackers only).
    pub fn with_max(mut self, max: f64) -> Self {
        self.max = Some(max);
        self
    }

    /// Set the lower bound for values (`number`/`float` trackers only).
    pub fn with_min(mut self, min: f64) -> Self {
        self.min = Some(min);
        self
    }

    /// Override the color palette for grid/today binning.
    pub fn with_colors(mut self, colors: ColorBins) -> Self {
        self.colors = Some(colors);
        self
    }
}
