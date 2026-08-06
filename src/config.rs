#![allow(clippy::derivable_impls)]

use cba::wbog;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::clap::FLAG_CHARACTERS;
use crate::embed::Embedder;
use crate::utils::Percentage;

#[cfg(debug_assertions)]
pub const DEFAULT_CONFIG: &str = include_str!("../assets/dev.toml");
#[cfg(not(debug_assertions))]
pub const DEFAULT_CONFIG: &str = include_str!("../assets/config.toml");

#[cfg(debug_assertions)]
pub const DEFAULT_MOODS: &str = include_str!("../assets/moods.dev.toml");
#[cfg(not(debug_assertions))]
pub const DEFAULT_MOODS: &str = include_str!("../assets/moods.toml");

mod types;
pub use types::*;

/// The whole configuration file (`config.toml`). Every section is optional
/// — a missing section or key falls back to a built-in default, so a config
/// can be as small as a single `[tracker.sleep]` block.
///
/// Sections: `[moods]` (color settings; the anchor pairs live in the file
/// named by `[moods] source`), `[tasks]` (defaults for new tasks, badge
/// colors), `[tracker.<name>]` (custom trackers), `[grid]` (tracker grid
/// ranges), `[tasks_view]` and `[today_view]` (view options), `[date]` (date
/// parsing dialect), `[editor]` (body editor).
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
    pub preview: PreviewConfig,

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
    /// Normalize a loaded config before use: drop custom trackers whose
    /// names cannot be addressed from the CLI, and fall back to the default
    /// badge palette when fewer than three colors are configured. Run
    /// automatically at startup, before any command is handled.
    ///
    /// The bundled default config never needs this; a user-edited config
    /// may. See [`is_valid_tracker_name`] for the exact tracker-name rules.
    pub fn init(&mut self) {
        // Drop trackers whose names are unusable: a `:` prefix collides with
        // the grid-view `:` command, `-`/whitespace can't be addressed as
        // `-name value`, and names made purely of the flag characters
        // (`q`/`v`) would be swallowed by the leading `-q`/`-v` flags.
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
        // Validate tracker-level color overrides: a palette with 2 or fewer
        // entries can't produce meaningful binning, so clear it and warn.
        for (name, setting) in self.tracker.iter_mut() {
            if let Some(ref colors) = setting.colors {
                if colors.len() <= 2 {
                    wbog!(
                        "config";
                        "Tracker '{}' has colors with {} entries (<= 2), clearing the override",
                        name,
                        colors.len()
                    );
                    setting.colors = None;
                }
            }
        }
        if self.tasks.colors.len() < 3 {
            wbog!(
                "Less than 3 colors defined for config.tasks.colors, overriding with the default."
            );
            self.tasks.colors = Default::default();
        }
    }
}

// Tracker-name validity for `Config::init`. A name is usable only when it
// is non-empty, does not begin with `:` (grid-view command syntax), contains
// no `-` or whitespace, and is not made purely of the leading flag
// characters (`q` / `v`).
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

/// `[grid]` section — how far back the tracker grids (`:`, `:week`, `:month`,
/// `:year`) reach, and which day each week starts on.
///
/// Each period has two modes. "Rolling" grids always end today and keep a
/// fixed number of cells, so today is always the last one. Calendar grids
/// run from the period's boundary through today, so they grow as the period
/// passes.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct GridViewConfig {
    /// `true`: the last 7 days, always ending today (7 cells).
    /// `false`: the current calendar week, from `week_start` through today.
    #[serde(default)]
    pub week_rolling: bool,

    /// `true`: the last 4 weeks, ending today.
    /// `false`: the current calendar month, from its first day through today.
    pub month_rolling: bool,

    /// `true`: the calendar year, aligned back to the nearest `week_start`
    /// before January 1 so the grid never opens with blank cells.
    /// `false`: the calendar year from January 1 through today.
    pub year_rolling: bool,

    /// The day each week starts on for the grids, and the alignment day for
    /// the rolling month and year windows.
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
    /// (day-first) or `"us"` (month-first). Affects ambiguous
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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PreviewConfig {
    pub show_last_when_done: bool,
}
impl Default for PreviewConfig {
    fn default() -> Self {
        Self {
            show_last_when_done: false,
        }
    }
}

/// `[tasks_view]` section — options for the task-list view (TUI tasks app).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TasksViewConfig {
    /// Keep a task visible in the pending view within this many seconds of
    /// its last completion entry, so a just-completed task doesn't vanish
    /// from the tui.
    pub persist_pending_seconds: i64,
}

impl Default for TasksViewConfig {
    fn default() -> Self {
        Self {
            persist_pending_seconds: 5 * 60,
        }
    }
}

/// `[editor]` section — options for the external body editor (`..`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct EditorConfig {
    /// When `true`, the body editor opens with a
    /// `# additional notes below` hint line; type below it and the hint is
    /// stripped when the file is saved. When `false`, the file starts empty
    /// and the first line you type is kept verbatim.
    pub hint: bool,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self { hint: true }
    }
}

