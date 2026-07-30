/// Unified action enum emitted by the event loop and consumed by render loops.
///
/// Both `TodayApp` and `App` match on every variant and ignore ones that don't
/// apply to their context. A single enum keeps the event loop simple — no
/// view-context tracking is required to decide which action type to emit.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Move selection up.
    Up,
    /// Move selection down.
    Down,

    /// Move modal button selection left (confirm modal Yes/No navigation).
    Left,
    /// Move modal button selection right (confirm modal Yes/No navigation).
    Right,

    /// Primary action: context-dependent.
    ///
    /// - TodayApp: complete selected task, or add a tracker entry if the
    ///   tracking modal is open.
    /// - App: mark selected task complete.
    /// - In `@done` view: show confirm modal asking whether to reset progress
    ///   (recurring tasks only).
    /// - In a DeleteConfirm modal: close the modal and re-send `Delete(true)`
    ///   through the render-event channel.
    Accept,

    /// Edit the selected item.
    ///
    /// - Tasks (oneshot and recurring): edit body via external editor.
    /// - Custom tracker (Text): edit value via external editor.
    /// - Custom tracker (Number / Float): edit numeric value via external
    ///   editor.
    /// - Mood entry: edit body via external editor.
    Edit,

    /// Delete the selected item. `false` opens a confirmation modal;
    /// `true` executes the deletion (re-sent by `Accept` while the
    /// DeleteConfirm modal is open).
    Delete(bool),

    /// Cycle the view mode.
    ///
    /// - TodayApp: cycles horizon (today → +tomorrow → +this week → today).
    /// - App: cycles ViewMode (OneShot → Recurring → Done → Due → Scheduled → ...).
    CycleMode,

    /// Toggle the task list sort direction.
    ToggleSort,

    /// Toggle whether scheduled tasks are included in the current view
    /// (tasks app only: `!`, `@`, `@done`, `@due`).
    ToggleScheduled,

    /// Toggle whether completed tasks are included in the current view
    /// (tasks app only).
    ToggleCompleted,

    /// Reload data from the database.
    Refresh,

    /// Exit the TUI.
    Quit,

    /// Acknowledgment from the event loop that a [`crate::message::ControlEvent`]
    /// (Pause/Resume) has been processed. The render loop waits for this
    /// after sending a control, so it knows input capture has stopped /
    /// restarted before (re)entering an external process.
    Ack,

    // ----- Modal input actions -----
    // These are emitted when any modal is open and the render loop should
    // route them to the modal rather than the main view.
    /// A character typed by the user that no bind matched (see
    /// `event_loop::default_binds`). Routed to the active modal input field,
    /// or ignored when no modal is open.
    Input(char),
}
