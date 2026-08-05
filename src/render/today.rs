use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Cell, Clear, Padding, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};
use sqlx::SqlitePool;
use std::io::Write;
use tokio::sync::mpsc;

use crate::action::Action;
use crate::message::{ControlEvent, RenderEvent};
use crate::tui::Tui;

use super::{build_preview, confirm_buttons, system::edit_with_editor, truncate_chars, Render};

// ---------- Today View App ----------

pub struct TodayApp {
    pool: SqlitePool,
    config: crate::config::Config,
    pub(crate) entries: Vec<crate::views::TodayEntry>,
    pub(crate) selected: usize,
    pub(crate) horizon: crate::views::TodayHorizon,
    /// Day the view is anchored to (`None` = today). `feeling @<date>`.
    day_epoch: Option<i64>,
    /// Title label for the anchored day: "Today" / "Yesterday" / DD-MM-YY.
    day_label: String,
    pub(crate) sort_by_priority: bool,
    should_quit: bool,
    pub(crate) modal: Option<Modal>,
    pub(crate) selected_task: Option<crate::sql::TaskRow>,
    pub(crate) color_cache: std::collections::HashMap<String, oklab::Oklab>,
}

/// Modal prompt state for the today view.
pub(crate) enum Modal {
    /// Numeric completion-count prompt for tasks with a target_count.
    Complete(CompleteModal),
    /// Confirm before deleting the selected feeling entry. `cursor` selects
    /// the navigable button (0 = Yes, 1 = No).
    DeleteConfirm { name: String, cursor: usize },
    /// Confirm before resetting a completed task's progress (target_count
    /// > 1 done, or any done task in the tasks app's @done view). Default Yes.
    ResetConfirm {
        id: i64,
        name: String,
        cursor: usize,
    },
    /// Numeric payload edit for a Number/Float tracker; only accepts when
    /// the input parses as the tracker's kind.
    EditTracker(EditTrackerModal),
}

/// Modal prompt state for marking a task complete when it has a target_count.
pub(crate) struct CompleteModal {
    pub(crate) task_id: i64,
    pub(crate) input: String,
    pub(crate) error: Option<String>,
}

/// Modal state for editing a Number/Float tracker payload in place.
pub(crate) struct EditTrackerModal {
    /// custom row id.
    pub(crate) custom_id: i64,
    /// Tracker type name from config (e.g. "sleep").
    pub(crate) tracker_type: String,
    /// Payload kind: Number (i64) or Float (f64). Text trackers don't use
    /// this modal — they open the external editor.
    pub(crate) kind: crate::config::TrackerType,
    pub(crate) input: String,
    pub(crate) error: Option<String>,
}

impl TodayApp {
    pub async fn new(pool: &SqlitePool, config: crate::config::Config, day_epoch: Option<i64>) -> Self {
        let mut color_cache = std::collections::HashMap::new();
        let entries = crate::views::fetch_today_entries(
            pool,
            &config,
            crate::views::TodayHorizon::Today,
            day_epoch,
            &mut color_cache,
        )
        .await
        .unwrap_or_default();
        let day_label = day_label_for(day_epoch);
        let mut app = Self {
            pool: pool.clone(),
            config,
            entries,
            selected: 0,
            horizon: crate::views::TodayHorizon::Today,
            day_epoch,
            day_label,
            sort_by_priority: false,
            should_quit: false,
            modal: None,
            selected_task: None,
            color_cache,
        };
        app.apply_sort();
        app.fetch_selected_task().await;
        app
    }

    async fn refresh(&mut self) {
        self.entries = crate::views::fetch_today_entries(
            &self.pool,
            &self.config,
            self.horizon,
            self.day_epoch,
            &mut self.color_cache,
        )
        .await
        .unwrap_or_default();
        self.apply_sort();
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        self.fetch_selected_task().await;
    }

    fn apply_sort(&mut self) {
        if self.sort_by_priority {
            self.entries
                .sort_by(|a, b| b.priority.cmp(&a.priority).then(a.time.cmp(&b.time)));
        } else {
            self.entries.sort_by_key(|e| e.time);
        }
    }

