use cba::wbog;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::clap::FLAG_CHARACTERS;
use crate::embed::Embedder;
use crate::utils::Percentage;

#[cfg(debug_assertions)]
pub const DEFAULT_CONFIG: &str = include_str!("../assets/dev.toml");
#[cfg(not(debug_assertions))]
pub const DEFAULT_CONFIG: &str = include_str!("../assets/config.toml");

mod types;
pub use types::*;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub moods: MoodConfig,

    #[serde(default)]
    pub tasks: TasksConfig,

    #[serde(default)]
    pub tracker: HashMap<String, TrackerSetting>,

    #[serde(default)]
    pub grid: GridViewConfig,

    #[serde(default)]
    pub tasks_view: TasksViewConfig,

    #[serde(default)]
    pub today_view: TodayViewConfig,

    #[serde(default)]
    pub date: DateConfig,

    #[serde(default)]
    pub editor: EditorConfig,
}

impl Default for Config {
    fn default() -> Self {
        toml::from_str(DEFAULT_CONFIG).expect("bundled assets/config.toml must parse into Config")
    }
}

impl Config {
    /// Normalize a loaded config:
    /// - Drop trackers whose names are unusable: begin with `:` (collides
    ///   with the grid-view `:` command), contain `-` or whitespace
    ///   (unaddressable — `-name value` splits on the dash/space), or
    ///   consist solely of letters from [`FLAG_CHARACTERS`] (`q`/`v` —
    ///   reserved for the leading `-q`/`-v` flags, so `-q` / `-v` / `-qv`
    ///   can never be a tracker token).
    /// - If `task_color.colors` is empty, fall back to the default palette
    ///   (DarkRed, DarkYellow, DarkGreen) so dot binning never panics on an
    ///   empty palette.
    pub fn init(&mut self) {
        self.tracker.retain(|name, _| {
            if !is_valid_tracker_name(name) {
                cba::ebog!(
                    "config";
                    "Dropping unusable tracker '{}': names cannot begin with ':', contain '-' or whitespace, or consist solely of flag characters '{}'",
                    name, FLAG_CHARACTERS
                );
                false
            } else {
                true
            }
        });
        if self.tasks.colors.len() < 3 {
            wbog!(
                "Less than 3 colors defined for config.tasks.colors, overriding with the default."
            );
            self.tasks.colors = Default::default();
        }
    }
}

/// Tracker-name validity for [`Config::init`]. A name is usable only when
/// it is non-empty, does not begin with `:` (grid-view command syntax),
/// contains no `-` or whitespace, and is not made purely of the leading
/// flag characters (`q` / `v`).
fn is_valid_tracker_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if name.starts_with(':') {
        return false;
    }
    if name.contains('-') || name.chars().any(char::is_whitespace) {
        return false;
    }
    !name.chars().all(|c| FLAG_CHARACTERS.contains(c))
}

/// `[grid]` section — tracker grid options (`:` / `:week` / `:month` / `:year`).
/// `[grid]` section — tracker grid options (`, :week`, `:month`, `:year`).
///
/// "Rolling" grids are anchored to today (a fixed-size window); non-rolling
/// grids start at the calendar period boundary (week_start / month start).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct GridViewConfig {
    /// true = last 7 days (including today).
    /// false = from week_start through today.
    #[serde(default)]
    pub week_rolling: bool,

    /// true = the rolling "last 4 weeks" window ending today.
    /// false = from the month start through today.
    pub month_rolling: bool,

    /// false = the calendar year (January 1 through
    /// today).
    /// true = the calendar year, aligned to a full week start (so the grid never opens with blank cells).
    pub year_rolling: bool,

    /// The day each week starts on for tracker grids. Defaults to Monday.
    pub week_start: chrono::Weekday,
}

impl Default for GridViewConfig {
    fn default() -> Self {
        Self {
            week_rolling: false,
            year_rolling: true,
            month_rolling: true,
            week_start: chrono::Weekday::Mon,
        }
    }
}

/// `[date]` section — date/time parsing options.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DateConfig {
    /// chrono-english dialect for natural-language date parsing: `"uk"`
    /// (day-first, default) or `"us"` (month-first). Affects ambiguous
    /// slash forms like `3/5/2024` only; ISO dates and relative phrases
    /// ("yesterday", "3 days ago") are unaffected.
    #[serde(default)]
    pub dialect: crate::date::DateDialect,
}

impl Default for DateConfig {
    fn default() -> Self {
        Self {
            dialect: crate::date::DateDialect::Uk,
        }
    }
}

/// `[tasks_view]` section — options for the task-list view (TUI tasks app).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TasksViewConfig {
    /// Start the TUI tasks app with scheduled tasks included in the `!`,
    /// `@`, `@done` and `@due` views (`Ctrl+a` toggles this live). Defaults
    /// to false.
    #[serde(default)]
    pub include_scheduled: bool,
}

