use ratatui::{
    layout::Alignment,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::date;
use crate::views::{EntryKind, TodayEntry};

/// A `  field: value` line: the field name (with colon) in yellow, the
/// value uncolored. Field names are lowercase.
fn field_line(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {}: ", label), Style::default().fg(Color::Yellow)),
        Span::raw(value),
    ])
}

/// The entry's timestamp, right-aligned and dark gray.
fn date_line(ts: i64) -> Line<'static> {
    Line::from(Span::styled(
        date::format_date_time(ts),
        Style::default().fg(Color::DarkGray),
    ))
    .alignment(Alignment::Right)
}

/// Build the preview pane for a task row. The layout, top to bottom:
///
/// - a blank line, then the heading: the type name ("Task" / "Recurring"
///   / "Scheduled", full caps, bold) in its own color, indented one space,
///   over a dark-grey rule as wide as the title plus two;
/// - the task name, indented, white, italic;
/// - a blank line, then the fields (`id`, `priority`, `creation`/`due` for
///   oneshot, `start` for scheduled, and the recurring metadata (`next`,
///   `interval`, `duration`, plus `ends`/`optional` when set / `duration`,
///   `state`) as `field: value` lines with yellow lowercase field names;
/// - the progress bar for counted tasks (a blank line on each side), then
///   the body when nonempty (a blank line, then the body indented two
///   spaces).
pub fn build_preview(task: &crate::sql::TaskRow) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Start with a blank line so the heading reads as a heading.
    lines.push(Line::default());

    let (title, title_color) = if task.is_recurring() {
        ("RECURRING", Color::Blue)
    } else if task.is_scheduled() {
        ("SCHEDULED", Color::LightRed)
    } else {
        ("TASK", Color::Yellow)
    };
    lines.push(Line::from(Span::styled(
        format!(" {}", title),
        Style::default()
            .fg(title_color)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        "─".repeat(title.len() + 2),
        Style::default().fg(Color::DarkGray),
    )));

    // Task name, indented, white, italic.
    lines.push(Line::from(Span::styled(
        format!("  {}", task.name),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::ITALIC),
    )));

    // Blank line, then the fields.
    lines.push(Line::default());
    // The short id is shown only while the task is not completed — a
    // completed task's short id is cleared, so the ID field disappears.
    if !task.is_done() {
        if let Some(short_id) = task.short_id {
            lines.push(field_line("id", short_id.to_string()));
        }
    }
    lines.push(field_line("priority", task.priority.to_string()));
    if task.is_recurring() {
        // Recurring tasks show when the next interval opens instead of
        // a fixed start time.
        if let Some(st) = task.start_time {
            let now = date::now();
            let next = match task.interval_secs {
                Some(interval) if interval > 0 => {
                    if now <= st {
                        st
                    } else {
                        st + ((now - st) / interval + 1) * interval
                    }
                }
                _ => st,
            };
            lines.push(field_line("next", date::format_date_time(next)));
        }
    } else if task.is_scheduled() {
        // Scheduled tasks show the window start.
        if let Some(st) = task.start_time {
            lines.push(field_line("start", date::format_date_time(st)));
        }
    } else {
        // Oneshot tasks: the creation time always, and the due time only
        // when one was set (`! name @<time>` → end_time).
        if let Some(st) = task.start_time {
            lines.push(field_line("creation", date::format_date_time(st)));
        }
        if let Some(et) = task.end_time {
            lines.push(field_line("due", date::format_date_time(et)));
        }
    }

    // Scheduled window: the availability duration and the current state
    // (ongoing / completed / auto-completed / failed).
    if task.is_scheduled() {
        if let Some(avail) = task.available_duration_secs {
            lines.push(field_line("duration", date::format_duration(avail)));
        }
        let now = date::now();
        let state = match task.completions {
            Some(c) if c > 0 => "completed",
            Some(_) => "failed",
            None => {
                let elapsed = task.start_time.unwrap_or(now)
                    + task.available_duration_secs.unwrap_or(0)
                    < now;
                if elapsed {
                    "auto-completed"
                } else {
                    "ongoing"
                }
            }
        };
        lines.push(field_line("state", state.to_string()));
    }

    // Recurring metadata: interval, availability window, end, optional.
    if task.is_recurring() {
        if let Some(interval) = task.interval_secs {
            lines.push(field_line("interval", date::format_duration(interval)));
        }
        if let Some(avail) = task.available_duration_secs {
            lines.push(field_line("duration", date::format_duration(avail)));
        }
        if let Some(ref s) = task.end_datetime() {
            lines.push(field_line("ends", s.clone()));
        }
        // The optional flag is only shown when the task is skippable.
        if task.optional != 0 {
            lines.push(field_line("optional", "Yes".to_string()));
        }
    }

    // Progress bar for counted tasks: after the fields and above the
    // body, with a blank line on each side.
    if task.target_count > 0 {
        lines.push(Line::default());
        let done = task.completions.unwrap_or(0);
        let target = task.target_count;
        let bar_width = 20usize;
        let filled = ((done as f64 / target as f64) * bar_width as f64).round() as usize;
        let filled = filled.min(bar_width);
        let empty = bar_width.saturating_sub(filled);
        let bar = format!(
            "  [{}{}] {}/{}",
            "█".repeat(filled),
            "░".repeat(empty),
            done,
            target
        );
        lines.push(Line::from(Span::styled(
            bar,
            Style::default().fg(if done >= target {
                Color::Green
            } else {
                Color::White
            }),
        )));
    }

    // Body: a blank line, then the text indented.
    if !task.body.is_empty() {
        lines.push(Line::default());
        for line_str in task.body.lines() {
            lines.push(Line::from(format!("  {}", line_str)));
        }
    }

    lines
}