    fn toggle_sort(&mut self) {
        self.sort_by_priority = !self.sort_by_priority;
        self.apply_sort();
    }

    async fn cycle_horizon(&mut self) {
        self.horizon = self.horizon.next();
        self.refresh().await;
    }

    /// Fetch the full TaskRow for the currently selected entry (if it is a
    /// task row).
    async fn fetch_selected_task(&mut self) {
        self.selected_task = None;
        let entry = match self.entries.get(self.selected) {
            Some(e) if e.entry_type == "task" => e,
            _ => return,
        };
        let Some(task_id) = entry.task_id else { return };
        self.selected_task = crate::sql::fetch_task_by_id(&self.pool, task_id, crate::date::now())
            .await
            .ok()
            .flatten();
    }

    async fn mark_selected_complete(&mut self) {
        let Some(task) = self.selected_task.as_ref() else {
            return;
        };
        let action = crate::task::enter_action(
            task.completions,
            task.is_scheduled(),
            task.target_count,
            task.start_time,
            task.available_duration_secs,
            crate::date::now(),
        );
        match action {
            // Modal-less toggle steps: scheduled cycles 1 → 0 → (none | 1),
            // done once-only/target-1 tasks reset directly.
            crate::task::EnterAction::Complete
            | crate::task::EnterAction::SetFailed
            | crate::task::EnterAction::Clear => {
                let task = task.clone();
                if let Err(e) = crate::task::apply_enter_action(&self.pool, &task, action).await {
                    log::error!("Failed to apply enter action to task {}: {e}", task.id);
                }
                self.refresh().await;
            }
            crate::task::EnterAction::Reset => {
                let task = task.clone();
                if let Err(e) = crate::task::reset_task_progress(&self.pool, &task).await {
                    log::error!("Failed to reset task progress for {}: {e}", task.id);
                }
                self.refresh().await;
            }
            crate::task::EnterAction::ResetConfirm => {
                // target_count > 1 done: ask first (default Yes).
                self.modal = Some(Modal::ResetConfirm {
                    id: task.id,
                    name: task.name.clone(),
                    cursor: 0,
                });
            }
            crate::task::EnterAction::CompletePrompt => {
                // target_count > 1 not done: the numeric prompt.
                self.modal = Some(Modal::Complete(CompleteModal {
                    task_id: task.id,
                    input: String::new(),
                    error: None,
                }));
            }
        }
    }

    /// Apply a completion delta to a task. Positive deltas append a new
    /// completion event with the delta as its count; negative deltas consume
    /// the most recent events within the current interval (recurring tasks).
    async fn complete_task(&mut self, task_id: i64, delta: i32) {
        let _ = crate::task::apply_completion_delta(&self.pool, task_id, delta).await;
        self.refresh().await;
    }

    /// Reset the selected task's completion progress (the ResetConfirm
    /// modal's confirmed action): recurring tasks keep earlier intervals.
    async fn reset_selected_progress(&mut self) {
        let Some(Modal::ResetConfirm { id, .. }) = self.modal.take() else {
            return;
        };
        // The modal always opens for the selected task; fall back to a
        // defensive refetch if the selection moved.
        let task = match self.selected_task.as_ref() {
            Some(t) if t.id == id => t.clone(),
            _ => match crate::sql::fetch_task_by_id(&self.pool, id, crate::date::now())
                .await
                .ok()
                .flatten()
            {
                Some(t) => t,
                None => {
                    self.refresh().await;
                    return;
                }
            },
        };
        if let Err(e) = crate::task::reset_task_progress(&self.pool, &task).await {
            log::error!("Failed to reset task progress for {id}: {e}");
        }
        self.refresh().await;
    }

