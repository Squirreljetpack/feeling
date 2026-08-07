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
//! `:trackers`/mood grids and the "2/5" progress label. [`tracker_color`]
//! colors numeric tracker score dots in both the today view and the grids.

use crossterm::style::Color as CtColor;

use crate::config::{Config, TrackerSetting};
use crate::db::TaskRow;

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

/// Tracker dot color: bin `score` onto the tracker palette given the
/// effective min/max endpoints. Grid semantics (the grid view derives the
/// endpoints via `effective_range` with data fallback; the today view
/// passes the tracker's configured `min`/`max` directly):
/// - both endpoints with a non-degenerate range → linear binning across the
///   palette (inverted ranges: `max < min` maps lower scores to the last
///   color);
/// - only a lower bound → binary: below it the first color, at/above it the
///   last (success) color;
/// - only an upper bound → binary: above it the first color, at/below it the
///   last (success) color;
/// - no usable range (neither bound, or degenerate `min == max`) → the middle
///   palette color, rounded down (`colors.len() >= 2`).
pub(crate) fn tracker_color(
    colors: &[CtColor],
    score: f64,
    eff_min: Option<f64>,
    eff_max: Option<f64>,
) -> CtColor {
    match (eff_min, eff_max) {
        (Some(min), Some(max)) if (max - min).abs() > f64::EPSILON => {
            let t = if min < max {
                // normal: higher score → success
                ((score - min) / (max - min)).clamp(0.0, 1.0)
            } else {
                // Inverted range (min > max): lower score → success
                ((min - score) / (min - max)).clamp(0.0, 1.0)
            };
            let idx = ((t * (colors.len() as f64 - 1.0)).round() as usize).min(colors.len() - 1);
            colors[idx]
        }
        (Some(min), None) => {
            if score < min {
                colors[0]
            } else {
                *colors.last().unwrap()
            }
        }
        (None, Some(max)) => {
            if score > max {
                colors[0]
            } else {
                *colors.last().unwrap()
            }
        }
        _ => colors[(colors.len() - 1) / 2],
    }
}

