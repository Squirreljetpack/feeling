use crate::clap::TaskType;

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub feeling: String,
    // Raw tracker values ("-type value"): interpreted per the tracker's
    // declared kind (text/number/float) at write time in handle_entry.
    pub customs: Vec<(String, String)>,
    /// Body text accumulated from words following `..`. Empty if `..` was
    /// absent, or if `..` was the last token (in which case the editor
    /// opens in the handler — see `open_editor`).
    pub body: String,
    /// Open the editor iff `..` was present and `body` is empty.
    pub open_editor: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub task_type: TaskType,
    pub name: Option<String>,
    pub priority: Option<i32>,
    pub date: Option<String>,
    /// Body text from words following `..` for oneshot creations.
    pub body: String,
    /// Open the editor iff `..` was present and `body` is empty.
    pub open_editor: bool,
    /// Pre-filled name for interactive recurring creation
    /// (`feeling ! @ <description>`), like oneshot creation where the
    /// name comes from the command line. `Some` always implies creation.
    pub prefill: Option<String>,
    /// Raw available-duration string for scheduled creation (`! @<time>;
    /// …; @<duration>`), carried into the interactive flow so the duration
    /// prompt can be skipped when it came from the command line.
    pub available_duration: Option<String>,
}
