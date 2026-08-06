//! Badge system: a badge is a `(glyph, color)` pair rendered next to a row.
//! Every row type in the app — tasks (TUI tasks view, CLI task lists) and
//! today-view entries — derives its badge from the rules in `docs/BADGE.md`
//! (the spec of record).
//!
//! [`task_badge`] is the single entry point for task rows. `done_view` is
//! true when rendering a done-list (`@done` tasks view / CLI `@done` list):
//! the done-state glyph stays `◷` / `↻` for scheduled / recurring rows
//! instead of switching to `✓`. It has no effect on oneshot | threshold rows
//! (always `✓` when done).
//!
//! [`completion_badge`] / [`completion_badge_text`] are the tracker-grid /
//! progress-text helpers (unchanged semantics): per-interval dot rows in the
//! `:trackers`/mood grids and the "2/5" progress label.

use crossterm::style::Color as CtColor;

use crate::config::Config;
use crate::sql::TaskRow;

/// Completion badge: (character, color) for a task's completion status.
///
/// - 0% (no entries, or per-interval sum 0) → ('◯', Reset), uncolored
/// - 100% (count >= target_count; any count when target_count <= 0) → ('●', last color)
/// - in between → ('●', binned into colors[..len-1] so the last color is
///   reserved exclusively for 100% completion). Binning only, no blending.
pub fn completion_badge(config: &Config, count: i64, target_count: i32) -> (char, CtColor) {
    let colors = &config.tasks.colors;
    if count <= 0 {
        return ('◯', CtColor::Reset);
    }
    if target_count <= 0 || count >= target_count as i64 {
        return ('●', *colors.last().unwrap());
    }
    // 0 < count < target_count: bin across colors[..len-1]
    if colors.len() <= 1 {
        return ('●', *colors.first().unwrap());
    }
    let n = colors.len() - 1;
    let t = count as f64 / target_count as f64;
    let idx = ((t * n as f64).round() as usize).min(n - 1);
    ('●', colors[idx])
}

/// Text form of the completion badge: "● 2/5" (in progress), "●" alone (100%,
/// regardless of target_count), or "◯" alone (0%). Never shows "n/m" when
/// target_count <= 0. The 100% case dropped the "DONE" word per TODO; the
/// leading character matches `completion_badge`.
pub fn completion_badge_text(count: i64, target_count: i32) -> String {
    let ch = if count <= 0 { '◯' } else { '●' };
    if count > 0 && (target_count <= 0 || count >= target_count as i64) {
        ch.to_string()
    } else if count > 0 {
        // 0 < count < target_count (target_count > 0 here)
        format!("{} {}/{}", ch, count, target_count)
    } else {
        ch.to_string()
    }
}

/// Completion color of a count, binned across `colors` (binning only, no
/// blending; the last color is reserved for 100%): 0 → `colors[0]` (missed),
/// done (count >= target_count) → last color, partial → binned across
/// `colors[..len-1]`.
fn count_color(count: i64, target_count: i32, colors: &[CtColor]) -> CtColor {
    if count <= 0 {
        return *colors.first().unwrap_or(&CtColor::Reset);
    }
    if target_count <= 0 || count >= target_count as i64 {
        return *colors.last().unwrap_or(&CtColor::Reset);
    }
    if colors.len() <= 1 {
        return *colors.first().unwrap_or(&CtColor::Reset);
    }
    let n = colors.len() - 1;
    let t = count as f64 / target_count as f64;
    let idx = ((t * n as f64).round() as usize).min(n - 1);
    colors[idx]
}