/// Color for a Null tracker entry.
///
/// With an interval and **both** min/max set, the entry is a time marker:
/// min/max are seconds-from-interval-start/end offsets that split the
/// interval into a "red" zone (everything except the tail just before `min`)
/// and a "blue" zone. Per the TODO spec (see NOTES.md for the full
/// interpretation of the ambiguous example):
///
/// - `max` is measured from the span end: effective `max' = interval_end - max`;
/// - cycle-back point `mid = (min + max') / 2`;
/// - blue (last palette color) for times in `[mid, min)` circular;
/// - red (first palette color) everywhere else.
///
/// So a sleep tracker with `min = 23:00`, `max = 2h` on a midnight-anchored
/// day colors 23:00→22:30 red and 22:30→23:00 blue: 1pm is red, and only the
/// half-hour before 23:00 is blue.
///
/// With a single bound (or none — count mode), the score is binned like any
/// numeric tracker. Without an interval the tracker is unsupported → Reset.
pub(crate) fn null_tracker_color(
    colors: &[CtColor],
    tracker: &TrackerSetting,
    time: i64,
    score: f64,
) -> CtColor {
    let (Some(min), Some(max), Some(interval)) = (tracker.min, tracker.max, tracker.interval)
    else {
        // No interval → unsupported. Single-bound (or no-bound) → the score
        // is a count; bin it like a numeric tracker.
        return tracker_color(colors, score, tracker.min, tracker.max);
    };
    let Some((interval_start, interval_end)) =
        crate::date::interval_slot_unix_secs(interval.anchor, interval.span, time)
    else {
        return tracker_color(colors, score, tracker.min, tracker.max);
    };
    let len = (interval_end - interval_start) as f64;
    if len <= 0.0 {
        return tracker_color(colors, score, tracker.min, tracker.max);
    }
    let max_prime = len - max;
    let mid = (min.min(max_prime) + min.max(max_prime)) / 2.0;
    // Circular position of `time` within the interval, measured from the
    // interval start.
    let pos = (time - interval_start) as f64;
    // In the blue zone `[mid, min)` circular?
    let blue = ((pos - mid).rem_euclid(len)) < ((min - mid).rem_euclid(len));
    if blue {
        *colors.last().unwrap_or(&CtColor::Reset)
    } else {
        *colors.first().unwrap_or(&CtColor::Reset)
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

/// Recurring badge color by state (shared by the per-task and per-window
/// forms): expired → dark grey; availability passed → Reset for optional
/// tasks, else the count bin; during availability zero entries → Reset;
/// partial → the count bin.
fn recurring_badge_color(
    count: i64,
    target_count: i32,
    expired: bool,
    availability_passed: bool,
    optional: i32,
    colors: &[CtColor],
) -> CtColor {
    if expired {
        CtColor::DarkGrey
    } else if availability_passed {
        if optional != 0 {
            CtColor::Reset
        } else {
            // Non-optional window elapsed: missed (0 → colors[0]) or binned.
            count_color(count, target_count, colors)
        }
    } else if count == 0 {
        // During availability, zero entries.
        CtColor::Reset
    } else {
        // During availability, partial.
        count_color(count, target_count, colors)
    }
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
    let color = recurring_badge_color(
        count,
        task.target_count,
        expired,
        availability_passed,
        task.optional,
        colors,
    );
    ('↻', color)
}

/// Per-window recurring badge for the today view (one row per availability
/// window): the window is done (`is_task_done` on the window-scoped row) →
/// `✓`; the window has passed (`now >=
/// window_end`) → Reset for optional tasks, else the count bin; during an
/// open window zero entries → Reset, partial → the count bin.
pub fn recurring_window_badge(
    task: &TaskRow,
    window_end: i64,
    config: &Config,
    now: i64,
) -> (char, CtColor) {
    let colors = &config.tasks.colors;
    let count = task.completions.unwrap_or(0) as i64;
    if crate::task::is_task_done(task.target_count, task.completions) {
        return ('✓', *colors.last().unwrap_or(&CtColor::Reset));
    }
    // No expired state: today-view window rows carry the task's unscoped
    // last completion in `end_time`, not the expiry.
    let color = recurring_badge_color(
        count,
        task.target_count,
        false,
        now >= window_end,
        task.optional,
        colors,
    );
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
            parent: None,
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
        let day_secs = 86_400;
        let day = crate::date::span_to_db(&jiff::Span::new().days(1));
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
                parent: None,
                completions,
                last_time: None,
            };

        // Old chain origin (60 days ago), window open in the current
        // interval (30min into a 1h window), zero entries → Reset (not
        // binned as "missed" — the absolute formula marked it passed
        // forever).
        let old = now - 60 * day_secs - 1800;
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

    #[test]
    fn test_tracker_color_linear() {
        let colors = vec![CtColor::DarkRed, CtColor::DarkYellow, CtColor::DarkGreen];
        // min=0, max=10: endpoints and midpoint bin onto the palette.
        assert_eq!(
            tracker_color(&colors, 0.0, Some(0.0), Some(10.0)),
            CtColor::DarkRed
        );
        assert_eq!(
            tracker_color(&colors, 10.0, Some(0.0), Some(10.0)),
            CtColor::DarkGreen
        );
        assert_eq!(
            tracker_color(&colors, 5.0, Some(0.0), Some(10.0)),
            CtColor::DarkYellow
        );
        // Out-of-range scores clamp to the endpoints.
        assert_eq!(
            tracker_color(&colors, -3.0, Some(0.0), Some(10.0)),
            CtColor::DarkRed
        );
        assert_eq!(
            tracker_color(&colors, 99.0, Some(0.0), Some(10.0)),
            CtColor::DarkGreen
        );
        // Inverted range (min > max): lower score → success.
        assert_eq!(
            tracker_color(&colors, 0.0, Some(10.0), Some(0.0)),
            CtColor::DarkGreen
        );
        assert_eq!(
            tracker_color(&colors, 10.0, Some(10.0), Some(0.0)),
            CtColor::DarkRed
        );
    }

    #[test]
    fn test_tracker_color_single_bound() {
        let colors = vec![CtColor::DarkRed, CtColor::DarkYellow, CtColor::DarkGreen];
        // Only a lower bound: below → first color, at/above → last.
        assert_eq!(
            tracker_color(&colors, 4.9, Some(5.0), None),
            CtColor::DarkRed
        );
        assert_eq!(
            tracker_color(&colors, 5.0, Some(5.0), None),
            CtColor::DarkGreen
        );
        assert_eq!(
            tracker_color(&colors, 8.0, Some(5.0), None),
            CtColor::DarkGreen
        );
        // Only an upper bound: above → first color, at/below → last.
        assert_eq!(
            tracker_color(&colors, 8.1, None, Some(8.0)),
            CtColor::DarkRed
        );
        assert_eq!(
            tracker_color(&colors, 8.0, None, Some(8.0)),
            CtColor::DarkGreen
        );
        assert_eq!(
            tracker_color(&colors, 2.0, None, Some(8.0)),
            CtColor::DarkGreen
        );
    }

    fn day_interval_tracker() -> crate::config::TrackerSetting {
        crate::config::TrackerSetting {
            interval: Some(crate::config::TrackerInterval {
                anchor: 0,
                span: jiff::Span::new().days(1),
            }),
            kind: crate::config::TrackerKind::Null,
            min: Some(23.0 * 3600.0), // 23:00, seconds from interval start
            max: Some(2.0 * 3600.0),  // 02:00, seconds from the span end
            colors: None,
        }
    }

    /// Null time-marker coloring: with min=23:00 and max=2h before the span
    /// end, the blue zone is `[mid, min)` = 22:30→23:00 and everything else
    /// is red (1pm is red; only the half-hour before 23:00 is blue).
    #[test]
    fn test_null_tracker_color_time_of_day() {
        let colors = vec![CtColor::DarkRed, CtColor::DarkYellow, CtColor::DarkGreen];
        let tracker = day_interval_tracker();
        // Midnight-anchored interval: t is seconds from the interval start.
        let color_at = |secs: i64| null_tracker_color(&colors, &tracker, secs, 0.0);
        // 13:00 → red zone.
        assert_eq!(color_at(13 * 3600), CtColor::DarkRed);
        // 22:15 → red (the formula's split leaves [22:00, 22:30) red).
        assert_eq!(color_at(22 * 3600 + 15 * 60), CtColor::DarkRed);
        // 22:45 → blue.
        assert_eq!(color_at(22 * 3600 + 45 * 60), CtColor::DarkGreen);
        // 23:30 → red (the wrap segment).
        assert_eq!(color_at(23 * 3600 + 30 * 60), CtColor::DarkRed);
        // 01:00 → red.
        assert_eq!(color_at(3600), CtColor::DarkRed);
    }

    /// Null trackers without an interval (or with a single bound) fall back
    /// to numeric score binning.
    #[test]
    fn test_null_tracker_color_fallbacks() {
        let colors = vec![CtColor::DarkRed, CtColor::DarkYellow, CtColor::DarkGreen];
        // No interval → score binning (middle color when there is no
        // usable range; both bounds still bin the score like a numeric
        // tracker, though they are meaningless without an interval).
        let mut tracker = day_interval_tracker();
        tracker.interval = None;
        tracker.min = None;
        tracker.max = None;
        assert_eq!(
            null_tracker_color(&colors, &tracker, 0, 42.0),
            CtColor::DarkYellow
        );
        // Single bound → binary score binning (count mode): below the
        // bound the first color, at/above it the last.
        let mut tracker = day_interval_tracker();
        tracker.min = Some(5.0);
        tracker.max = None;
        assert_eq!(
            null_tracker_color(&colors, &tracker, 0, 10.0),
            CtColor::DarkGreen
        );
        assert_eq!(
            null_tracker_color(&colors, &tracker, 0, 1.0),
            CtColor::DarkRed
        );
    }

    #[test]
    fn test_tracker_color_no_range() {
        let colors3 = vec![CtColor::DarkRed, CtColor::DarkYellow, CtColor::DarkGreen];
        // Neither bound → middle color, rounded down.
        assert_eq!(
            tracker_color(&colors3, 42.0, None, None),
            CtColor::DarkYellow
        );
        // Degenerate min == max is also "no usable range" → middle.
        assert_eq!(
            tracker_color(&colors3, 42.0, Some(5.0), Some(5.0)),
            CtColor::DarkYellow
        );
        // Even-length palette: rounded-down middle is (len - 1) / 2.
        let colors4 = vec![
            CtColor::DarkRed,
            CtColor::DarkYellow,
            CtColor::DarkGreen,
            CtColor::DarkCyan,
        ];
        assert_eq!(
            tracker_color(&colors4, 42.0, None, None),
            CtColor::DarkYellow
        );
    }
}