/// Build the preview pane for a today-view entry. Same heading shape as
/// [`build_preview`], titled after the entry type in full caps and bold:
/// "FEELING" (cyan, italic) when the entry carries a mood, "JOURNAL"
/// (gray) for moodless journal-only entries, "CUSTOM" (dark gray) for
/// tracker entries. Journal-only entries skip the mood segment, showing
/// the body directly after the date.
pub(crate) fn build_today_preview(entry: &TodayEntry) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Start with a blank line so the heading reads as a heading.
    lines.push(Line::default());

    let (title, title_style): (String, Style) = match entry.kind {
        EntryKind::Mood => (
            "FEELING".to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        EntryKind::Journal => (
            "JOURNAL".to_string(),
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        ),
        EntryKind::Custom => (
            "CUSTOM".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        // Task entries normally render via build_preview (they carry a
        // selected TaskRow); this is the fallback for entries reaching here
        // without one.
        EntryKind::Oneshot | EntryKind::Threshold | EntryKind::Recurring | EntryKind::Scheduled => {
            (
                "TASK".to_string(),
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            )
        }
    };
    lines.push(Line::from(Span::styled(format!(" {}", title), title_style)));
    lines.push(Line::from(Span::styled(
        "─".repeat(title.chars().count() + 2),
        Style::default().fg(Color::DarkGray),
    )));

    if entry.kind == EntryKind::Journal {
        // Journal-only: skip the mood segment — the date, then the body
        // directly.
        lines.push(date_line(entry.time));
        for line_str in entry.body.lines() {
            lines.push(Line::from(format!("  {}", line_str)));
        }
        return lines;
    }

    // Mood string (or custom label), indented, white, italic.
    if !entry.label.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  {}", entry.label),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // Date after the name, right-aligned and dark gray.
    lines.push(date_line(entry.time));

    // Body: a blank line, then the text indented.
    if !entry.body.is_empty() {
        lines.push(Line::default());
        for line_str in entry.body.lines() {
            lines.push(Line::from(format!("  {}", line_str)));
        }
    }

    lines
}