/// Oneshot | Threshold badge. Four branches — don't combine them:
///
/// - done (`completions >= target_count`) → `✓` + last color
/// - not done, overdue (`end_time` set && `now > end_time`) → `○` +
///   completion color of count (0 → colors[0])
/// - not done, not overdue, zero entries → `○` + Reset
/// - not done, not overdue, partial → `○` + completion color of count
///   (0 → colors[0])
///
/// Undated tasks (no `end_time`) are never overdue.
fn oneshot_badge(task: &TaskRow, config: &Config, now: i64) -> (char, CtColor) {
    let colors = &config.tasks.colors;
    let count = task.completions.unwrap_or(0) as i64;
    if crate::task::is_task_done(task.target_count, task.completions) {
        return ('✓', *colors.last().unwrap_or(&CtColor::Reset));
    }
    if task.end_time.is_some_and(|end| now > end) {
        // Overdue: colored by count (0 → colors[0]).
        return ('○', count_color(count, task.target_count, colors));
    }
    if count == 0 {
        return ('○', CtColor::Reset);
    }
    ('○', count_color(count, task.target_count, colors))
}

/// Recurring badge. The glyph is `✓` when done (`↻` when `done_view`),
/// `↻` always otherwise.
///
/// | State | Color |
/// | --- | --- |
/// | done in current interval (`completions >= target_count`) | last `cN` |
/// | expired (`end_time` set && `now > end_time`) | `DarkGray` |
/// | availability passed, optional | `Reset` |
/// | availability passed, non-optional | binned (0 → colors[0]) |
/// | during availability, zero entries | `Reset` |
/// | during availability, partial | binned |
fn recurring_badge(task: &TaskRow, config: &Config, done_view: bool, now: i64) -> (char, CtColor) {
    let colors = &config.tasks.colors;
    let count = task.completions.unwrap_or(0) as i64;
    if crate::task::is_task_done(task.target_count, task.completions) {
        return (
            if done_view { '↻' } else { '✓' },
            *colors.last().unwrap_or(&CtColor::Reset),
        );
    }
    let expired = task.end_time.is_some_and(|end| now > end);
    let availability_passed = crate::task::availability_passed(task, now);
    let color = if expired {
        CtColor::DarkGrey
    } else if availability_passed {
        if task.optional != 0 {
            CtColor::Reset
        } else {
            // Non-optional window elapsed: missed (0 → colors[0]) or binned.
            count_color(count, task.target_count, colors)
        }
    } else if count == 0 {
        // During availability, zero entries.
        CtColor::Reset
    } else {
        // During availability, partial.
        count_color(count, task.target_count, colors)
    };
    ('↻', color)
}

/// Scheduled badge. Done (entry `>= 1`, or no entry with the window
/// elapsed — auto-completed) → `✓` (`◷` when `done_view`) + last color;
/// failed (entry 0, window open OR closed — the two branches are kept
/// separate, don't combine them) → `◷` + colors[0]; ongoing (no entry,
/// window still open) → `◷` + Reset.
fn scheduled_badge(task: &TaskRow, config: &Config, done_view: bool, now: i64) -> (char, CtColor) {
    let colors = &config.tasks.colors;
    let auto_completed = task.completions.is_none()
        && task
            .available_duration_secs
            .is_some_and(|dur| task.start_time.unwrap_or(now) + dur < now);
    let done = task.completions.is_some_and(|c| c > 0) || auto_completed;
    let color = if done {
        *colors.last().unwrap_or(&CtColor::Reset)
    } else if task.completions == Some(0) {
        *colors.first().unwrap_or(&CtColor::Reset)
    } else {
        CtColor::Reset
    };
    let glyph = if done && !done_view { '✓' } else { '◷' };
    (glyph, color)
}

