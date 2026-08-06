//! Shared logic for task completion checks.
//! Used by both oneshot and recurring tasks.

use sqlx::SqlitePool;

/// Apply a completion delta to a list of per-event counts (most recent last).
///
/// - delta > 0: append a new entry with that count.
/// - delta < 0: consume entries from the end while remaining > 0 — if the
///   last entry's count is >= remaining, reduce it by remaining; otherwise
///   remove the entry entirely and subtract its count from remaining.
/// - delta == 0: unchanged.
///
/// Counts are never negative: negative deltas only remove/reduce existing
/// entries, so at read time the total is always >= 0.
pub fn apply_delta_to_counts(counts: &[i32], delta: i32) -> Vec<i32> {
    if delta > 0 {
        let mut out = counts.to_vec();
        out.push(delta);
        out
    } else if delta < 0 {
        let mut remaining = -delta;
        let mut out = counts.to_vec();
        while remaining > 0 {
            match out.pop() {
                Some(count) if count > remaining => {
                    out.push(count - remaining);
                    remaining = 0;
                }
                Some(count) => remaining -= count,
                None => break,
            }
        }
        out
    } else {
        counts.to_vec()
    }
}

/// Apply a completion delta to a task at write time, keeping the per-event
/// counts in `todo_completions` as the single source of truth.
///
/// Positive deltas append a new entry with that count; negative deltas
/// consume the most recent entries (see [`apply_delta_to_counts`]). Returns
/// the new total (SUM of counts), which is always >= 0.
///
/// For recurring tasks the consumption is bounded to the current interval:
/// entries from before the current interval started are never touched, and
/// the returned total is the sum within the current interval only.
///
/// After applying the delta the task's `short_id` is synced to its
/// completion state: a oneshot task that just completed loses its short id;
/// a oneshot task that just became not-done again is reassigned the
/// smallest free one.
///
/// The SQL lives in `crate::sql::update_task`; this wrapper keeps the
/// task-completion API at `task::` for callers and tests.
pub async fn apply_completion_delta(
    pool: &SqlitePool,
    todo_id: i64,
    delta: i32,
) -> anyhow::Result<i32> {
    crate::sql::update_task(pool, todo_id, delta).await
}

/// Compute the start time of the interval that contains `now` for a recurring
/// task that began at `start_time` with a fixed `interval_secs`.
///
/// The result is always >= start_time: for `now` before the task began, the
/// boundary is the task start itself.
pub fn current_interval_start(start_time: i64, interval_secs: i64, now: i64) -> i64 {
    if now <= start_time {
        return start_time;
    }
    let idx = (now - start_time).div_euclid(interval_secs);
    start_time + idx * interval_secs
}

/// Check if a task is considered "done" based on its target_count and completions.
///
/// - target_count == 0: Simple done/not-done. Done if completions > 0.
///   (`Some(0)` is *not* done — zero completions is the not-done state regardless
///   of target_count.)
/// - target_count > 0: Needs N completions. Done if completions >= target_count.
pub fn is_task_done(target_count: i32, completions: Option<i32>) -> bool {
    match completions {
        None => false,
        Some(0) => false,
        Some(count) => {
            if target_count == 0 {
                true
            } else {
                count >= target_count
            }
        }
    }
}

/// Calculate the completion percentage for a task.
/// Returns None if target_count is 0 (simple done/not-done).
/// Returns Some(percentage) if target_count > 0.
pub fn completion_percentage(target_count: i32, completions: Option<i32>) -> Option<f64> {
    if target_count == 0 {
        None
    } else {
        let count = completions.unwrap_or(0);
        Some((count as f64 / target_count as f64) * 100.0)
    }
}

/// What Enter should do with a task — decided by the shared pure fn so both
/// TUIs behave identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnterAction {
    /// Mark done — scheduled: set 1; other: +1.
    Complete,
    /// Scheduled only: set 0 (failed).
    SetFailed,
    /// Scheduled Some(0) before the window end: remove the entry.
    Clear,
    /// `target_count <= 1` done: reset directly, no modal.
    Reset,
    /// `target_count > 1` done: the TUI asks "Reset progress?" (default Yes).
    ResetConfirm,
    /// `target_count > 1` not done: the TUI opens the numeric CompleteModal.
    CompletePrompt,
}