    /// Apply a non-mutating routing of an [`Action`] when a modal is open.
    /// Returns `true` if the action was consumed by the modal.
    async fn handle_modal_action(
        &mut self,
        action: &Action,
        tx: &mpsc::UnboundedSender<RenderEvent>,
    ) -> bool {
        // Accept while a DeleteConfirm modal is open: "Yes" (cursor 0)
        // closes the modal and re-sends `Delete(true)` through the
        // render-event channel so it is handled by the main match in
        // `handle_action`; "No" (cursor 1) just closes it.
        if let Some(Modal::DeleteConfirm { cursor, .. }) = self.modal.as_mut() {
            match action {
                Action::Accept => {
                    let yes = *cursor == 0;
                    self.modal = None;
                    if yes {
                        let _ = tx.send(RenderEvent::Action(Action::Delete(true)));
                    }
                }
                Action::Left => *cursor = 0,
                Action::Right => *cursor = 1,
                Action::Input(c) if c.eq_ignore_ascii_case(&'y') => {
                    self.modal = None;
                    let _ = tx.send(RenderEvent::Action(Action::Delete(true)));
                }
                Action::Input(c) if c.eq_ignore_ascii_case(&'n') => {
                    self.modal = None;
                }
                Action::Quit => {
                    self.modal = None;
                }
                _ => {}
            }
            return true;
        }

        // Accept while a ResetConfirm modal is open: "Yes" (cursor 0)
        // resets the task's progress; "No" just closes.
        if let Some(Modal::ResetConfirm { cursor, .. }) = self.modal.as_mut() {
            match action {
                Action::Accept => {
                    if *cursor == 0 {
                        self.reset_selected_progress().await;
                    } else {
                        self.modal = None;
                    }
                }
                Action::Left => *cursor = 0,
                Action::Right => *cursor = 1,
                Action::Input(c) if c.eq_ignore_ascii_case(&'y') => {
                    self.reset_selected_progress().await;
                }
                Action::Input(c) if c.eq_ignore_ascii_case(&'n') => {
                    self.modal = None;
                }
                Action::Quit => {
                    self.modal = None;
                }
                _ => {}
            }
            return true;
        }

        // Numeric tracker payload edit: Accept validates the input against
        // the tracker's kind and only applies on success.
        if matches!(self.modal, Some(Modal::EditTracker(_))) {
            match action {
                Action::Accept => self.submit_edit_tracker().await,
                Action::Quit => {
                    self.modal = None;
                }
                Action::Delete(_) => {
                    if let Some(Modal::EditTracker(m)) = self.modal.as_mut() {
                        m.input.pop();
                        m.error = None;
                    }
                }
                Action::Input(c) if c.is_ascii_digit() || *c == '-' || *c == '.' => {
                    if let Some(Modal::EditTracker(m)) = self.modal.as_mut() {
                        m.input.push(*c);
                        m.error = None;
                    }
                }
                _ => {}
            }
            return true;
        }

        let Some(Modal::Complete(modal)) = self.modal.as_mut() else {
            return false;
        };
        match action {
            // Accept (Enter key) confirms the modal.
            Action::Accept => self.submit_complete_modal().await,
            Action::Quit => {
                self.modal = None;
            }
            Action::Delete(_) => {
                modal.input.pop();
                modal.error = None;
            }
            Action::Input(c) if c.is_ascii_digit() || *c == '-' => {
                modal.input.push(*c);
                modal.error = None;
            }
            // Anything else is ignored while a modal is open.
            _ => {}
        }
        true
    }

    /// Validate the tracker-payload modal input against the tracker's kind
    /// and apply it. Invalid input keeps the modal open with an error.
    async fn submit_edit_tracker(&mut self) {
        let Some(Modal::EditTracker(m)) = self.modal.as_ref() else {
            return;
        };
        let custom_id = m.custom_id;
        let kind = m.kind;
        let input = m.input.trim().to_string();
        let valid = match kind {
            crate::config::TrackerType::Number => input.parse::<i64>().is_ok(),
            crate::config::TrackerType::Float => input.parse::<f64>().is_ok(),
            crate::config::TrackerType::Text => true,
        };
        if !valid {
            let m = self.modal.as_mut().expect("modal");
            let Modal::EditTracker(m) = m else {
                unreachable!()
            };
            m.error = Some(format!(
                "must be a valid {}",
                match kind {
                    crate::config::TrackerType::Number => "number",
                    crate::config::TrackerType::Float => "float",
                    crate::config::TrackerType::Text => "text",
                }
            ));
            return;
        }
        self.modal = None;
        self.update_custom_score(custom_id, kind, &input).await;
        self.refresh().await;
    }