/// `[editor]` section — options for the external body editor (`..`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct EditorConfig {
    /// Write the `# additional notes below` hint line as the first line of
    /// the body-editor temp file. When disabled the file starts empty and
    /// the first line the user types is kept verbatim (with the hint on,
    /// that first line is the hint and is stripped on read).
    #[serde(default = "EditorConfig::default_hint")]
    pub hint: bool,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self { hint: true }
    }
}

impl EditorConfig {
    fn default_hint() -> bool {
        true
    }
}

/// `[today_view]` section — options for the today view (`feeling -`).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TodayViewConfig {
    /// Include overdue oneshot tasks (due before today) in the today view.
    /// Defaults to false: only tasks due within the horizon are shown.
    #[serde(default)]
    pub include_overdue: bool,
    /// Badge glyph for journal-only entries (feeling rows with no mood);
    /// `None` renders no badge at all.
    #[serde(default)]
    pub journal_badge: Option<char>,
}

/// Settings consumed by [`crate::color::ColorAxes`] when building the mood
/// color pipeline. Flattened into [`MoodConfig`] so the `[moods]` TOML keys
/// stay exactly as they are.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct ColorAxesSettings {
    /// Text anchor prefixed to a mood before embedding ("person says: "), so
    /// the embedding encodes the mood as a statement.
    pub prefix_string: String,

    /// Text used as the neutral baseline anchor subtracted when computing basis ray shift vectors.
    pub base_string: String,

    /// Power exponent used for power-weighted centroid blending of basis mood colors.
    pub blend_steepness: f32,

    /// Maximum number of top NNLS weights to include in blending.
    pub top_k: usize,

    /// Minimum contribution percentage (0-100) required for a basis mood to be included in blending.
    pub min_contribution: Percentage,

    /// Gate on emotional saliency's control of the produced color's saturation/lightness.
    /// Effective saliency `Seff = 1 + P*(S - 1)` for predicted saliency S in [0, 1];
    pub effective_saliency_gate: Percentage,

    /// Neutral baseline Oklab lightness (0-100), default 65.
    pub baseline_oklab_l: Percentage,
}

impl Default for ColorAxesSettings {
    fn default() -> Self {
        Self {
            prefix_string: "person says: ".to_string(),
            base_string: "this person feels:".to_string(),
            blend_steepness: 2.0,
            top_k: 5,
            min_contribution: Percentage::new(7),
            effective_saliency_gate: Percentage::new(50),
            baseline_oklab_l: Percentage::new(65),
        }
    }
}

/// Config for sentence-embedding mood colors via NNLS regression & saliency scaling.
///
/// `color_axes` caches the built [`crate::color::ColorAxes`] struct (computed at init
/// via `init_with`, skipped by serde) so subsequent color projections skip MiniLM forward passes.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub struct MoodConfig {
    /// Settings consumed by the color axes (flattened — the `[moods]` TOML
    /// keys for these live directly on the table).
    #[serde(flatten)]
    pub axes: ColorAxesSettings,

    pub pairs: Vec<MoodEndpoint>,

    #[serde(skip)]
    pub color_axes: Option<crate::color::ColorAxes>,
}


