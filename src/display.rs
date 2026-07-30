use crate::config::Config;
use crate::sql::TaskRow;
use crate::views::TodayEntry;
use anyhow::Result;

/// Plain banner line for an interactive task flow (replaces the cliclack
/// `intro` call — plain stdout, no styling).
pub fn task_intro(title: &str) -> Result<()> {
    println!("{}", title);
    Ok(())
}

/// Display a created task: a summary header line followed by field/value
/// rows. Field names and values are separated by a tab with the labels
/// padded to a fixed width, so all values line up vertically.
pub fn display_task(task: &crate::sql::TaskObject) -> Result<()> {
    let kind = if task.is_recurring() {
        "recurring"
    } else if task.is_scheduled() {
        "scheduled"
    } else {
        "oneshot"
    };
    println!(
        "Created {} task #{}: {}",
        kind,
        task.short_id.unwrap_or_default(),
        task.name
    );
    print_rows(&task_rows(task));
    Ok(())
}

/// Display a logged entry: mood, custom trackers, and body as field/value
/// rows (tab-separated, vertically aligned).
pub fn display_entry(entry: &crate::sql::EntryObject) -> Result<()> {
    let mut rows: Vec<(String, String)> = Vec::new();
    if !entry.mood.is_empty() {
        rows.push(("Feeling".to_string(), entry.mood.clone()));
    }
    for custom in &entry.customs {
        rows.push((custom.tracker_type.clone(), custom.value.to_string()));
    }
    if !entry.body.is_empty() {
        rows.push(("Body".to_string(), entry.body.clone()));
    }
    print_rows(&rows);
    Ok(())
}

/// Ordered field/value pairs for a task's tab-aligned display.
/// Recurring-only fields (Interval, Available, Optional, End) are shown only
/// for recurring tasks; scheduled tasks show their Available window; Start
/// is shown when set; Body only when non-empty.
fn task_rows(task: &crate::sql::TaskObject) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    rows.push((
        "Type".to_string(),
        if task.is_recurring() {
            "Recurring".to_string()
        } else if task.is_scheduled() {
            "Scheduled".to_string()
        } else {
            "OneShot".to_string()
        },
    ));
    rows.push(("Priority".to_string(), task.priority.to_string()));
    if let Some(st) = task.start_time {
        rows.push(("Start".to_string(), crate::date::format_date_time(st)));
    }
    if task.is_recurring() {
        rows.push((
            "Interval".to_string(),
            crate::date::format_duration(task.interval_secs.unwrap_or_default()),
        ));
        rows.push((
            "Available".to_string(),
            match task.available_duration_secs {
                Some(a) => crate::date::format_duration(a),
                None => "Always".to_string(),
            },
        ));
        rows.push((
            "Optional".to_string(),
            if task.optional { "Yes" } else { "No" }.to_string(),
        ));
        rows.push((
            "End".to_string(),
            match task.end_time {
                Some(e) => crate::date::format_date_time(e),
                None => "Never".to_string(),
            },
        ));
    } else if task.is_scheduled() {
        rows.push((
            "Available".to_string(),
            task.available_duration_secs
                .map(crate::date::format_duration)
                .unwrap_or_default(),
        ));
    }
    rows.push(("Target".to_string(), task.target_count.to_string()));
    if !task.body.is_empty() {
        rows.push(("Body".to_string(), task.body.clone()));
    }
    rows
}

/// Print tab-aligned field/value rows. Labels are padded to a fixed width so
/// every value starts at the same column regardless of label length.
fn print_rows(rows: &[(String, String)]) {
    for (label, value) in rows {
        println!("{label:<14}\t{value}");
    }
}

/// Format today view entries into tab-separated output text.
pub fn format_today_simple(entries: &[TodayEntry]) -> String {
    use crossterm::style::{Color as CtColor, Stylize};
    use ratatui::backend::IntoCrossterm;

    let mut output = String::new();
    for entry in entries {
        let ts = crate::date::format_time(entry.time);

        // Journal-only entries (no mood) carry the body as the label.
        let (label, detail) = if entry.entry_type == "feeling" && entry.label.is_empty() {
            (entry.body.to_string(), String::new())
        } else {
            (entry.label.clone(), entry.body.clone())
        };

        // Same badge as the TUI: marker glyph colored with the entry's dot
        // color. Reset-colored badges (e.g. 0% tasks) stay plain.
        let color = entry.color.into_crossterm();
        let badge = if color == CtColor::Reset {
            entry.badge.to_string()
        } else {
            entry.badge.to_string().with(color).to_string()
        };

        output.push_str(&format!("{}\t{}\t{}\t{}\n", ts, badge, label, detail));
    }
    output
}

/// Format task view rows into tab-separated output text.
///
/// 6 columns: `id \t interval \t next_available \t pri \t name \t status`.
/// Recurring tasks fill `interval` (`format_duration`) and `next_available`
/// (the next interval window start, `format_date_time`); oneshot tasks render
/// a single space in both.
pub fn format_tasks_simple(tasks: &[TaskRow], config: &Config) -> String {
    use crossterm::style::{Color as CtColor, Stylize};

    let mut output = String::new();
    for task in tasks {
        let count = task.completions.unwrap_or(0) as i64;
        let (ch, color) = crate::views::completion_badge(config, count, task.target_count);
        // Same badge as the TUI preview: colored dot + plain label.
        let dot = if color == CtColor::Reset {
            ch.to_string()
        } else {
            ch.to_string().with(color).to_string()
        };
        let label = crate::views::completion_badge_text(count, task.target_count);
        let label = label.strip_prefix(ch).unwrap_or("").to_string();
        // Completed tasks have no short id — the id column stays empty.
        let id_cell = if task.is_done() {
            String::new()
        } else {
            task.short_id.map(|s| s.to_string()).unwrap_or_default()
        };
        // Recurring tasks show their interval and the next time they become
        // available (the start of the next interval window); oneshot tasks
        // render a single space in both columns.
        let interval_cell = task
            .interval_secs
            .map(crate::date::format_duration)
            .unwrap_or_else(|| " ".to_string());
        let next_available_cell = match (task.start_time, task.interval_secs) {
            (Some(start), Some(interval)) if interval > 0 => {
                let now = crate::date::now();
                let next = if now <= start {
                    start
                } else {
                    start + ((now - start) / interval + 1) * interval
                };
                crate::date::format_date_time(next)
            }
            _ => " ".to_string(),
        };
        output.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}{}\n",
            id_cell, interval_cell, next_available_cell, task.priority, task.name, dot, label,
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use crate::sql::TaskObject;

    use super::*;

    #[test]
    fn test_display_task_runs() {
        let task = TaskObject {
            id: Some(1),
            short_id: Some(1),
            name: "test task".to_string(),
            body: "test body".to_string(),
            priority: 5,
            start_time: Some(1700000000),
            available_duration_secs: None,
            interval_secs: None,
            target_count: 0,
            optional: false,
            end_time: None,
        };
        assert!(display_task(&task).is_ok());
    }
}