    async fn submit_complete_modal(&mut self) {
        let Some(Modal::Complete(modal)) = self.modal.as_ref() else {
            return;
        };
        let task_id = modal.task_id;
        let input = modal.input.trim().to_string();
        let parsed: Option<i32> = if input.is_empty() {
            Some(1)
        } else {
            input.parse::<i32>().ok()
        };
        match parsed {
            // 0 is allowed: it completes as a no-op (no completions added
            // or removed).
            Some(delta) => {
                self.modal = None;
                self.complete_task(task_id, delta).await;
            }
            None => {
                let modal = self.modal.as_mut().expect("modal");
                let Modal::Complete(modal) = modal else {
                    unreachable!()
                };
                modal.error = Some("invalid number".into());
            }
        }
    }

    /// Edit the selected entry: task body / mood body via editor, tracker
    /// payload via editor (text kind) or validation modal (number/float).
    async fn edit_selected(
        &mut self,
        tui: &mut Tui<impl Write>,
        controller: &mpsc::UnboundedSender<ControlEvent>,
        rx: &mut mpsc::UnboundedReceiver<RenderEvent>,
    ) {
        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };
        match entry.entry_type {
            // Task body edit (oneshot or recurring).
            "task" => {
                let Some(task) = self.selected_task.as_ref() else {
                    return;
                };
                let body = task.body.clone();
                if let Some(new_body) = edit_with_editor(tui, controller, rx, &body).await {
                    self.update_todo_body(task.id, &new_body).await;
                    self.refresh().await;
                }
            }
            // Tracker payload: text opens the editor; number/float open a
            // validation modal.
            "custom" => {
                let Some(custom_id) = entry.id else { return };
                let Some((tracker_type, current)) = entry.label.split_once(':') else {
                    return;
                };
                let tracker_type = tracker_type.trim();
                let current = current.trim();
                let kind = self
                    .config
                    .tracker
                    .get(tracker_type)
                    .map(|t| t.kind)
                    .unwrap_or(crate::config::TrackerType::Float);
                match kind {
                    crate::config::TrackerType::Text => {
                        if let Some(new_value) =
                            edit_with_editor(tui, controller, rx, current).await
                        {
                            self.update_custom_score(custom_id, kind, &new_value).await;
                            self.refresh().await;
                        }
                    }
                    crate::config::TrackerType::Number | crate::config::TrackerType::Float => {
                        self.modal = Some(Modal::EditTracker(EditTrackerModal {
                            custom_id,
                            tracker_type: tracker_type.to_string(),
                            kind,
                            input: current.to_string(),
                            error: None,
                        }));
                    }
                }
            }
            // Mood body edit.
            "feeling" => {
                let Some(id) = entry.id else { return };
                let body = entry.body.to_string();
                if let Some(new_body) = edit_with_editor(tui, controller, rx, &body).await {
                    self.update_feeling_body(id, &new_body).await;
                    self.refresh().await;
                }
            }
            // Completions aren't editable.
            _ => {}
        }
    }

    async fn update_todo_body(&self, id: i64, body: &str) {
        let _ = crate::sql::update_todo_body(&self.pool, id, body).await;
    }

    async fn update_feeling_body(&self, id: i64, body: &str) {
        let _ = crate::sql::update_feeling_body(&self.pool, id, body).await;
    }

    async fn update_custom_score(&self, id: i64, kind: crate::config::TrackerType, value: &str) {
        let _ = crate::sql::update_custom_score(&self.pool, id, kind, value).await;
    }
}

impl TodayApp {
    /// Delete a feeling row and any linked custom tracker rows in a
    /// transaction. `custom.feeling` has a FK to `feeling(id)` with no
    /// ON DELETE CASCADE, so linked custom rows must be deleted first
    /// (handled inside `sql::delete_feeling`).
    async fn delete_feeling(&self, id: i64) {
        if let Err(e) = crate::sql::delete_feeling(&self.pool, id).await {
            cba::ebog!("delete-feeling"; "{e:#}");
        }
    }
}