/// `[today_view]` section — options for the today view (bare `feeling`,
/// `feeling @<date>`, and the today TUI).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TodayViewConfig {
    /// Include overdue oneshot tasks (due before today) in the today view;
    /// when false, only tasks due within the horizon are shown.
    #[serde(default)]
    pub include_overdue: bool,
    /// Glyph shown next to journal-only entries (a feeling with no mood
    /// word). Omit the key to show no badge.
    #[serde(default)]
    pub journal_badge: Option<char>,
    /// Merge a task's adjacent completion entries into a single "done" row
    /// in the today view (currently accepted and stored on TodayApp; no
    /// behavior yet).
    #[serde(default)]
    pub coalesce_completions: bool,
}

/// `[moods]` color settings — how mood words are turned into colors from
/// the anchors in the moods file (`[moods] source`). These keys live
/// directly on the `[moods]` table (they are flattened into [`MoodConfig`]).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct ColorAxesSettings {
    /// A short phrase prepended to every anchor mood before it is converted
    /// to a color, so the anchors read as statements about a person. Keep it
    /// in sync with `base_string`.
    pub prefix_string: String,

    /// A neutral phrase standing for "no particular mood"; anchor colors are
    /// measured from this baseline, so moods far from it produce more vivid
    /// colors.
    pub base_string: String,

    /// How decisively the strongest anchor mood wins the final color:
    /// `1.0` mixes the contributing moods evenly, higher values let the
    /// strongest mood's color dominate.
    pub blend_steepness: f32,

    /// The maximum number of anchor moods that may contribute to a single
    /// color, strongest first.
    pub top_k: usize,

    /// An anchor mood must make up at least this percentage of the color
    /// mix to be included at all.
    pub min_contribution: Percentage,

    /// How much emotional intensity (saliency) moves a color away from
    /// neutral: `0` disables it entirely, `100` keeps the full effect.
    pub effective_saliency_gate: Percentage,

    /// The lightness of the neutral color used when no anchor mood matches
    /// (0–100).
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

/// `[moods]` section — the color settings that derive every mood's color
/// from the anchor pairs, plus `source`, the path of the moods file
/// holding those anchors.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct MoodConfig {
    /// The color settings — the `[moods]` keys other than `source`
    /// (flattened, so they live directly on the table).
    #[serde(flatten)]
    pub axes: ColorAxesSettings,

    /// Path of the moods file holding the `[[pairs]]` anchors, relative to
    /// the config directory. Empty (the default) uses the bundled moods
    /// file; a missing or unparsable file falls back to it as well.
    #[serde(default)]
    pub source: PathBuf,

    // The built color model. Computed once per run by `init_with`; never
    // serialized (marked `skip`).
    #[serde(skip)]
    pub color_axes: Option<crate::color::ColorAxes>,
}

impl MoodConfig {
    /// Build the color model from the configured anchors. Run automatically
    /// before any color-producing command (entry logging, today view,
    /// trackers); calling it again later is a no-op.
    pub async fn init_with(
        &mut self,
        pool: &sqlx::SqlitePool,
        embedder: &Embedder,
    ) -> anyhow::Result<()> {
        if self.color_axes.is_some() {
            return Ok(());
        }
        let pairs = self.load_pairs();
        let axes = crate::color::ColorAxes::build_async(pool, embedder, &self.axes, &pairs).await?;
        self.color_axes = Some(axes);
        Ok(())
    }

    /// Resolve the anchor pairs. An empty `source` skips deserialization
    /// and uses the bundled default directly. Otherwise the `source` file
    /// (relative to the config directory) is deserialized, falling back to
    /// the bundled default when it can't be read or parsed, or when it
    /// yields no pairs (the same load-or-default pattern as the config
    /// itself, see `cba::bo::load_type_or_default`).
    fn load_pairs(&self) -> Vec<MoodEndpoint> {
        if self.source.as_os_str().is_empty() {
            return MoodsFile::default().pairs;
        }
        let path = crate::paths::config_dir().join(&self.source);
        let file = cba::bo::load_type_or_default(path, |s| toml::from_str::<MoodsFile>(s));
        if file.pairs.is_empty() {
            return MoodsFile::default().pairs;
        }
        file.pairs
    }
}

/// `[tasks]` section — defaults for new tasks and the completion-badge
/// colors.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TasksConfig {
    /// Default priority (1–999) for new oneshot tasks.
    pub default_priority: i32,
    /// Default priority for new recurring tasks.
    pub default_recurring_priority: i32,
    /// Default priority for new scheduled tasks.
    pub default_scheduled_priority: i32,
    /// Colors for the completion badge (`◯`/`●`) shown in task lists, from
    /// lowest to highest progress.
    pub colors: ColorBins,
}

impl Default for TasksConfig {
    fn default() -> Self {
        Self {
            default_priority: 10,
            default_recurring_priority: 5,
            default_scheduled_priority: 15,
            colors: Default::default(),
        }
    }
}

