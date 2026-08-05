use anyhow::Result;
use crossterm::style::Color as CtColor;
use ratatui::{
    backend::FromCrossterm,
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
use crate::clap::ViewMode;
use crate::config::Config;
use crate::message::{ControlEvent, RenderEvent};
use crate::sql::TaskRow;
use crate::tui::Tui;

use super::system::edit_with_editor;
use super::{build_preview, confirm_buttons, mode_label, Render};

// ---------- Interactive App ----------

pub struct TasksApp {
    pool: SqlitePool,
    pub(crate) tasks: Vec<TaskRow>,
    pub(crate) selected: usize,
    pub(crate) mode: ViewMode,
    config: Config,
    include_completed: bool,
    /// Whether scheduled tasks are merged into the current view (the
    /// `include_scheduled` fetch flag; toggled with Ctrl+a).
    show_scheduled: bool,
    should_quit: bool,
    pub(crate) sort_by_due: bool,
    pub(crate) modal: Option<Modal>,
}

/// Modal prompt state for the task view.
pub(crate) enum Modal {
    /// Numeric completion-count prompt for tasks with a target_count.
    Complete(CompleteModal),
    /// Confirm before deleting the selected task. `cursor` selects the
    /// navigable button (0 = Yes, 1 = No — the default for deletes);
    /// `is_recurring` shows the "This task will stop recurring!" warning.
    DeleteConfirm {
        name: String,
        is_recurring: bool,
        cursor: usize,
    },
    /// @done view: confirm before resetting the selected task's completion
    /// progress (recurring tasks: current interval only).
    ResetConfirm {
        id: i64,
        name: String,
        cursor: usize,
    },
}

/// Modal prompt state for marking a task complete when it has a target_count.
pub(crate) struct CompleteModal {
    pub(crate) task_id: i64,
    pub(crate) input: String,
    pub(crate) error: Option<String>,
}

impl TasksApp {
    pub async fn new(
        pool: &SqlitePool,
        mode: ViewMode,
        config: Config,
        include_completed: bool,
        show_scheduled: bool,
    ) -> Self {
        let tasks = fetch_tasks(pool, mode, include_completed, show_scheduled)
            .await
            .unwrap_or_default();
        let sort_by_due = true;
        let mut app = Self {
            pool: pool.clone(),
            tasks,
            selected: 0,
            mode,
            config,
            include_completed,
            show_scheduled,
            should_quit: false,
            sort_by_due,
            modal: None,
        };
        app.apply_sort();
        app
    }

    async fn refresh(&mut self) {
        self.tasks =
            fetch_tasks(&self.pool, self.mode, self.include_completed, self.show_scheduled)
                .await
                .unwrap_or_default();
        self.apply_sort();
        self.selected = self.selected.min(self.tasks.len().saturating_sub(1));
    }

    /// Re-sort the current task list according to the selected sort mode.
    fn apply_sort(&mut self) {
        if self.sort_by_due {
            // Nearness of next due date first; priority as tiebreak (descending)
            self.tasks.sort_by(|a, b| {
                a.start_time
                    .unwrap_or(i64::MAX)
                    .cmp(&b.start_time.unwrap_or(i64::MAX))
                    .then(b.priority.cmp(&a.priority))
            });
        } else {
            // Priority descending first; due date as tiebreak
            self.tasks.sort_by(|a, b| {
                b.priority.cmp(&a.priority).then_with(|| {
                    a.start_time
                        .unwrap_or(i64::MAX)
                        .cmp(&b.start_time.unwrap_or(i64::MAX))
                })
            });
        }
    }

    async fn next_mode(&mut self) {
        self.mode = match self.mode {
            ViewMode::OneShotTasks => ViewMode::RecurringTasks,
            ViewMode::RecurringTasks => ViewMode::DoneTasks,
            ViewMode::DoneTasks => ViewMode::DueTasks,
            ViewMode::DueTasks => ViewMode::OneShotTasks,
        };
        self.refresh().await;
    }

    fn toggle_sort(&mut self) {
        self.sort_by_due = !self.sort_by_due;
        self.apply_sort();
    }

    async fn mark_selected_complete(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        let task = &self.tasks[self.selected];
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
                // The @done view asks before resetting; everywhere else a
                // done once-only/target-1 task resets directly.
                if self.mode == ViewMode::DoneTasks {
                    self.modal = Some(Modal::ResetConfirm {
                        id: task.id,
                        name: task.name.clone(),
                        // Default Yes.
                        cursor: 0,
                    });
                } else {
                    let task = task.clone();
                    if let Err(e) = crate::task::reset_task_progress(&self.pool, &task).await {
                        log::error!("Failed to reset task progress for {}: {e}", task.id);
                    }
                    self.refresh().await;
                }
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

        // @done: Accept while the reset-confirm modal is open clears the
        // task's completion progress (current interval for recurring tasks).
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

    async fn submit_complete_modal(&mut self) {
        let Some(Modal::Complete(modal)) = self.modal.as_ref() else {
            return;
        };
        let task_id = modal.task_id;
        let input = modal.input.trim().to_string();
        let parsed: Option<i32> = if input.is_empty() {
            Some(1) // Enter on empty input adds 1
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

    /// Edit the selected task's body via the external editor. Applies to
    /// both oneshot and recurring tasks (same mechanism as mood editing).
    async fn edit_selected_task(
        &mut self,
        tui: &mut Tui<impl Write>,
        controller: &mpsc::UnboundedSender<ControlEvent>,
        rx: &mut mpsc::UnboundedReceiver<RenderEvent>,
    ) {
        let Some(task) = self.tasks.get(self.selected) else {
            return;
        };
        let id = task.id;
        let body = task.body.clone();
        if let Some(new_body) = edit_with_editor(tui, controller, rx, &body).await {
            if let Err(e) = crate::sql::update_todo_body(&self.pool, id, &new_body).await {
                log::error!("Failed to update task body: {e}");
            }
            self.refresh().await;
        }
    }

    /// Delete a task row; `todo_completions` rows cascade automatically
    /// via the FK with `ON DELETE CASCADE`.
    async fn delete_task(&self, id: i64) {
        if let Err(e) = crate::sql::delete_task(&self.pool, id).await {
            log::error!("Failed to delete task {id}: {e}");
        }
    }

    /// @done / target_count > 1: clear the selected task's completion
    /// progress. For recurring tasks only the current interval's completions
    /// are removed, preserving history from earlier intervals.
    async fn reset_selected_progress(&mut self) {
        let Some(Modal::ResetConfirm { id, .. }) = self.modal.take() else {
            return;
        };
        let task = self.tasks.iter().find(|t| t.id == id).cloned();
        let Some(task) = task else {
            return;
        };
        if let Err(e) = crate::task::reset_task_progress(&self.pool, &task).await {
            log::error!("Failed to reset task progress for {id}: {e}");
        }
        self.refresh().await;
    }
}

impl Render for TasksApp {
    fn render(&self, f: &mut Frame) {
        render_app(self, f)
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
            }
            Action::Down => {
                if !self.tasks.is_empty() {
                    self.selected = (self.selected + 1).min(self.tasks.len() - 1);
                }
            }
            Action::CycleMode => self.next_mode().await,
            Action::ToggleSort => self.toggle_sort(),
            Action::ToggleScheduled => {
                self.show_scheduled = !self.show_scheduled;
                self.refresh().await;
            }
            Action::ToggleCompleted => {
                self.include_completed = !self.include_completed;
                self.refresh().await;
            }
            Action::Refresh => self.refresh().await,
            Action::Quit => self.should_quit = true,
            Action::Accept => self.mark_selected_complete().await,
            Action::Delete(false) => {
                if let Some(task) = self.tasks.get(self.selected) {
                    self.modal = Some(Modal::DeleteConfirm {
                        name: task.name.clone(),
                        is_recurring: task.is_recurring(),
                        // Default to the safe option (No).
                        cursor: 1,
                    });
                }
            }
            Action::Delete(true) => {
                // Sent by `Accept` in the DeleteConfirm modal (which closes
                // itself first); delete the selected task.
                if let Some(task) = self.tasks.get(self.selected) {
                    let id = task.id;
                    self.delete_task(id).await;
                    self.refresh().await;
                }
            }
            Action::Edit => self.edit_selected_task(tui, controller, rx).await,
            // no-ops (Left/Right are modal-only, handled in `handle_modal_action`).
            Action::Input(_) | Action::Ack | Action::Left | Action::Right => {}
        }
    }

    fn should_quit(&self) -> bool {
        self.should_quit
    }
}

// ---------- Render helpers for App ----------

fn render_app(app: &TasksApp, f: &mut Frame) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    render_app_task_list(f, app, chunks[0]);
    render_app_preview(f, app, chunks[1]);

    if app.modal.is_some() {
        render_app_modal(f, app);
    }
}

fn render_app_task_list(f: &mut Frame, app: &TasksApp, area: Rect) {
    let header_cells = ["id", "pri", "name"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().add_modifier(Modifier::BOLD)));
    let header = Row::new(header_cells)
        .style(Style::default().bg(Color::DarkGray))
        .height(1);

    // Autosize the id column to the widest short id seen in the current view
    // (completed tasks show no id; minimum 2 chars so single-digit ids still
    // feel intentional).
    let id_width = app
        .tasks
        .iter()
        .filter(|t| !t.is_done())
        .map(|t| t.short_id.map(|s| s.to_string().len()).unwrap_or(0))
        .max()
        .unwrap_or(2)
        .max(2) as u16;

    let rows: Vec<Row> = app
        .tasks
        .iter()
        .map(|task| {
            let count = task.completions.unwrap_or(0) as i64;
            // Scheduled tasks carry their own badge semantics (ongoing /
            // completed / failed); everything else uses the count badge.
            let (ch, color) = if task.is_scheduled() {
                crate::views::scheduled_badge(
                    &app.config,
                    task.completions,
                    task.start_time,
                    task.available_duration_secs,
                    crate::date::now(),
                )
            } else {
                crate::views::completion_badge(&app.config, count, task.target_count)
            };
            let dot_style = if color == CtColor::Reset {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::from_crossterm(color))
                    .add_modifier(Modifier::BOLD)
            };

            // Per TODO: the badge sits before the title (not in all caps),
            // and target_count > 0 adds an m/n sub-line under the title. The
            // previous "type" + "status" columns are gone — type lives in the
            // preview as a dedicated field, and the badge itself carries the
            // completion semantics that used to live in status.
            let mut name_spans = vec![Span::styled(format!("{} ", ch), dot_style)];
            name_spans.push(Span::raw(task.name.clone()));
            let mut name_lines: Vec<Line> = vec![Line::from(name_spans)];
            if task.target_count > 0 {
                name_lines.push(Line::from(Span::styled(
                    format!("  {}/{}", count, task.target_count),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            // Completed tasks have no short id — the id column stays empty.
            let id_cell = if task.is_done() {
                String::new()
            } else {
                task.short_id
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            };
            let cells = vec![
                Cell::from(id_cell),
                Cell::from(task.priority.to_string())
                    .style(crate::render::priority_color(task.priority)),
                Cell::from(Text::from(name_lines)),
            ];
            // Reserve 2 rows when target > 0 so the m/n sub-line does not
            // visually overlap the next row.
            let row_height = if task.target_count > 0 { 2 } else { 1 };
            Row::new(cells).height(row_height)
        })
        .collect();

    let sort_indicator = if app.sort_by_due { "due" } else { "priority" };
    let mut title = format!("{} [sort: {}]", mode_label(app.mode), sort_indicator);
    if app.show_scheduled {
        title.push_str(" +scheduled");
    }
    if app.include_completed {
        title.push_str(" +completed");
    }

    // Column widths: id is autosized, pri is a small fixed column, name fills.
    let widths = vec![
        Constraint::Length(id_width),
        Constraint::Length(3),
        Constraint::Fill(1),
    ];

    let table = Table::new(rows, &widths)
        .header(header)
        .block(Block::bordered().title(title))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    let mut table_state = TableState::new().with_selected(if app.tasks.is_empty() {
        None
    } else {
        Some(app.selected)
    });
    f.render_stateful_widget(table, area, &mut table_state);
}

fn render_app_preview(f: &mut Frame, app: &TasksApp, area: Rect) {
    if app.tasks.is_empty() {
        let text = Text::from("No tasks selected");
        let paragraph = Paragraph::new(text)
            .block(Block::bordered().title("Preview"))
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
        return;
    }

    let task = &app.tasks[app.selected];
    let lines = build_preview(task);
    let paragraph = Paragraph::new(Text::from(lines))
        .block(Block::bordered().title("Preview"))
        .style(Style::default())
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

/// Label prefix for the count input in the Complete modal. The box is sized
/// exactly to this label plus the typed input, and the cursor is placed
/// right after both — keep this and the cursor math in [`render_app_modal`]
/// in sync.
const COUNT_LABEL: &str = "Count: ";

fn render_app_modal(f: &mut Frame, app: &TasksApp) {
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
        Modal::DeleteConfirm {
            name,
            is_recurring,
            cursor,
        } => {
            let mut lines = vec![Line::from(vec![
                Span::styled(
                    "Delete",
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::ITALIC),
                ),
                Span::raw(format!(" '{}'?", name)),
            ])];
            // Recurring tasks warn that deleting stops the recurrence.
            if *is_recurring {
                lines.push(Line::from(Span::styled(
                    "  This task will stop recurring!",
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
            lines.push(Line::from(""));
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

// ---------- Fetch helper ----------

/// Task rows for the current view mode; SQL lives in sql.rs (shared with
/// the CLI task view).
async fn fetch_tasks(
    pool: &SqlitePool,
    mode: ViewMode,
    include_completed: bool,
    show_scheduled: bool,
) -> Result<Vec<TaskRow>> {
    crate::sql::fetch_tasks_for_view(pool, mode, include_completed, show_scheduled).await
}