impl Render for TodayApp {
    fn render(&self, f: &mut Frame) {
        let area = f.area();

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area);

        render_today_entry_list(f, self, chunks[0]);
        render_today_preview(f, self, chunks[1]);

        if self.modal.is_some() {
            render_today_modal(f, self);
        }
    }

    /// Dispatch a single [`Action`] to the right handler.
    ///
    /// Modal confirmations don't recurse: `Accept` in a DeleteConfirm modal
    /// closes the modal and re-sends `Delete(true)` through the render-event
    /// channel (`tx`), so it arrives here as a fresh event on the next loop
    /// iteration.
    async fn handle_action(
        &mut self,
        tui: &mut Tui<impl Write>,
        controller: &mpsc::UnboundedSender<ControlEvent>,
        tx: &mpsc::UnboundedSender<RenderEvent>,
        rx: &mut mpsc::UnboundedReceiver<RenderEvent>,
        action: Action,
    ) {
        // Modal routing wins over main routing.
        if self.modal.is_some() {
            self.handle_modal_action(&action, tx).await;
            return;
        }

        match action {
            Action::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.fetch_selected_task().await;
            }
            Action::Down => {
                if !self.entries.is_empty() {
                    self.selected = (self.selected + 1).min(self.entries.len() - 1);
                    self.fetch_selected_task().await;
                }
            }
            Action::CycleMode => self.cycle_horizon().await,
            Action::ToggleSort => self.toggle_sort(),
            // Scheduled/completed toggles are tasks-app-only; the today view
            // always includes scheduled tasks (window overlap) and completed
            // activity, so both are no-ops here.
            Action::ToggleScheduled | Action::ToggleCompleted => {}
            Action::Refresh => self.refresh().await,
            Action::Quit => self.should_quit = true,
            Action::Accept => self.mark_selected_complete().await,
            Action::Delete(false) => {
                // Only feeling entries are deletable in the today view.
                if let Some(entry) = self
                    .entries
                    .get(self.selected)
                    .filter(|e| e.entry_type == "feeling")
                {
                    self.modal = Some(Modal::DeleteConfirm {
                        name: entry.label.clone(),
                        // Default to the safe option (No).
                        cursor: 1,
                    });
                }
            }
            Action::Delete(true) => {
                // Sent by `Accept` in the DeleteConfirm modal (which closes
                // itself first); delete the selected feeling entry.
                if let Some(entry) = self
                    .entries
                    .get(self.selected)
                    .filter(|e| e.entry_type == "feeling")
                {
                    if let Some(id) = entry.id {
                        self.delete_feeling(id).await;
                        self.refresh().await;
                    }
                }
            }
            Action::Edit => self.edit_selected(tui, controller, rx).await,
            // Modal keys without an open modal are no-ops (Left/Right are
            // modal-only, handled in `handle_modal_action`).
            Action::Input(_) | Action::Ack | Action::Left | Action::Right => {}
        }
    }

    fn should_quit(&self) -> bool {
        self.should_quit
    }
}

/// Title label for the anchored day: "Today" / "Yesterday" / DD-MM-YY.
fn day_label_for(day_epoch: Option<i64>) -> String {
    match day_epoch {
        None => "Today".to_string(),
        Some(ts) if ts == crate::date::today_start() => "Today".to_string(),
        Some(ts) if ts == crate::date::today_start() - 86400 => "Yesterday".to_string(),
        Some(ts) => crate::date::format_date_dmy(ts),
    }
}