/// `[tracker.<name>]` section — a custom tracker. The table key is the
/// tracker's name, used as `-<name> <value>` when logging an entry (e.g.
/// `-sleep 8` for a tracker named `sleep`).
#[derive(Debug, Clone, Deserialize, Default, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TrackerSetting {
    /// How often the tracker is expected to be logged, e.g. `"1 day"` or
    /// `"1 week"`. With an interval, re-logging the same tracker within the
    /// same period replaces the previous entry; without one, every log adds
    /// a new entry.
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
    fn test_init_clears_invalid_tracker_colors() {
        let mut config = Config::default();

        // colors with 1 entry (<= 2) should be cleared and a warning emitted
        config.tracker.insert(
            "bad_colors".to_string(),
            TrackerSetting {
                colors: Some(ColorBins::from(vec![crossterm::style::Color::DarkRed])),
                ..Default::default()
            },
        );
        // colors with exactly 2 entries should also be cleared
        config.tracker.insert(
            "bad_colors2".to_string(),
            TrackerSetting {
                colors: Some(ColorBins::from(vec![
                    crossterm::style::Color::DarkRed,
                    crossterm::style::Color::DarkGreen,
                ])),
                ..Default::default()
            },
        );
        // colors with 3+ entries should be kept
        config.tracker.insert(
            "good_colors".to_string(),
            TrackerSetting {
                colors: Some(ColorBins::from(vec![
                    crossterm::style::Color::DarkRed,
                    crossterm::style::Color::DarkYellow,
                    crossterm::style::Color::DarkGreen,
                ])),
                ..Default::default()
            },
        );
        // None colors should be left as None
        config
            .tracker
            .insert("no_colors".to_string(), TrackerSetting::default());

        config.init();

        assert!(config.tracker["bad_colors"].colors.is_none());
        assert!(config.tracker["bad_colors2"].colors.is_none());
        assert!(config.tracker["good_colors"].colors.is_some());
        assert_eq!(
            config.tracker["good_colors"].colors.as_ref().unwrap().len(),
            3
        );
        assert!(config.tracker["no_colors"].colors.is_none());
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
    fn test_moods_source_serde_roundtrip() {
        // [moods] with only `source` (all settings missing) → settings default.
        let cfg: Config = toml::from_str("[moods]\nsource = \"moods.toml\"\n")
            .expect("[moods] with only source parses");
        assert_eq!(cfg.moods.axes.prefix_string, "person says: ");
        assert_eq!(cfg.moods.axes.blend_steepness, 2.0);
        assert_eq!(cfg.moods.source, PathBuf::from("moods.toml"));

        // Explicit settings are honored through the flatten.
        let cfg: Config = toml::from_str(
            "[moods]\nblend_steepness = 3.5\ntop_k = 8\nsource = \"my-moods.toml\"\n",
        )
        .expect("[moods] with settings parses");
        assert_eq!(cfg.moods.axes.blend_steepness, 3.5);
        assert_eq!(cfg.moods.axes.top_k, 8);
        assert_eq!(cfg.moods.source, PathBuf::from("my-moods.toml"));

        // A missing `source` key defaults to the empty path.
        let empty: Config = toml::from_str("").expect("empty toml parses");
        assert!(empty.moods.source.as_os_str().is_empty());

        // Unknown keys under [moods] are rejected (deny_unknown_fields holds
        // through the flattened ColorAxesSettings).
        assert!(
            toml::from_str::<Config>("[moods]\nbogus_key = 1\n").is_err(),
            "unknown [moods] key must be rejected"
        );

        // Full round-trip: serialize then re-parse keeps source + settings.
        let serialized = toml::to_string(&cfg).expect("serializes");
        let reparsed: Config = toml::from_str(&serialized).expect("re-parses");
        assert_eq!(reparsed.moods.axes.blend_steepness, 3.5);
        assert_eq!(reparsed.moods.source, cfg.moods.source);
    }

    #[test]
    fn test_moods_file_deserialization() {
        // The bundled moods file must parse and yield anchors.
        let moods = MoodsFile::default();
        assert!(!moods.pairs.is_empty());
        assert!(moods.pairs.iter().all(|p| !p.mood.is_empty()));

        // A moods file with explicit entries deserializes.
        let moods: MoodsFile = toml::from_str(
            "[[pairs]]\nmood = \"happy\"\ncolor = \"#FF0000\"\n\
             [[pairs]]\nmood = \"sad\"\ncolor = \"blue\"\n",
        )
        .expect("moods file parses");
        assert_eq!(moods.pairs.len(), 2);
        assert_eq!(moods.pairs[0].mood, "happy");
        assert_eq!(moods.pairs[1].color, crossterm::style::Color::Blue);

        // Unknown keys in the moods file are rejected.
        assert!(toml::from_str::<MoodsFile>("bogus = 1\n").is_err());
    }

    #[test]
    fn test_load_pairs_default_when_source_empty() {
        // Empty source (the default) resolves to the bundled pairs, and
        // never touches the filesystem.
        let config = MoodConfig::default();
        assert!(config.source.as_os_str().is_empty());
        let pairs = config.load_pairs();
        assert_eq!(pairs.len(), MoodsFile::default().pairs.len());
        assert!(!pairs.is_empty());
    }
}
