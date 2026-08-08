use crate::date::{self, Epoch};

/// Category of a task, derived from its scheduling fields or selected during creation.
///
/// A target count distinguishes threshold-style completion behavior, but does not
/// create a separate task kind: those tasks are still one-shot tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    /// One-shot task, with or without a completion target.
    Oneshot,
    /// Recurring task (has an interval).
    Recurring,
    /// Scheduled task (has an availability window and no interval).
    Scheduled,
}

impl std::fmt::Display for TaskKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TaskKind::Oneshot => "oneshot",
            TaskKind::Recurring => "recurring",
            TaskKind::Scheduled => "scheduled",
        })
    }
}

/// The task-list mode selected by `@` and `@done` views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    PendingTasks,
    DoneTasks,
}

/// Shared view subset control used by both the task and today TUIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewVariant {
    #[default]
    All,
    A,
    B,
}

impl ViewVariant {
    /// Short suffix label used in TUI titles: `[show: a|b|all]`.
    pub fn label(&self) -> &'static str {
        match self {
            ViewVariant::All => "all",
            ViewVariant::A => "a",
            ViewVariant::B => "b",
        }
    }

    /// Cycle order: All → A → B → All.
    pub fn next(&self) -> Self {
        match self {
            ViewVariant::All => ViewVariant::A,
            ViewVariant::A => ViewVariant::B,
            ViewVariant::B => ViewVariant::All,
        }
    }
}

/// How far ahead to include incomplete todos in the today view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodayHorizon {
    Today,
    Tomorrow,
    Week,
}

impl TodayHorizon {
    pub fn next(&self) -> Self {
        match self {
            TodayHorizon::Today => TodayHorizon::Tomorrow,
            TodayHorizon::Tomorrow => TodayHorizon::Week,
            TodayHorizon::Week => TodayHorizon::Today,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            TodayHorizon::Today => "today",
            TodayHorizon::Tomorrow => "+tomorrow",
            TodayHorizon::Week => "+this week",
        }
    }

    /// End of the horizon (inclusive) as epoch seconds, relative to the
    /// anchored day (its day-start). `Week` is always the next 7 days from
    /// the anchored day.
    pub fn end_epoch(&self, day_start: i64) -> i64 {
        match self {
            TodayHorizon::Today => date::day_end(day_start),
            TodayHorizon::Tomorrow => date::day_end(day_start + 86400),
            TodayHorizon::Week => date::day_end(day_start + 6 * 86400),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub mood: String,
    // Raw tracker values ("-type value"): interpreted per the tracker's
    // declared kind (text/number/float/null) at write time in handle_entry.
    pub trackers: Vec<(String, String)>,
    /// Task short ids from `-<id>` tokens: resolved to row ids and linked
    /// to the mood entry at write time (a plain link, not a completion).
    pub task_links: Vec<i64>,
    /// Body text accumulated from words following `..`. Empty if `..` was
    /// absent, or if `..` was the last token (in which case the editor
    /// opens in the handler — see `open_editor`).
    pub body: String,
    /// Open the editor iff `..` was present and `body` is empty.
    pub open_editor: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub task_type: TaskKind,
    pub name: Option<String>,
    pub priority: Option<i32>,
    /// Start/due time for oneshot and scheduled creations (`! name @<time>`),
    /// resolved to an epoch at CLI parse time (`DATE_DIALECT`).
    pub date: Option<Epoch>,
    /// Body text from words following `..`. `None` when no `..` was given;
    /// `Some(s)` when `..` was used, with `s` possibly empty (a bare `..`).
    /// How an empty/absent body is resolved (editor or not) is decided in
    /// the handler from whether the creation flow is interactive.
    pub body: Option<String>,
    /// Pre-filled name for interactive recurring creation
    /// (`im ! @ <name>`), like oneshot creation where the
    /// name comes from the command line. `Some` always implies creation.
    pub prefill: Option<String>,
    /// Parent task's short id from `! -<parent_id>`; `None` for a
    /// root-level task. Resolved to a row id at creation time (an
    /// invalid short id errors out in the handler).
    pub parent: Option<i64>,
    /// Available duration in seconds for scheduled creation
    /// (`! @<time>; …; %<duration>`), parsed at CLI parse time; carried into
    /// the interactive flow so the duration prompt can be skipped when it
    /// came from the command line.
    pub available_duration: Option<Epoch>,
}
