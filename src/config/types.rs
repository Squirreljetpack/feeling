use cba::define_collection_wrapper;
use crossterm::style::Color;
use serde::{Deserialize, Serialize};

/// A mood endpoint: a label string (used as the embedding prompt) and a
/// mood color. Color accepts hex (`#RRGGBB`),
/// `rgb_(r,g,b)`, or named crossterm colors via the crossterm Color serde
/// feature.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MoodEndpoint {
    pub mood: String,
    pub color: Color,
}

// The default pairs are compiled in from the bundled config's `[[moods.pairs]]`
// section by `build.rs`; see the generated `default_pairs` fn's doc comment.
include!(concat!(env!("OUT_DIR"), "/default_pairs.rs"));

define_collection_wrapper!(
  /// A set of nucleo `u32` indices representing the items the user has selected.
  ///
  /// The index is the nucleo item index (the value stored in [`nucleo::Match::idx`])
  /// and is stable for the lifetime of the worker's items. It is used as the row-cache
  /// key in `ResultsUI` so that selected rows can be highlighted.
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
pub enum TrackerType {
    #[default]
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "number")]
    Number,
    #[serde(rename = "float")]
    Float,
}
