use crate::types::{Entry, Task, TodayHorizon, ViewMode, ViewVariant};

pub const FLAG_CHARACTERS: &str = "qv";

/// Counts of the leading `-q` / `-v` flag characters. `qv[0]` = number of
/// `q` chars, `qv[1]` = number of `v` chars (combined tokens like `-qv`
/// count once each). Order is not tracked — the logger and handlers only
/// care about presence/counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CliOpts {
    pub qv: [u8; 2],
}

impl CliOpts {
    pub fn quiet(&self) -> bool {
        self.qv[0] > 0
    }
    pub fn verbose(&self) -> bool {
        self.qv[1] > 0
    }
    /// `-vv`-gated output (e.g. the WP7 grid period suffix).
    pub fn verbose_level(&self) -> u8 {
        self.qv[1]
    }
}

/// A parsed command line: the flags given in the initial position (`-q` /
/// `-v`, as counts) plus the command they apply to. The flags drive log
/// verbosity in `main.rs` and quiet/verbose output in the commands;
/// `cmd` is what `execute_command` dispatches on.
#[derive(Debug, Clone, PartialEq)]
pub struct Cli {
    pub opts: CliOpts,
    pub cmd: Command,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Entry(Entry),
    View {
        mode: ViewMode,
        show: ViewVariant,
    },
    Tracker {
        period: TrackerPeriod,
        items: Vec<TrackerItem>,
    },
    Task(Task),
    Update {
        target: UpdateTarget,
        count: Option<i64>,
    },
    Embed,
    Score {
        start: String,
        end: String,
    },
    /// `feeling` with no args — today view; `feeling @<date>` anchors it to
    /// an arbitrary day (any date string that parses); `feeling @due[:t|:w]`
    /// opens the today view at `ShowVariant::B` with the day/tomorrow/week
    /// horizon. `feeling -` (bare) is TasksEdit.
    Today {
        date: Option<String>,
        show: ViewVariant,
        horizon: TodayHorizon,
    },
    /// `feeling -` (bare) — tasks-edit entry point. The handler is a stub
    /// for now: `handle_tasks_edit` bails "not yet implemented" (interactive
    /// task editing is future work, see TODO.md).
    TasksEdit,
    /// `feeling --help` / `feeling -h` in the initial position (handled in
    /// `parse_cli`, before the command dispatchers — `parse_from` never sees
    /// a help token). Handlers print the contents of `assets/help.txt`.
    Help,
    /// `feeling :config` — handlers open the active config file in
    /// $VISUAL/$EDITOR via [`crate::editor::open_editor_at`]. The bundled
    /// `assets/config.toml` is copied to the path first when missing.
    Config,
    /// `feeling :moods` — like `:config`, but opens the moods file named by
    /// `[moods] source` (relative to the config directory) in
    /// $VISUAL/$EDITOR. A missing file is created from the bundled moods
    /// defaults first; when `source` is unset the handler warns that it
    /// must be configured.
    Moods,
    /// `feeling :prune` — handlers delete completed oneshot tasks and
    /// recurring tasks whose `end_time` has passed.
    Prune,
    /// `feeling :color <feeling>` — embed a mood string (with `"feeling "`
    /// prefix) and print the projected Oklab / sRGB color plus intermediate
    /// pipeline values (raw scores, blend factors, per-axis colors).
    /// Diagnostic tool for debugging the mood-color pipeline.
    Color {
        mood: String,
    },
    /// `feeling :clear [@date]` — clear all mood entries from that day.
    Clear {
        date: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum UpdateTarget {
    /// `feeling - <id> [count]` — the oneshot task with that user-facing
    /// short id. Completed tasks have no short id and are not addressable.
    OneShot(i64),
    /// `feeling - <words…> [count]` — the task whose name contains all
    /// `words` in order (whitespace-separated subsequence match). The
    /// handler requires the match to be unique.
    Query { words: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerPeriod {
    Week,
    Month,
    Year,
}

/// One item in a `feeling :` display list. `Mood` is a positional marker
/// (a bare `:` token in the args) that renders the mood grid at that spot;
/// `Tracker(name)` renders that tracker's grid (`@name` for recurring).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackerItem {
    Mood,
    Tracker(String),
}

mod parse;
mod parser;

pub use parser::{parse_args, parse_cli, parse_from};