fn render_today_entry_list(f: &mut Frame, app: &TodayApp, area: Rect) {
    let sort_indicator = if app.sort_by_priority {
        "priority"
    } else {
        "time"
    };
    // Parenthesized horizon suffix only when it differs from plain "today".
    let horizon_suffix = if app.horizon == crate::views::TodayHorizon::Today {
        String::new()
    } else {
        format!(" ({})", app.horizon.label())
    };
    let title = format!(" {}{} [sort: {}] ", app.day_label, horizon_suffix, sort_indicator);

    // Last column width = area.width minus the fixed time (6) and dot (4) columns.
    let entry_col_width = area.width.saturating_sub(10) as usize;

    let rows: Vec<Row> = app
        .entries
        .iter()
        .map(|entry| {
            let time = crate::date::format_time(entry.time);
            let dot_style = Style::default().fg(entry.color);
            // Feeling entries with no mood get the first line of the body in the
            // last column (truncated to fit the column). All other entries show
            // their label as-is.
            let label_cell = if entry.entry_type == "feeling" && entry.label.is_empty() {
                let body = entry.body.lines().next().unwrap_or("");
                Cell::from(truncate_chars(body, entry_col_width))
            } else {
                Cell::from(truncate_chars(&entry.label, entry_col_width))
            };
            let cells = vec![
                Cell::from(time),
                Cell::from(entry.badge.to_string()).style(dot_style),
                label_cell,
            ];
            Row::new(cells).height(1)
        })
        .collect();

    let table = Table::new(
        rows,
        &[
            Constraint::Length(6),
            Constraint::Length(4),
            Constraint::Fill(1),
        ],
    )
    .block(Block::bordered().title(title))
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("");

    let mut table_state = TableState::new();
    if !app.entries.is_empty() {
        table_state = table_state.with_selected(Some(app.selected));
    }
    f.render_stateful_widget(table, area, &mut table_state);
}

fn render_today_preview(f: &mut Frame, app: &TodayApp, area: Rect) {
    let paragraph = if let Some(task) = &app.selected_task {
        let lines = build_preview(task);
        Paragraph::new(Text::from(lines))
            .block(Block::bordered().title("Preview"))
            .style(Style::default())
            .wrap(Wrap { trim: false })
    } else if let Some(entry) = app.entries.get(app.selected) {
        let lines = super::build_today_preview(entry);
        Paragraph::new(Text::from(lines))
            .block(Block::bordered().title("Preview"))
            .style(Style::default())
            .wrap(Wrap { trim: false })
    } else {
        Paragraph::new(Text::from("Nothing today"))
            .block(Block::bordered().title("Preview"))
            .style(Style::default())
            .wrap(Wrap { trim: false })
    };
    f.render_widget(paragraph, area);
}

/// Label prefix for the count input in the Complete modal. The box is sized
/// exactly to this label plus the typed input, and the cursor is placed
/// right after both — keep this and the cursor math in [`render_today_modal`]
/// in sync.
const COUNT_LABEL: &str = "Count: ";

