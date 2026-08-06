use crate::date::Epoch;

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

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub feeling: String,
    // Raw tracker values ("-type value"): interpreted per the tracker's
    // declared kind (text/number/float) at write time in handle_entry.
    pub trackers: Vec<(String, String)>,
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
    /// Body text from words following `..` for oneshot creations.
    pub body: String,
    /// Open the editor iff `..` was present and `body` is empty.
    pub open_editor: bool,
    /// Pre-filled name for interactive recurring creation
    /// (`feeling ! @ <name>`), like oneshot creation where the
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
