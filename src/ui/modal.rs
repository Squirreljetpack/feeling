/// Shared numeric completion prompt state used by both interactive TUIs.
#[derive(Debug)]
pub(crate) struct CompletePrompt {
    pub(crate) task_id: i64,
    pub(crate) input: String,
    pub(crate) error: Option<String>,
}

/// Shared delete confirmation payload.
#[derive(Debug)]
pub(crate) struct DeleteConfirmation {
    pub(crate) name: String,
    pub(crate) is_recurring: bool,
    pub(crate) cursor: usize,
}

/// Shared reset-progress confirmation payload.
#[derive(Debug)]
pub(crate) struct ResetConfirmation {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) cursor: usize,
}

/// Shared availability-window confirmation payload.
#[derive(Debug)]
pub(crate) struct AvailabilityConfirmation {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) cursor: usize,
}