/// Next Enter-action for a task, pure and testable. Scheduled tasks never
/// prompt: they cycle 1 → 0 → (none | 1) with `before` = window still open
/// (`start + duration >= now`; other kinds are always "before").
pub fn enter_action(
    completions: Option<i32>,
    is_scheduled: bool,
    target_count: i32,
    start_time: Option<i64>,
    duration: Option<i64>,
    now: i64,
) -> EnterAction {
    if is_scheduled {
        match completions {
            None => EnterAction::Complete,
            Some(c) if c > 0 => EnterAction::SetFailed,
            Some(_) => {
                let before = start_time.unwrap_or(now) + duration.unwrap_or(0) >= now;
                if before {
                    EnterAction::Clear
                } else {
                    EnterAction::Complete
                }
            }
        }
    } else if is_task_done(target_count, completions) {
        if target_count <= 1 {
            EnterAction::Reset
        } else {
            EnterAction::ResetConfirm
        }
    } else if target_count <= 1 {
        EnterAction::Complete
    } else {
        EnterAction::CompletePrompt
    }
}

/// Execute a modal-less [`EnterAction`] against the database. `ResetConfirm`
/// and `CompletePrompt` are TUI-side (modals) and unreachable here.
pub async fn apply_enter_action(
    pool: &SqlitePool,
    task: &crate::sql::TaskRow,
    action: EnterAction,
) -> anyhow::Result<()> {
    match action {
        EnterAction::Complete => {
            if task.is_scheduled() {
                crate::sql::set_scheduled_completion(pool, task.id, 1).await?;
            } else {
                apply_completion_delta(pool, task.id, 1).await?;
            }
        }
        EnterAction::SetFailed => {
            crate::sql::set_scheduled_completion(pool, task.id, 0).await?;
        }
        EnterAction::Clear => {
            crate::sql::reset_task_completions(pool, task.id, None).await?;
        }
        EnterAction::Reset => reset_task_progress(pool, task).await?,
        EnterAction::ResetConfirm | EnterAction::CompletePrompt => {
            unreachable!("modal actions are handled by the TUI")
        }
    }
    Ok(())
}

/// Start of the current interval for a recurring task (`None` for others):
/// the floor used by the interval-scoped completion queries.
pub fn interval_start(task: &crate::sql::TaskRow, now: i64) -> Option<i64> {
    let start = task.start_time?;
    let interval = task.interval_secs?;
    Some(current_interval_start(start, interval, now))
}

/// Whether a task's availability window has fully passed (`now >= window
/// end`). For a recurring task the window is anchored to the **current
/// interval** — `current_interval_start + available_duration_secs <= now`
/// — since `start_time` is the chain origin and never advances; for a
/// scheduled task it is absolute (`start_time + available_duration_secs
/// <= now`). Tasks without a duration never pass. Used by the D10 confirm
/// modal (Enter on a recurring task whose window has passed) and the
/// recurring badge — see docs/VIEWS.md.
pub fn availability_passed(task: &crate::sql::TaskRow, now: i64) -> bool {
    match (
        task.start_time,
        task.interval_secs,
        task.available_duration_secs,
    ) {
        // Recurring: the window moves with each interval. With
        // `dur >= interval` the window covers the whole interval, so the
        // end never precedes now (consistent with `recurring_available`).
        (Some(st), Some(interval), Some(dur)) if interval > 0 => {
            current_interval_start(st, interval, now) + dur <= now
        }
        // Scheduled: the window is absolute.
        (Some(st), None, Some(dur)) => st + dur <= now,
        _ => false,
    }
}

