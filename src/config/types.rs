use cba::define_collection_wrapper;
use crossterm::style::Color;
use serde::{Deserialize, Serialize};

/// One mood anchor: a mood word or phrase and the color it should produce.
/// Colors accept `#RRGGBB` hex, `rgb_(r,g,b)`, or named crossterm colors.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MoodEndpoint {
    pub mood: String,
    pub color: Color,
}

/// The moods file (`[moods] source`) — one `[[pairs]]` entry per mood
/// anchor, mapping a mood word (or phrase) to the color it should produce.
///
/// The bundled `assets/moods.toml` (release) / `assets/moods.dev.toml`
/// (debug) is the default: `Default` deserializes it at runtime, replacing
/// the old build-time `default_pairs()` codegen (see `build.rs` history).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MoodsFile {
    /// The anchor moods: one entry per mood.
    pub pairs: Vec<MoodEndpoint>,
}

impl Default for MoodsFile {
    fn default() -> Self {
        toml::from_str(crate::config::DEFAULT_MOODS)
            .expect("bundled assets/moods.toml must parse into MoodsFile")
    }
}

define_collection_wrapper!(
  /// A list of colors, e.g. the completion-badge bins in `[tasks] colors`
  /// (`colors = ["dark_red", "dark_yellow", "dark_green"]`).
  #[derive(Debug, Clone, Serialize, Deserialize)]
  #[serde(transparent)]
  ColorBins : Vec<Color>
);

impl Default for ColorBins {
    fn default() -> Self {
        vec![Color::DarkRed, Color::DarkYellow, Color::DarkGreen].into()
    }
}

/// Payload type for a custom tracker entry.
///
/// `Text` stores a string (e.g. `-accomplishment "fixed 2 bugs"`), `Number` an
/// integer, `Float` a decimal. min/max apply to `Number` and `Float`; they are
/// ignored for `Text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
pub enum TrackerKind {
    #[default]
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "number")]
    Number,
    #[serde(rename = "float")]
    Float,
}