/// Badge glyph + color for a task row, shared by the tasks view (TUI + CLI)
/// and the today view. Rules per task kind live in `docs/BADGE.md`.
///
/// `done_view` switches the done-state glyph for scheduled (`✓` → `◷`) and
/// recurring (`✓` → `↻`) rows; oneshot | threshold rows are unaffected
/// (always `✓` when done).
pub fn task_badge(task: &TaskRow, config: &Config, done_view: bool) -> (char, CtColor) {
    let now = crate::date::now();
    if task.is_recurring() {
        recurring_badge(task, config, done_view, now)
    } else if task.is_scheduled() {
        scheduled_badge(task, config, done_view, now)
    } else {
        oneshot_badge(task, config, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_row(completions: Option<i32>) -> TaskRow {
        TaskRow {
            id: 1,
            short_id: Some(1),
            name: "t".to_string(),
            body: String::new(),
            priority: 5,
            start_time: Some(1_000_000),
            available_duration_secs: None,
            interval_secs: None,
            target_count: 0,
            optional: 0,
            end_time: None,
            completions,
            last_time: None,
        }
    }

    #[test]
    fn test_oneshot_done_is_check() {
        let config = Config::default();
        let last = *config.tasks.colors.last().unwrap();
        let t = task_row(Some(1));
        assert_eq!(task_badge(&t, &config, false), ('✓', last));
        // done_view has no effect on oneshot rows.
        assert_eq!(task_badge(&t, &config, true), ('✓', last));
    }

    #[test]
    fn test_oneshot_not_done_not_overdue() {
        let config = Config::default();
        let t = task_row(None);
        assert_eq!(task_badge(&t, &config, false), ('○', CtColor::Reset));
    }

    #[test]
    fn test_oneshot_overdue_colored_by_count() {
        let config = Config::default();
        let mut t = task_row(None);
        t.end_time = Some(0); // far in the past
                              // Zero entries overdue → colors[0].
        assert_eq!(
            task_badge(&t, &config, false),
            ('○', config.tasks.colors[0])
        );
        // Partial overdue → binned.
        t.completions = Some(2);
        t.target_count = 10;
        let (ch, color) = task_badge(&t, &config, false);
        assert_eq!(ch, '○');
        assert!(color != CtColor::Reset);
        assert!(color != *config.tasks.colors.last().unwrap());
    }

    /// Recurring badge availability-window regression: the window is
    /// anchored to the current interval, so an old chain origin must not
    /// make "availability passed" permanently true (the absolute
    /// `start + duration <= now` formula did). `task_badge` reads the real
    /// clock, so all fixtures are built relative to it.
    #[test]
    fn test_recurring_not_done_availability_window() {
        let config = Config::default();
        let day = 86_400;
        let hour = 3600;
        let now = crate::date::now();
        let row =
            |st: i64, dur: i64, target: i32, optional: i32, completions: Option<i32>| TaskRow {
                id: 1,
                short_id: Some(1),
                name: "r".to_string(),
                body: String::new(),
                priority: 5,
                start_time: Some(st),
                available_duration_secs: Some(dur),
                interval_secs: Some(day),
                target_count: target,
                optional,
                end_time: None,
                completions,
                last_time: None,
            };

        // Old chain origin (60 days ago), window open in the current
        // interval (30min into a 1h window), zero entries → Reset (not
        // binned as "missed" — the absolute formula marked it passed
        // forever).
        let old = now - 60 * day - 1800;
        assert_eq!(
            task_badge(&row(old, hour, 0, 0, None), &config, false),
            ('↻', CtColor::Reset)
        );
        // Inside the window, partial → binned.
        let partial = task_badge(&row(now - 1800, hour, 2, 0, Some(1)), &config, false);
        assert_eq!(partial.0, '↻');
        assert_ne!(partial.1, CtColor::Reset, "partial inside window is binned");

        // Window passed (ended 2h ago), zero entries, non-optional → missed
        // (colors[0]); optional → Reset.
        assert_eq!(
            task_badge(&row(now - 3 * hour, hour, 0, 0, None), &config, false),
            ('↻', config.tasks.colors[0])
        );
        assert_eq!(
            task_badge(&row(now - 3 * hour, hour, 0, 1, None), &config, false),
            ('↻', CtColor::Reset)
        );

        // Expired → DarkGrey regardless of window state.
        let mut expired = row(old, hour, 0, 0, None);
        expired.end_time = Some(now - 100);
        assert_eq!(
            task_badge(&expired, &config, false),
            ('↻', CtColor::DarkGrey)
        );
    }
}