/// Reset a task's completion progress: scheduled/oneshot clear all
/// completions; recurring resets only the current interval (earlier
/// history survives).
pub async fn reset_task_progress(
    pool: &SqlitePool,
    task: &crate::sql::TaskRow,
) -> anyhow::Result<()> {
    let floor = interval_start(task, crate::date::now());
    crate::sql::reset_task_completions(pool, task.id, floor).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_delta_positive_appends() {
        assert_eq!(apply_delta_to_counts(&[], 3), vec![3]);
        assert_eq!(apply_delta_to_counts(&[1, 2], 3), vec![1, 2, 3]);
        assert_eq!(apply_delta_to_counts(&[4], 0), vec![4]);
    }

    #[test]
    fn test_apply_delta_negative_reduces_last_entry() {
        // last entry count >= remaining → reduce it, keep the entry
        assert_eq!(apply_delta_to_counts(&[2, 5], -3), vec![2, 2]);
        assert_eq!(apply_delta_to_counts(&[5], -1), vec![4]);
    }

    #[test]
    fn test_apply_delta_negative_removes_entries() {
        // entry count < remaining → remove entirely and continue
        assert_eq!(apply_delta_to_counts(&[2, 3], -4), vec![1]);
        assert_eq!(apply_delta_to_counts(&[2, 3], -5), Vec::<i32>::new());
        // partial consume across multiple entries
        assert_eq!(apply_delta_to_counts(&[2, 2, 2], -5), vec![1]);
    }

    #[test]
    fn test_apply_delta_negative_more_than_total() {
        // remaining exceeds total → all entries removed
        assert_eq!(apply_delta_to_counts(&[2, 3], -99), Vec::<i32>::new());
        assert_eq!(apply_delta_to_counts(&[], -3), Vec::<i32>::new());
    }

    #[test]
    fn test_apply_delta_negative_reduce_to_zero_removes() {
        // count exactly equal to remaining → entry becomes 0, must be dropped
        assert_eq!(apply_delta_to_counts(&[2, 3], -3), vec![2]);
        assert_eq!(apply_delta_to_counts(&[3], -3), Vec::<i32>::new());
    }

    #[test]
    fn test_availability_passed() {
        let day = 86_400;
        let st = 1_000_000i64;
        let hour = 3600;
        let row = |interval: Option<i64>, dur: Option<i64>| crate::sql::TaskRow {
            id: 1,
            short_id: Some(1),
            name: "t".to_string(),
            body: String::new(),
            priority: 5,
            start_time: Some(st),
            available_duration_secs: dur,
            interval_secs: interval,
            target_count: 0,
            optional: 0,
            end_time: None,
            completions: None,
            last_time: None,
        };

        // Recurring, old origin (60 days ago): the window is anchored to
        // the current interval, not the chain origin.
        let now = st + 60 * day + 10_000;
        assert!(
            availability_passed(&row(Some(day), Some(hour)), now),
            "window ended at interval_start + 1h, now is 2.7h in"
        );
        assert!(
            !availability_passed(&row(Some(day), Some(hour)), st + 60 * day + 1800),
            "window still open 30min in"
        );
        // Exactly at the window end → passed (<=).
        assert!(availability_passed(
            &row(Some(day), Some(hour)),
            st + 60 * day + hour
        ));
        // dur >= interval: the window covers the whole interval → never passed.
        assert!(!availability_passed(
            &row(Some(day), Some(2 * day)),
            st + 60 * day + 1800
        ));
        // No duration → never passed.
        assert!(!availability_passed(&row(Some(day), None), now));
        assert!(!availability_passed(&row(None, None), now));

        // Scheduled: the window is absolute.
        assert!(availability_passed(&row(None, Some(hour)), st + 2 * hour));
        assert!(!availability_passed(
            &row(None, Some(3 * hour)),
            st + 2 * hour
        ));
    }

    #[test]
    fn test_current_interval_start() {
        let day = 86_400;
        let start = 1_000_000;
        // now exactly at start → first interval
        assert_eq!(current_interval_start(start, day, start), start);
        // now mid-first-interval → boundary is start
        assert_eq!(current_interval_start(start, day, start + 100), start);
        // now exactly one interval later → second interval starts at start+day
        assert_eq!(current_interval_start(start, day, start + day), start + day);
        // now mid-second-interval → boundary is start+day
        assert_eq!(
            current_interval_start(start, day, start + day + 50),
            start + day
        );
        // now before task start → boundary clamps to task start
        assert_eq!(current_interval_start(start, day, start - 10), start);
        // now many intervals later
        assert_eq!(
            current_interval_start(start, day, start + 10 * day + 123),
            start + 10 * day
        );
    }

    #[test]
    fn test_is_task_done_simple() {
        // target_count = 0: simple done/not-done
        assert!(!is_task_done(0, None));
        assert!(is_task_done(0, Some(1)));
        assert!(is_task_done(0, Some(5)));
    }

    #[test]
    fn test_is_task_done_with_target() {
        // target_count = 3: needs 3 completions
        assert!(!is_task_done(3, None));
        assert!(!is_task_done(3, Some(0)));
        assert!(!is_task_done(3, Some(1)));
        assert!(!is_task_done(3, Some(2)));
        assert!(is_task_done(3, Some(3)));
        assert!(is_task_done(3, Some(5))); // over-completed
    }

    #[test]
    fn test_completion_percentage_simple() {
        // target_count = 0: no percentage
        assert_eq!(completion_percentage(0, None), None);
        assert_eq!(completion_percentage(0, Some(1)), None);
    }

    #[test]
    fn test_completion_percentage_with_target() {
        assert_eq!(completion_percentage(4, None), Some(0.0));
        assert_eq!(completion_percentage(4, Some(0)), Some(0.0));
        assert_eq!(completion_percentage(4, Some(1)), Some(25.0));
        assert_eq!(completion_percentage(4, Some(2)), Some(50.0));
        assert_eq!(completion_percentage(4, Some(3)), Some(75.0));
        assert_eq!(completion_percentage(4, Some(4)), Some(100.0));
        assert_eq!(completion_percentage(4, Some(5)), Some(125.0)); // over-completed
    }

    #[test]
    fn test_enter_action_scheduled() {
        // Scheduled tasks never prompt: none → done; done → failed;
        // failed → cleared before the window end, done after.
        let now = 1_000_000i64;
        let (start, dur) = (Some(now - 3600), Some(3600)); // window ends at now
        let scheduled = |c| enter_action(c, true, 0, start, dur, now);
        assert_eq!(scheduled(None), EnterAction::Complete);
        assert_eq!(scheduled(Some(1)), EnterAction::SetFailed);
        assert_eq!(scheduled(Some(0)), EnterAction::Clear); // now == window end → before

        let (start, dur) = (Some(now - 7200), Some(3600)); // window ended an hour ago
        let scheduled = |c| enter_action(c, true, 0, start, dur, now);
        assert_eq!(scheduled(None), EnterAction::Complete);
        assert_eq!(scheduled(Some(1)), EnterAction::SetFailed);
        assert_eq!(scheduled(Some(0)), EnterAction::Complete); // after → done
    }

    #[test]
    fn test_enter_action_once_only_and_target_one() {
        // Once-only and target-1 tasks just toggle: not done → Complete,
        // done → Reset (no modal).
        let now = 1_000_000i64;
        assert_eq!(
            enter_action(None, false, 0, None, None, now),
            EnterAction::Complete
        );
        assert_eq!(
            enter_action(Some(0), false, 0, None, None, now),
            EnterAction::Complete
        );
        assert_eq!(
            enter_action(Some(1), false, 0, None, None, now),
            EnterAction::Reset
        );
        assert_eq!(
            enter_action(None, false, 1, None, None, now),
            EnterAction::Complete
        );
        assert_eq!(
            enter_action(Some(1), false, 1, None, None, now),
            EnterAction::Reset
        );
        assert_eq!(
            enter_action(Some(0), false, 1, None, None, now),
            EnterAction::Complete
        );
    }

    #[test]
    fn test_enter_action_multi_target() {
        // target_count > 1: prompt paths — CompleteModal when not complete,
        // ResetConfirm when complete.
        let now = 1_000_000i64;
        assert_eq!(
            enter_action(None, false, 5, None, None, now),
            EnterAction::CompletePrompt
        );
        assert_eq!(
            enter_action(Some(0), false, 5, None, None, now),
            EnterAction::CompletePrompt
        );
        assert_eq!(
            enter_action(Some(2), false, 5, None, None, now),
            EnterAction::CompletePrompt
        );
        assert_eq!(
            enter_action(Some(5), false, 5, None, None, now),
            EnterAction::ResetConfirm
        );
        assert_eq!(
            enter_action(Some(7), false, 5, None, None, now),
            EnterAction::ResetConfirm
        );
    }
}
