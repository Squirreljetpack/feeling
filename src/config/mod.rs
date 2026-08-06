#![allow(clippy::derivable_impls)]

use cba::{ebog, wbog};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::cli::FLAG_CHARACTERS;

#[cfg(debug_assertions)]
pub const DEFAULT_CONFIG: &str = include_str!("../../assets/dev.toml");
#[cfg(not(debug_assertions))]
pub const DEFAULT_CONFIG: &str = include_str!("../../assets/config.toml");

#[cfg(debug_assertions)]
pub const DEFAULT_MOODS: &str = include_str!("../../assets/moods.dev.toml");
#[cfg(not(debug_assertions))]
pub const DEFAULT_MOODS: &str = include_str!("../../assets/moods.toml");

mod types;
pub use types::*;

mod moods;
pub use moods::*;
mod tasks;
pub use tasks::*;
mod trackers;
pub use trackers::*;
mod views;
pub use views::*;

/// The whole configuration file (`config.toml`). Every section is optional
/// — a missing section or key falls back to a built-in default, so a config
/// can be as small as a single `[tracker.sleep]` block.
///
/// Sections: `[moods]` (color settings; the anchor pairs live in the file
/// named by `[moods] source`), `[tasks]` (defaults for new tasks, badge
/// colors), `[tracker.<name>]` (trackers), `[grid]` (tracker grid
/// ranges), `[tasks_view]` and `[today_view]` (view options), `[editor]`
/// (body editor).
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
    pub editor: EditorConfig,
}

impl Default for Config {
    fn default() -> Self {
        toml::from_str(DEFAULT_CONFIG).expect("bundled assets/config.toml must parse into Config")
    }
}

impl Config {
    /// Normalize a loaded config before use: drop trackers whose
    /// names cannot be addressed from the CLI, clear non-positive tracker
    /// intervals (they would divide by zero when computing replacement
    /// slots), and fall back to the default badge palette when fewer than
    /// three colors are configured. Run automatically at startup, before
    /// any command is handled.
    ///
    /// The bundled default config never needs this; a user-edited config
    /// may. See [`is_valid_tracker_name`] for the exact tracker-name rules.
    pub fn init(&mut self) {
        // Drop trackers whose names are unusable: a `:` prefix collides with
        // the grid-view `:` command, `-`/whitespace can't be addressed as
        // `-name value`, names made purely of the flag characters
        // (`q`/`v`) would be swallowed by the leading `-q`/`-v` flags, and
        // purely numeric names collide with the `! -<parent_id>` flag.
        self.tracker.retain(|name, _| {
            if !is_valid_tracker_name(name) {
                cba::ebog!(
                    "config";
                    "Dropping unusable tracker '{}': names cannot begin with ':', contain '-' or whitespace, be purely numeric, or consist solely of flag characters '{}'",
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
                        "Ignoring colors override on Tracker '{}' with {} entries (<= 2)",
                        name,
                        colors.len()
                    );
                    setting.colors = None;
                }
            }
            // A non-positive interval would divide by zero when computing
            // the tracker's replacement slot, so clear it and warn.
            if setting.interval.is_some_and(|i| i <= 0) {
                ebog!(
                    "config";
                    "Ignoring zero interval setting on Tracker '{name}'"
                );
                setting.interval = None;
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
// no `-` or whitespace, is not made purely of the leading flag
// characters (`q` / `v`), and is not purely numeric — `-123` would
// collide with the `! -<parent_id>` task flag.
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
    if name.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    !name.chars().all(|c| FLAG_CHARACTERS.contains(c))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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
        // Purely numeric names collide with the `! -<parent_id>` flag
        assert!(!is_valid_tracker_name("123"));
        assert!(!is_valid_tracker_name("0"));
        assert!(!is_valid_tracker_name(""));
    }

    #[test]
    fn test_init_clears_non_positive_intervals() {
        let mut config = Config::default();
        for (name, interval) in [("zero", 0), ("negative", -3600)] {
            config.tracker.insert(
                name.to_string(),
                TrackerSetting::new(TrackerKind::Float).with_interval(interval),
            );
        }
        config.tracker.insert(
            "good".to_string(),
            TrackerSetting::new(TrackerKind::Float).with_interval(86_400),
        );
        config.init();

        assert_eq!(config.tracker["zero"].interval, None);
        assert_eq!(config.tracker["negative"].interval, None);
        assert_eq!(config.tracker["good"].interval, Some(86_400));
    }

    #[test]
    fn test_init_drops_invalid_trackers() {
        let mut config = Config::default(); // debug: assets/dev.toml trackers
        for bad in [
            ":collide",
            "sleep-time",
            "two words",
            "q",
            "v",
            "qv",
            "123",
            "",
        ] {
            config
                .tracker
                .insert(bad.to_string(), TrackerSetting::default());
        }
        config.init();

        for bad in [
            ":collide",
            "sleep-time",
            "two words",
            "q",
            "v",
            "qv",
            "123",
            "",
        ] {
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