fn render_today_modal(f: &mut Frame, app: &TodayApp) {
    let modal = app.modal.as_ref().expect("modal must be open");
    let area = f.area();

    let (title, mut lines, buttons): (Option<String>, Vec<Line>, Option<Line>) = match modal {
        Modal::Complete(modal) => {
            // Count display: starts at 1, overwritten by whatever is typed.
            let input_display = if modal.input.is_empty() {
                "1".to_string()
            } else {
                modal.input.clone()
            };
            let mut lines = vec![Line::from(vec![
                Span::styled(COUNT_LABEL, Style::default().fg(Color::Yellow)),
                Span::styled(input_display, Style::default().fg(Color::White)),
            ])];

            if let Some(err) = &modal.error {
                lines.push(Line::from(Span::styled(
                    format!(" ✗ {}", err),
                    Style::default().fg(Color::LightRed),
                )));
            }
            (Some("Update".to_string()), lines, None)
        }
        Modal::EditTracker(modal) => {
            let kind_label = match modal.kind {
                crate::config::TrackerType::Number => "number",
                crate::config::TrackerType::Float => "float",
                crate::config::TrackerType::Text => "text",
            };
            let mut lines = vec![
                Line::from(Span::styled(
                    format!(" Edit '{}' ({kind_label}) ", modal.tracker_type),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    " Enter: save | Esc: cancel ",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
            ];
            let input_display = if modal.input.is_empty() {
                "(empty)".to_string()
            } else {
                modal.input.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(" Value: ", Style::default().fg(Color::Yellow)),
                Span::styled(input_display, Style::default().fg(Color::White)),
            ]));
            if let Some(err) = &modal.error {
                lines.push(Line::from(Span::styled(
                    format!(" ✗ {}", err),
                    Style::default().fg(Color::LightRed),
                )));
            }
            (Some("Edit Tracker".to_string()), lines, None)
        }
        Modal::DeleteConfirm { name, cursor } => {
            let lines = vec![
                Line::from(vec![
                    Span::styled(
                        "Delete",
                        Style::default()
                            .fg(Color::Red)
                            .add_modifier(Modifier::ITALIC),
                    ),
                    Span::raw(format!(" '{}'?", name)),
                ]),
                Line::from(""),
            ];
            (None, lines, Some(confirm_buttons(*cursor)))
        }
        Modal::ResetConfirm { name, cursor, .. } => {
            let lines = vec![
                Line::from(vec![
                    Span::styled(
                        "Reset progress of",
                        Style::default()
                            .fg(Color::Red)
                            .add_modifier(Modifier::ITALIC),
                    ),
                    Span::raw(format!(" '{}'?", name)),
                ]),
                Line::from(""),
            ];
            (None, lines, Some(confirm_buttons(*cursor)))
        }
    };

    // Centered box. The count modal is perfectly sized to its content and
    // drawn with zero padding, so "Count:" sits flush against the border
    // and the cursor can be parked right after the typed input; the confirm
    // modals keep the standard wide centered box.
    let (width, height, padding) = match modal {
        Modal::Complete(modal) => {
            // Box is sized to the count line ("Count: " + typed input)
            // only; an error line, if any, keeps its row but may clip.
            let input_display = if modal.input.is_empty() {
                "1"
            } else {
                modal.input.as_str()
            };
            let content_width = COUNT_LABEL.len() + input_display.len() + 1;
            (
                (content_width as u16 + 2).min(area.width),
                (lines.len() as u16 + 2).min(area.height),
                Padding::ZERO,
            )
        }
        _ => (
            (area.width / 2).clamp(40, area.width.saturating_sub(2)),
            7,
            Padding::new(2, 2, 0, 1),
        ),
    };
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect::new(x, y, width, height);

    // Center the confirm buttons on the last inner row (fist-style manual
    // span padding keeps the rest of the content left-aligned).
    if let Some(buttons) = buttons {
        let inner_width = popup.width.saturating_sub(6); // 2 borders + 2 left + 2 right padding
        let leading = inner_width.saturating_sub(buttons.width() as u16) / 2;
        let mut spans = vec![Span::raw(" ".repeat(leading as usize))];
        spans.extend(buttons.spans);
        lines.push(Line::from(spans));
    }

    // Fist-style overlay: `Clear` wipes whatever the list/preview drew
    // underneath so no text shows through the box, but the terminal
    // background is left untouched (no bg paint).
    f.render_widget(Clear, popup);
    let mut block = Block::bordered()
        .border_style(Style::default().fg(Color::White))
        .padding(padding);
    if let Some(title) = &title {
        block = block.title(title.clone());
    }
    let paragraph = Paragraph::new(Text::from(lines)).block(block);
    f.render_widget(paragraph, popup);

    // Park the cursor right after "Count: " + the typed input (inner row 1),
    // so the next keystroke lands at the end of what's displayed. When the
    // input is empty the placeholder "1" is shown and the cursor sits at it.
    if let Modal::Complete(modal) = modal {
        f.set_cursor_position((
            popup.x + 1 + COUNT_LABEL.len() as u16 + modal.input.len() as u16,
            popup.y + 1,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_day_label() {
        let today = crate::date::today_start();
        // Anchored today (explicit or implicit) → "Today".
        assert_eq!(day_label_for(None), "Today");
        assert_eq!(day_label_for(Some(today)), "Today");
        // Yesterday.
        assert_eq!(day_label_for(Some(today - 86400)), "Yesterday");
        // Any other day → DD-MM-YY.
        let other = crate::date::parse_datetime("2024-03-15", crate::date::DateDialect::Uk).unwrap();
        assert_eq!(day_label_for(Some(crate::date::day_start(other))), "15-03-24");
    }
}
