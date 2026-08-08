use serde::{Deserialize, Serialize};

/// The day each week starts on, as configured in `[grid]` (`"Monday"` …
/// `"Sunday"`, case-insensitive on parse). jiff-english's serde-friendly
/// `Weekday` wrapper (jiff's `Weekday` has no serde impl); converts at use
/// sites via `jiff::civil::Weekday::from`.
pub use jiff_english::serde::Weekday;

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
    pub week_start: Weekday,
}

impl Default for GridViewConfig {
    fn default() -> Self {
        Self {
            week_rolling: false,
            year_rolling: true,
            month_rolling: true,
            week_start: Weekday::Monday,
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

/// `[today_view]` section — options for the today view (bare `im`,
/// `im @<date>`, and the today TUI).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TodayViewConfig {
    /// Include overdue oneshot tasks (due before today) in the today view;
    /// when false, only tasks due within the horizon are shown.
    #[serde(default)]
    pub include_overdue: bool,
    /// Glyph shown next to journal-only entries (a mood entry with no mood
    /// word). Omit the key to show no badge.
    #[serde(default)]
    pub journal_badge: Option<char>,
    /// Merge a task's adjacent completion entries into a single "done" row
    /// in the today view (currently accepted and stored on TodayApp; no
    /// behavior yet).
    #[serde(default)]
    pub coalesce_completions: bool,
}
