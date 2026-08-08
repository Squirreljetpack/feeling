use ratatui::{
    layout::Alignment,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::date;
use crate::today::{EntryKind, TodayEntry};

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
        date::format_datetime(ts),
        Style::default().fg(Color::DarkGray),
    ))
    .alignment(Alignment::Right)
}

/// Build the preview pane lines for a task row. With `today`, the row is
/// a today-view entry row: for recurring tasks `end_time` carries the
/// unscoped last completion (not the expiry), so `last` is read from it
/// and the `ends` field is skipped. `preview.show_last_when_done` controls
/// whether done rows still show their `last` field (`last:` is otherwise
/// shown only while the task is not done).
///
/// The layout, top to bottom:
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
pub fn build_preview(
    task: &crate::db::TaskRow,
    today: bool,
    preview: &crate::config::PreviewConfig,
    linked_moods: &[crate::db::MoodRow],
    axes: Option<&crate::color::ColorAxes>,
) -> Vec<Line<'static>> {
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
    if !task.is_done()
        && let Some(short_id) = task.short_id {
            lines.push(field_line("id", short_id.to_string()));
        }
    lines.push(field_line("priority", task.priority.to_string()));
    // The last completion on the task. Done rows show it only when
    // `preview.show_last_when_done` is set (the default). Today-view
    // recurring rows carry the unscoped last completion in `end_time`
    // (their `last_time` is window-scoped); everywhere else `last_time` is
    // unscoped for recurring rows too.
    if !task.is_done() || preview.show_last_when_done {
        let last = if today && task.is_recurring() {
            task.end_time
        } else {
            task.last_time
        };
        if let Some(last) = last {
            lines.push(field_line("last", date::format_human_datetime(last)));
        }
    }

    if task.is_recurring() {
        // Recurring tasks show when the next interval opens instead of
        // a fixed start time.
        if let Some(st) = task.start_time {
            let now = date::now();
            let next = match task.interval_span() {
                Some(span) if crate::date::span_rough_seconds(span) > 0.0 => {
                    if now <= st {
                        st
                    } else {
                        // Next interval start = end of the current interval.
                        crate::date::interval_end_unix_secs(st, span, now).unwrap_or(st)
                    }
                }
                _ => st,
            };
            lines.push(field_line("next", date::format_human_datetime(next)));
        }
    } else if task.is_scheduled() {
        // Scheduled tasks show the window start.
        if let Some(st) = task.start_time {
            lines.push(field_line("start", date::format_human_datetime(st)));
        }
    } else {
        // Oneshot tasks: the creation time always, and the due time only
        // when one was set (`! name @<time>` → end_time).
        if let Some(st) = task.start_time {
            lines.push(field_line("creation", date::format_human_datetime(st)));
        }
        if let Some(et) = task.end_time {
            lines.push(field_line("due", date::format_human_datetime(et)));
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
        if let Some(span) = task.interval_span() {
            lines.push(field_line("interval", date::format_span(&span)));
        }
        if let Some(avail) = task.available_duration_secs {
            lines.push(field_line("duration", date::format_duration(avail)));
        }
        // Today-view rows carry the unscoped last completion in `end_time`
        // instead of the expiry — no `ends` field there.
        if !today
            && let Some(ref s) = task.end_datetime() {
                lines.push(field_line("ends", s.clone()));
            }
        // The optional flag is only shown when the task is skippable.
        if task.optional != 0 {
            lines.push(field_line("optional", "Yes".to_string()));
        }
    }

    // Linked moods (`im good -5` recorded the link): a `moods:` field
    // with one `  - {badge} {mood text}` line per linked mood. The badge
    // color comes from the sync mood-color pipeline (process-wide cache;
    // see `color::global_mood_color_cache`).
    if !linked_moods.is_empty() {
        lines.push(field_line("moods", String::new()));
        let embedder = crate::embedding::global_embedder();
        let mut cache = crate::color::global_mood_color_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for mood in linked_moods {
            // Journal-only rows (empty mood) have no badge to show.
            if mood.mood.is_empty() {
                continue;
            }
            let color = axes
                .and_then(|axes| axes.mood_color_cached(embedder, mood, &mut cache))
                .map(|oklab| {
                    let rgb = oklab.to_srgb();
                    Color::Rgb(rgb.r, rgb.g, rgb.b)
                })
                .unwrap_or(Color::DarkGray);
            lines.push(Line::from(vec![
                Span::raw("  - "),
                Span::styled("●", Style::default().fg(color)),
                Span::raw(format!(" {}", mood.mood)),
            ]));
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
/// "MOOD" (cyan, italic) when the entry carries a mood, "JOURNAL"
/// (gray) for moodless journal-only entries, "TRACKER" (dark gray) for
/// tracker entries. Journal-only entries skip the mood segment, showing
/// the body directly after the date.
pub(crate) fn build_today_preview(entry: &TodayEntry) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Start with a blank line so the heading reads as a heading.
    lines.push(Line::default());

    let (title, title_style): (String, Style) = match entry.kind {
        EntryKind::Mood => (
            "MOOD".to_string(),
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
        EntryKind::Tracker(_) => (
            "TRACKER".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        // Task entries normally render via build_preview (they carry a
        // selected TaskRow); this is the fallback for entries reaching here
        // without one.
        EntryKind::Task(_) => (
            "TASK".to_string(),
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        ),
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

    // Mood string (or tracker label), indented, white, italic.
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

    // Interval trackers show when the next interval opens — like recurring
    // tasks.
    if let Some((anchor, span)) = entry.tracker_interval
        && crate::date::span_rough_seconds(span) > 0.0 {
            let now = date::now();
            let next = if now <= anchor {
                anchor
            } else {
                // Next interval start = end of the current interval.
                crate::date::interval_end_unix_secs(anchor, span, now).unwrap_or(anchor)
            };
            lines.push(field_line("next", date::format_human_datetime(next)));
        }

    // `prev:` shows the previous entry of this kind whenever one exists.
    if let Some(prev) = entry.tracker_prev {
        lines.push(field_line("prev", date::format_human_datetime(prev)));
    }

    // Linked trackers and tasks (mood entries): a `linked:` field with one
    // `  - {tracker}: {payload}` line per attached tracker (the name in the
    // tracker's own color, matching the main `name: value` label format;
    // payload omitted when the tracker carries none) and one
    // `  - {badge} {task name}` line per linked task.
    if !entry.linked_trackers.is_empty() || !entry.linked_tasks.is_empty() {
        lines.push(field_line("linked", String::new()));
        for t in &entry.linked_trackers {
            let mut spans = vec![
                Span::raw("  - "),
                Span::styled(format!("{}:", t.name), Style::default().fg(t.color)),
            ];
            if !t.payload.is_empty() {
                spans.push(Span::raw(format!(" {}", t.payload)));
            }
            lines.push(Line::from(spans));
        }
        for t in &entry.linked_tasks {
            let mut spans = vec![Span::raw("  - ")];
            if let Some(badge) = t.badge {
                spans.push(Span::styled(
                    badge.to_string(),
                    Style::default().fg(t.color),
                ));
            }
            spans.push(Span::raw(format!(" {}", t.name)));
            lines.push(Line::from(spans));
        }
    }

    // Body: a blank line, then the text indented.
    if !entry.body.is_empty() {
        lines.push(Line::default());
        for line_str in entry.body.lines() {
            lines.push(Line::from(format!("  {}", line_str)));
        }
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recurring_row() -> crate::db::TaskRow {
        crate::db::TaskRow {
            id: 1,
            short_id: Some(7),
            name: "water plants".to_string(),
            body: String::new(),
            priority: 3,
            start_time: Some(1_700_000_000),
            available_duration_secs: Some(3600),
            interval_secs: Some(crate::date::span_to_db(&jiff::Span::new().days(1))),
            target_count: 0,
            optional: 0,
            end_time: Some(1_700_500_000),
            parent: None,
            completions: Some(0),
            last_time: Some(1_700_400_000),
        }
    }

    fn preview_config(show_last_when_done: bool) -> crate::config::PreviewConfig {
        crate::config::PreviewConfig {
            show_last_when_done,
        }
    }

    /// The values of the `field: value` lines, e.g. `["id: 7", "last: ..."]`.
    fn fields(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .filter_map(|l| {
                let text: String = l.spans.iter().map(|s| s.content.to_string()).collect();
                text.strip_prefix("  ").map(|s| s.trim().to_string())
            })
            .filter(|s| s.contains(':'))
            .collect()
    }

    #[test]
    fn test_build_preview_today_recurring_last_from_end_time() {
        let task = recurring_row();
        let lines = build_preview(&task, true, &preview_config(true), &[], None);
        let fields = fields(&lines);
        // `last` reads the unscoped completion carried in `end_time`, and
        // the `ends` field is skipped (end_time is not the expiry here).
        assert!(
            fields
                .iter()
                .any(|f| f == &format!("last: {}", date::format_human_datetime(1_700_500_000))),
            "expected last: from end_time, got {fields:?}"
        );
        assert!(!fields.iter().any(|f| f.starts_with("ends:")), "{fields:?}");
    }

    #[test]
    fn test_build_preview_not_today_recurring_last_from_last_time() {
        let task = recurring_row();
        let lines = build_preview(&task, false, &preview_config(true), &[], None);
        let fields = fields(&lines);
        assert!(
            fields
                .iter()
                .any(|f| f == &format!("last: {}", date::format_human_datetime(1_700_400_000))),
            "expected last: from last_time, got {fields:?}"
        );
        assert!(fields
            .iter()
            .any(|f| f == &format!("ends: {}", date::format_human_datetime(1_700_500_000))));
    }

    #[test]
    fn test_build_preview_done_shows_last() {
        let mut task = recurring_row();
        task.completions = Some(1); // target 0 -> done
        let fields = fields(&build_preview(
            &task,
            true,
            &preview_config(true),
            &[],
            None,
        ));
        assert!(
            fields
                .iter()
                .any(|f| f == &format!("last: {}", date::format_human_datetime(1_700_500_000))),
            "expected last: on a done row, got {fields:?}"
        );
    }

    /// A tracker entry preview shows `prev:` (the previous entry of this
    /// kind, human-formatted) when one exists, and no `prev:`/`last:` field
    /// at all otherwise.
    #[test]
    fn test_build_today_preview_prev() {
        let mk = |tracker_prev: Option<i64>| TodayEntry {
            id: Some(1),
            time: 1_700_000_000,
            time_label: "18:00".to_string(),
            kind: EntryKind::Tracker(crate::config::TrackerKind::Float),
            label: "sleep: 7.5".to_string(),
            body: String::new(),
            task_id: None,
            priority: 0,
            badge: Some('◆'),
            color: Color::DarkGray,
            recurring_window: None,
            tracker_interval: None,
            tracker_prev,
            linked_trackers: Vec::new(),
            linked_tasks: Vec::new(),
        };
        let rendered: Vec<String> = build_today_preview(&mk(Some(1_699_000_000)))
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert!(
            rendered
                .iter()
                .any(|l| l == &format!("  prev: {}", date::format_human_datetime(1_699_000_000))),
            "expected a prev: field, got {rendered:?}"
        );
        assert!(
            !rendered.iter().any(|l| l.contains("last:")),
            "{rendered:?}"
        );

        let rendered: Vec<String> = build_today_preview(&mk(None))
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert!(
            !rendered.iter().any(|l| l.trim_start().starts_with("prev:")),
            "expected no prev: field, got {rendered:?}"
        );
    }

    /// A mood entry with attached trackers and linked tasks shows a
    /// `linked:` field with one `  - {tracker} {payload}` line per tracker
    /// (name in the tracker's color) and one `  - {badge} {task name}` line
    /// per task; null-tracker payloads are omitted.
    #[test]
    fn test_build_today_preview_linked() {
        use crate::today::{LinkedTask, LinkedTracker};
        let entry = TodayEntry {
            id: Some(1),
            time: 1_700_000_000,
            time_label: "18:00".to_string(),
            kind: EntryKind::Mood,
            label: "good".to_string(),
            body: String::new(),
            task_id: None,
            priority: 0,
            badge: Some('●'),
            color: Color::DarkGray,
            recurring_window: None,
            tracker_interval: None,
            tracker_prev: None,
            linked_trackers: vec![
                LinkedTracker {
                    name: "sleep".to_string(),
                    payload: "7.5".to_string(),
                    color: Color::LightBlue,
                },
                // Null trackers carry the entry moment as their payload.
                LinkedTracker {
                    name: "sitting".to_string(),
                    payload: "3-15 14:30".to_string(),
                    color: Color::LightYellow,
                },
            ],
            linked_tasks: vec![LinkedTask {
                badge: Some('✓'),
                color: Color::Green,
                name: "water plants".to_string(),
            }],
        };
        let rendered: Vec<String> = build_today_preview(&entry)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert!(
            rendered
                .iter()
                .any(|l| l.trim_start().starts_with("linked:")),
            "expected a linked: field, got {rendered:?}"
        );
        assert!(
            rendered.iter().any(|l| l == "  - sleep: 7.5"),
            "expected a '  - sleep: 7.5' line, got {rendered:?}"
        );
        assert!(
            rendered.iter().any(|l| l == "  - sitting: 3-15 14:30"),
            "expected a '  - sitting: 3-15 14:30' line, got {rendered:?}"
        );
        assert!(
            rendered.iter().any(|l| l == "  - ✓ water plants"),
            "expected a '  - ✓ water plants' line, got {rendered:?}"
        );
    }

    /// A task with linked moods shows a `moods:` field with one
    /// `  - ● mood` line per linked mood (empty-mood journal rows are
    /// skipped).
    #[test]
    fn test_build_preview_linked_moods() {
        let task = recurring_row();
        let mood = crate::db::MoodRow {
            id: 1,
            mood: "good".to_string(),
            body: String::new(),
            time: 1_700_000_000,
            embedding: None,
            score: None,
        };
        let lines = build_preview(&task, true, &preview_config(true), &[mood], None);
        let rendered: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert!(
            rendered
                .iter()
                .any(|l| l.trim_start().starts_with("moods:")),
            "expected a moods: field, got {rendered:?}"
        );
        assert!(
            rendered.iter().any(|l| l == "  - ● good"),
            "expected a '  - ● good' line, got {rendered:?}"
        );
    }
}