impl MoodConfig {
    /// Embed each pair's mood using SQLite cache and store the built [`crate::color::ColorAxes`] struct.
    /// Idempotent: a second call is a no-op.
    pub async fn init_with(
        &mut self,
        pool: &sqlx::SqlitePool,
        embedder: &Embedder,
    ) -> anyhow::Result<()> {
        if self.color_axes.is_some() {
            return Ok(());
        }
        if self.pairs.is_empty() {
            self.pairs = default_pairs();
        }
        let axes = crate::color::ColorAxes::build_async(pool, embedder, &self.axes, &self.pairs).await?;
        self.color_axes = Some(axes);
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct TasksConfig {
    pub default_priority: i32,
    /// Default priority used when creating scheduled tasks without an
    /// explicit priority (immediate `! @<time>; …; @<duration>` creation
    /// and the interactive flow's prompt default).
    #[serde(default = "TasksConfig::default_scheduled_priority")]
    pub default_scheduled_priority: i32,
    pub colors: ColorBins,
}

impl TasksConfig {
    fn default_scheduled_priority() -> i32 {
        10
    }
}

impl Default for TasksConfig {
    fn default() -> Self {
        Self {
            default_priority: 5,
            default_scheduled_priority: TasksConfig::default_scheduled_priority(),
            colors: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default, Serialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct TrackerSetting {
    #[serde(
        default,
        deserialize_with = "crate::date::deserialize::deserialize_duration"
    )]
    pub interval: Option<i64>,
    /// Payload type; defaults to `text` when omitted in config.
    pub kind: TrackerType,
    pub max: Option<f64>,
    pub min: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracker_name_validation() {
        assert_eq!(FLAG_CHARACTERS, "qv");
        // Valid names
        assert!(is_valid_tracker_name("sleep"));
        assert!(is_valid_tracker_name("run_times"));
        assert!(is_valid_tracker_name("mood_notes"));
        assert!(is_valid_tracker_name("query")); // 'q' inside a longer name is fine
        assert!(is_valid_tracker_name("vibe"));
        // ':' prefix collides with grid-view commands
        assert!(!is_valid_tracker_name(":foo"));
        // '-' or whitespace can't be addressed as `-name value`
        assert!(!is_valid_tracker_name("sleep-time"));
        assert!(!is_valid_tracker_name("my sleep"));
        assert!(!is_valid_tracker_name("my\tsleep"));
        // Names made purely of flag characters (q / v) are reserved
        assert!(!is_valid_tracker_name("q"));
        assert!(!is_valid_tracker_name("v"));
        assert!(!is_valid_tracker_name("qv"));
        assert!(!is_valid_tracker_name("vvq"));
        assert!(!is_valid_tracker_name(""));
    }

    #[test]
    fn test_init_drops_invalid_trackers() {
        let mut config = Config::default(); // debug: assets/dev.toml trackers
        for bad in [":collide", "sleep-time", "two words", "q", "v", "qv", ""] {
            config
                .tracker
                .insert(bad.to_string(), TrackerSetting::default());
        }
        config.init();

        for bad in [":collide", "sleep-time", "two words", "q", "v", "qv", ""] {
            assert!(
                !config.tracker.contains_key(bad),
                "tracker {:?} should have been dropped",
                bad
            );
        }
        // The bundled dev.toml trackers survive untouched.
        for good in [
            "sleep",
            "run_times",
            "water",
            "notes",
            "steps",
            "mood_notes",
            "temperature",
        ] {
            assert!(
                config.tracker.contains_key(good),
                "tracker {:?} should survive init",
                good
            );
        }
    }

    #[test]
    fn test_editor_config_serde_defaults() {
        // Missing [editor] section → hint defaults to true (current behavior).
        let cfg: Config = toml::from_str("").expect("empty toml parses");
        assert!(cfg.editor.hint);

        // Empty [editor] section → hint still defaults to true.
        let cfg: Config = toml::from_str("[editor]\n").expect("empty editor table parses");
        assert!(cfg.editor.hint);

        // Explicit false is honored.
        let cfg: Config = toml::from_str("[editor]\nhint = false\n").expect("hint=false parses");
        assert!(!cfg.editor.hint);

        // Explicit true is honored.
        let cfg: Config = toml::from_str("[editor]\nhint = true\n").expect("hint=true parses");
        assert!(cfg.editor.hint);
    }

    #[test]
    fn test_moods_flatten_serde_roundtrip() {
        // [moods] with only `pairs` (all settings missing) → settings default.
        let cfg: Config = toml::from_str(
            "[moods]\n[[moods.pairs]]\nmood = \"happy\"\ncolor = \"#FF0000\"\n",
        )
        .expect("[moods] with only pairs parses");
        assert_eq!(cfg.moods.axes.prefix_string, "person says: ");
        assert_eq!(cfg.moods.axes.blend_steepness, 2.0);
        assert_eq!(cfg.moods.pairs.len(), 1);
        assert_eq!(cfg.moods.pairs[0].mood, "happy");

        // Explicit settings are honored through the flatten.
        let cfg: Config = toml::from_str(
            "[moods]\nblend_steepness = 3.5\ntop_k = 8\n[[moods.pairs]]\nmood = \"sad\"\ncolor = \"blue\"\n",
        )
        .expect("[moods] with settings parses");
        assert_eq!(cfg.moods.axes.blend_steepness, 3.5);
        assert_eq!(cfg.moods.axes.top_k, 8);
        assert_eq!(cfg.moods.pairs.len(), 1);
        assert_eq!(cfg.moods.pairs[0].mood, "sad");

        // Unknown keys under [moods] are rejected (deny_unknown_fields holds
        // through the flattened ColorAxesSettings).
        assert!(
            toml::from_str::<Config>("[moods]\nbogus_key = 1\n").is_err(),
            "unknown [moods] key must be rejected"
        );

        // Full round-trip: serialize then re-parse keeps pairs + settings.
        let serialized = toml::to_string(&cfg).expect("serializes");
        let reparsed: Config = toml::from_str(&serialized).expect("re-parses");
        assert_eq!(reparsed.moods.axes.blend_steepness, 3.5);
        assert_eq!(reparsed.moods.pairs.len(), 1);
        assert_eq!(reparsed.moods.pairs[0].color, cfg.moods.pairs[0].color);
    }
}
