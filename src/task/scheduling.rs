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

/// Pending-view sort key shared by the task list and today view.
///
/// Done tasks use their last completion (or an auto-completed scheduled
/// window end); pending tasks use their schedule-specific availability key.
pub fn pending_sort_time(task: &crate::db::TaskRow, now: i64) -> i64 {
    if task.is_done() {
        return task.last_time.unwrap_or_else(|| {
            task.start_time
                .unwrap_or(i64::MAX)
                .saturating_add(task.available_duration_secs.unwrap_or(0))
        });
    }
    if task.is_scheduled() {
        return task.start_time.unwrap_or(i64::MAX);
    }
    if task.is_recurring() {
        return recurring_window_end(task, now);
    }
    task.end_time.unwrap_or(i64::MAX)
}

/// Done-view sort key: last completion, or the completion moment implied by
/// an entry-less scheduled/recurring history row.
pub fn completed_sort_time(task: &crate::db::TaskRow) -> i64 {
    if let Some(last) = task.last_time {
        return last;
    }
    let start = task.start_time.unwrap_or(i64::MAX);
    if task.interval_secs.is_none() {
        start.saturating_add(task.available_duration_secs.unwrap_or(0))
    } else {
        start
    }
}

fn recurring_window_end(task: &crate::db::TaskRow, now: i64) -> i64 {
    match (task.start_time, task.interval_secs) {
        (Some(st), Some(interval)) => {
            let interval_start = current_interval_start(st, interval, now);
            match task.available_duration_secs {
                Some(dur) if dur < interval && now < interval_start + dur => interval_start + dur,
                _ => interval_start + interval,
            }
        }
        // Defensive: interval-less recurring row — fall back to the anchor.
        _ => task.start_time.unwrap_or(now),
    }
}

/// Start of the current interval for a recurring task (`None` for others):
/// the floor used by the interval-scoped completion queries.
pub fn interval_start(task: &crate::db::TaskRow, now: i64) -> Option<i64> {
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
pub fn availability_passed(task: &crate::db::TaskRow, now: i64) -> bool {
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
