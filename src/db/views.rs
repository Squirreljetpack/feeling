use anyhow::{Context, Result};
use sqlx::{FromRow, SqlitePool};

use super::entries::fetch_completions_between;
use super::models::{RecurringWindow, TaskRow};
use crate::types::{ViewMode, ViewVariant};

/// Incomplete oneshot tasks due by `horizon_end` (and >= `floor`, which
/// excludes overdue tasks unless the config opts in). Due is `end_time` when
/// set (`! name @<time>`); rows without one fall back to `start_time`, so
/// legacy rows (where the old due lived in `start_time`) and undated tasks
/// (due at creation) keep working.
pub async fn fetch_due_oneshot_tasks(
    pool: &SqlitePool,
    horizon_end: i64,
    floor: i64,
) -> Result<Vec<TaskRow>> {
    // available_duration_secs IS NULL excludes scheduled tasks — the today
    // view fetches those separately via fetch_scheduled_today. Completed
    // tasks are included too: their row carries the ✓ badge (the today view
    // no longer emits separate completion rows). Due is the end_time when
    // set, else the start_time (legacy rows / undated tasks).
    let tasks = sqlx::query_as::<_, TaskRow>(
        r#"SELECT t.*, SUM(tc.count) AS completions, MAX(tc.time) AS last_time
           FROM todos t
           LEFT JOIN todo_completions tc ON tc.todo_id = t.id
           WHERE t.interval_secs IS NULL
           AND t.available_duration_secs IS NULL
           AND COALESCE(t.end_time, t.start_time) <= ?
           AND COALESCE(t.end_time, t.start_time) >= ?
           GROUP BY t.id
           ORDER BY t.priority DESC, COALESCE(t.end_time, t.start_time) ASC"#,
    )
    .bind(horizon_end)
    .bind(floor)
    .fetch_all(pool)
    .await
    .context("Failed to fetch due tasks")?;
    Ok(tasks)
}

/// Scheduled tasks whose availability window overlaps `[floor, horizon_end)`:
/// started before the horizon ends and still open past the floor. Used by the
/// today view (floor = today start, horizon_end scales with the horizon). All
/// states are included — ongoing, completed, failed — matching the window
/// overlap semantics in VIEWS.md.
pub async fn fetch_scheduled_today(
    pool: &SqlitePool,
    horizon_end: i64,
    floor: i64,
) -> Result<Vec<TaskRow>> {
    let tasks = sqlx::query_as::<_, TaskRow>(
        r#"SELECT t.*, SUM(tc.count) AS completions, MAX(tc.time) AS last_time
           FROM todos t
           LEFT JOIN todo_completions tc ON tc.todo_id = t.id
           WHERE t.interval_secs IS NULL
             AND t.available_duration_secs IS NOT NULL
             AND t.start_time < ?
             AND t.start_time + t.available_duration_secs > ?
           GROUP BY t.id
           ORDER BY t.priority DESC, t.start_time ASC"#,
    )
    .bind(horizon_end)
    .bind(floor)
    .fetch_all(pool)
    .await
    .context("Failed to fetch scheduled tasks for today")?;
    Ok(tasks)
}

/// Whether a recurring task is currently within its availability window.
/// Tasks without an `available_duration_secs` are always available.
///
/// Expired tasks (end_time set and past) are *not* subject to the window
/// check: they have no current interval, so expiry is handled by the SQL
/// `end_time` filter instead (they pass through here).
pub fn recurring_available(task: &TaskRow, now: i64) -> bool {
    if task.end_time.is_some_and(|end| now > end) {
        return true;
    }
    match (
        task.start_time,
        task.interval_secs,
        task.available_duration_secs,
    ) {
        (Some(st), Some(interval), Some(dur)) if dur < interval => {
            let elapsed = now - st;
            let mod_pos = ((elapsed % interval) + interval) % interval;
            mod_pos < dur
        }
        _ => true,
    }
}

/// Tasks with a completion entry in `[day_start, day_end)` — the
/// "completed today" fetch for the today view. The completions sum is
/// scoped to the recurring task's current interval (so the badge matches
/// the regular recurring fetch, D8); non-recurring rows keep the unscoped
/// sum. `last_time` is the most recent completion timestamp within the day
/// window (the time label + sort key for the merged row).
pub async fn fetch_tasks_completed_on(
    pool: &SqlitePool,
    day_start: i64,
    day_end: i64,
) -> Result<Vec<TaskRow>> {
    let now = crate::date::now();
    let tasks = sqlx::query_as::<_, TaskRow>(
        r#"SELECT t.*, SUM(tc.count) AS completions,
                  (SELECT MAX(c.time) FROM todo_completions c
                    WHERE c.todo_id = t.id AND c.time >= ? AND c.time < ?) AS last_time
           FROM todos t
           LEFT JOIN todo_completions tc ON tc.todo_id = t.id
               AND tc.time >= CASE
                   WHEN t.interval_secs IS NOT NULL AND t.start_time IS NOT NULL THEN
                       CASE WHEN ? <= t.start_time THEN t.start_time
                            ELSE t.start_time + ((? - t.start_time) / t.interval_secs) * t.interval_secs END
                   ELSE 0 END
           WHERE EXISTS (
               SELECT 1 FROM todo_completions c
               WHERE c.todo_id = t.id AND c.time >= ? AND c.time < ?
           )
           GROUP BY t.id"#,
    )
    .bind(day_start)
    .bind(day_end)
    .bind(now)
    .bind(now)
    .bind(day_start)
    .bind(day_end)
    .fetch_all(pool)
    .await
    .context("Failed to fetch tasks completed today")?;
    Ok(tasks)
}

/// Availability windows of a recurring task that intersect
/// `[period_start, period_end]`, as `(window_start, window_end)` pairs
/// (ascending). Windows are `[start + k*interval, start + k*interval +
/// dur)` — the whole interval when `dur` is None or >= interval — and
/// move with each interval, so the scan is built from `interval_start`
/// math: a raw `start_time + duration >= period_start` comparison would
/// degenerate to "every task ever started" for old start times. When
/// `end_time` is set, the last window is truncated at the expiry and
/// windows after it don't count.
fn recurring_windows_in_period(
    task: &TaskRow,
    period_start: i64,
    period_end: i64,
) -> Vec<(i64, i64)> {
    let (Some(st), Some(interval)) = (task.start_time, task.interval_secs) else {
        return Vec::new();
    };
    if interval <= 0 {
        return Vec::new();
    }
    // Window length: explicit duration when set and < interval, else the
    // whole interval.
    let dur = match task.available_duration_secs {
        Some(d) if d < interval => d,
        _ => interval,
    };
    // First window index that could reach the period (integer division
    // truncates, so overshoot by one window; k >= 0).
    let k0 = ((period_start - st) / interval - 1).max(0);
    let mut windows = Vec::new();
    let mut k = k0;
    loop {
        let w_start = st + k * interval;
        if w_start > period_end {
            break;
        }
        let w_end = match task.end_time {
            Some(end) => (w_start + dur).min(end),
            None => w_start + dur,
        };
        if w_end > period_start {
            windows.push((w_start, w_end));
        }
        k += 1;
    }
    windows
}

/// A recurring task row plus its unscoped last completion — the today-view
/// window fetch returns it so each window row can carry the unscoped last
/// in `end_time`.
#[derive(Debug, FromRow)]
struct RecurringTaskRow {
    #[sqlx(flatten)]
    task: TaskRow,
    unscoped_last: Option<i64>,
}
/// Per-availability-window rows for every recurring task with a window
/// intersecting `[period_start, period_end]` — the today-view recurring
/// fetch (all variants). One [`RecurringWindow`] per intersecting window;
/// the view decides whether to keep them all (All) or only the next per
/// task (B). Completions are scoped per window's interval (sum + most
/// recent completion time), matching the interval-scoped completion
/// queries elsewhere (D8). Expired tasks (`end_time` passed before the
/// period) are excluded, they have no windows in it.
pub async fn fetch_recurring_windows_for_period(
    pool: &SqlitePool,
    period_start: i64,
    period_end: i64,
) -> Result<Vec<RecurringWindow>> {
    let tasks = sqlx::query_as::<_, RecurringTaskRow>(
        r#"SELECT t.*, NULL AS completions, NULL AS last_time,
                  (SELECT MAX(tc.time) FROM todo_completions tc
                       WHERE tc.todo_id = t.id) AS unscoped_last
           FROM todos t
           WHERE t.interval_secs IS NOT NULL
           AND (t.end_time IS NULL OR t.end_time > ?)
           AND t.start_time <= ?
           ORDER BY t.priority DESC, t.start_time ASC"#,
    )
    .bind(period_start)
    .bind(period_end)
    .fetch_all(pool)
    .await
    .context("Failed to fetch recurring tasks for period")?;

    let mut windows = Vec::new();
    for row in &tasks {
        let task = &row.task;
        let wins = recurring_windows_in_period(task, period_start, period_end);
        if wins.is_empty() {
            continue;
        }
        let interval = task.interval_secs.expect("filtered to recurring tasks");
        let st = task.start_time.expect("filtered to tasks with a start");
        // Completion events within the span of the intersecting windows
        // (each window's interval, i.e. up to the last interval end).
        let span_end = wins.last().expect("non-empty").0 + interval;
        let completions = fetch_completions_between(pool, task.id, wins[0].0, span_end).await?;
        let k_first = (wins[0].0 - st) / interval;
        for (wi, (w_start, w_end)) in wins.iter().enumerate() {
            let mut count = 0i64;
            let mut last_time: Option<i64> = None;
            for c in &completions {
                if (c.time - st).div_euclid(interval) == k_first + wi as i64 {
                    count += c.count;
                    last_time = Some(c.time);
                }
            }
            let mut task = task.clone();
            task.completions = Some(count as i32);
            task.last_time = last_time;
            // The window row's `end_time` carries the task's unscoped last
            // completion (the today view doesn't use the expiry; the window
            // geometry above was computed against the real end_time).
            task.end_time = row.unscoped_last;
            windows.push(RecurringWindow {
                task,
                window_start: *w_start,
                window_end: *w_end,
            });
        }
    }
    Ok(windows)
}

/// Task rows for a view mode at a [`ShowVariant`], with per-mode completion
/// scoping. Shared by the CLI (`task_view::write_task_view`) and the TUI tasks app.
///
/// `persist_pending_seconds` keeps just-completed tasks visible in the
/// pending `All` view (D9). See VIEWS.md for the full matrix.
pub async fn fetch_tasks_for_view(
    pool: &SqlitePool,
    mode: ViewMode,
    show: ViewVariant,
    persist_pending_seconds: i64,
) -> Result<Vec<TaskRow>> {
    let now = crate::date::now();
    match (mode, show) {
        // `@` All: not-done oneshots ∪ recurring (interval-scoped, not
        // expired, availability-filtered in Rust) ∪ `ongoing(S)` only (D1:
        // failed/auto-completed/completed scheduled excluded) ∪ any task
        // with a completion entry in the last persist_pending_seconds (D9;
        // the window is `[now - persist, now]` — inclusive upper bound so a
        // same-second completion stays visible).
        (ViewMode::PendingTasks, ViewVariant::All) => {
            let tasks = sqlx::query_as::<_, TaskRow>(
                r#"SELECT t.*, SUM(tc.count) AS completions,
                       (SELECT MAX(tc2.time) FROM todo_completions tc2 WHERE tc2.todo_id = t.id) AS last_time
                   FROM todos t
                   LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                       AND tc.time >= CASE
                           WHEN t.interval_secs IS NOT NULL AND t.start_time IS NOT NULL THEN
                               CASE WHEN ? <= t.start_time THEN t.start_time
                                    ELSE t.start_time + ((? - t.start_time) / t.interval_secs) * t.interval_secs END
                           ELSE 0 END
                   WHERE (t.interval_secs IS NULL AND t.available_duration_secs IS NULL)
                      OR (t.interval_secs IS NOT NULL AND (t.end_time IS NULL OR t.end_time > ?))
                      OR (t.interval_secs IS NULL AND t.available_duration_secs IS NOT NULL
                          AND t.start_time + t.available_duration_secs >= ?)
                      OR t.id IN (SELECT todo_id FROM todo_completions
                                  WHERE time >= ? AND time <= ?)
                   GROUP BY t.id
                   HAVING (t.interval_secs IS NULL AND t.available_duration_secs IS NULL
                           AND (completions IS NULL OR completions < t.target_count))
                       OR (t.interval_secs IS NOT NULL
                           AND (completions IS NULL OR completions < t.target_count))
                       OR (t.interval_secs IS NULL AND t.available_duration_secs IS NOT NULL
                           AND completions IS NULL)
                       OR t.id IN (SELECT todo_id FROM todo_completions
                                   WHERE time >= ? AND time <= ?)
                   ORDER BY t.priority DESC, t.start_time ASC"#,
            )
            .bind(now)
            .bind(now)
            .bind(now)
            .bind(now)
            .bind(now - persist_pending_seconds)
            .bind(now)
            .bind(now - persist_pending_seconds)
            .bind(now)
            .fetch_all(pool)
            .await
            .context("Failed to fetch pending tasks")?;

            // Keep only recurring tasks currently within their availability
            // window (`recurring_available` skips expired tasks — they have
            // no current interval; the SQL end_time filter already excluded
            // them from the recurring branch, but the D9 recently-completed
            // union can still surface them).
            Ok(tasks
                .into_iter()
                .filter(|t| !t.is_recurring() || recurring_available(t, now))
                .collect())
        }
        // `@` A: not-done oneshot tasks only (old `!` list) ∪ D9: any
        // oneshot (incl. done) with a completion entry in the last
        // persist_pending_seconds.
        (ViewMode::PendingTasks, ViewVariant::A) => {
            let tasks = sqlx::query_as::<_, TaskRow>(
                r#"SELECT t.*, SUM(tc.count) AS completions, MAX(tc.time) AS last_time
                   FROM todos t
                   LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                   WHERE t.interval_secs IS NULL AND t.available_duration_secs IS NULL
                   GROUP BY t.id
                   HAVING completions IS NULL OR completions < t.target_count
                       OR t.id IN (SELECT todo_id FROM todo_completions
                                   WHERE time >= ? AND time <= ?)
                   ORDER BY t.priority DESC, t.start_time ASC"#,
            )
            .bind(now - persist_pending_seconds)
            .bind(now)
            .fetch_all(pool)
            .await
            .context("Failed to fetch pending oneshot tasks")?;
            Ok(tasks)
        }
        // `@` B: not-done recurring (any not expired, NOT availability-
        // filtered — tasks whose availability window has passed stay) ∪
        // non-done scheduled with `window_open` (`now <= start + duration`
        // — ongoing or failed-with-open-window; failed with a closed window
        // belongs to @done) ∪ D9: sched/recur tasks (incl. done) with a
        // completion entry in the last persist_pending_seconds.
        (ViewMode::PendingTasks, ViewVariant::B) => {
            let tasks = sqlx::query_as::<_, TaskRow>(
                r#"SELECT t.*, SUM(tc.count) AS completions,
                       (SELECT MAX(tc2.time) FROM todo_completions tc2 WHERE tc2.todo_id = t.id) AS last_time
                   FROM todos t
                   LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                       AND tc.time >= CASE
                           WHEN t.interval_secs IS NOT NULL AND t.start_time IS NOT NULL THEN
                               CASE WHEN ? <= t.start_time THEN t.start_time
                                    ELSE t.start_time + ((? - t.start_time) / t.interval_secs) * t.interval_secs END
                           ELSE 0 END
                   WHERE (t.interval_secs IS NOT NULL AND (t.end_time IS NULL OR t.end_time > ?))
                      OR (t.interval_secs IS NULL AND t.available_duration_secs IS NOT NULL
                          AND t.start_time + t.available_duration_secs >= ?)
                      OR (t.id IN (SELECT todo_id FROM todo_completions
                                   WHERE time >= ? AND time <= ?)
                          AND (t.interval_secs IS NOT NULL
                               OR t.available_duration_secs IS NOT NULL))
                   GROUP BY t.id
                   HAVING (t.interval_secs IS NOT NULL
                           AND (completions IS NULL OR completions < t.target_count))
                       OR (t.interval_secs IS NULL AND t.available_duration_secs IS NOT NULL
                           AND (completions IS NULL OR completions = 0)
                           AND t.start_time + t.available_duration_secs >= ?)
                       OR (t.id IN (SELECT todo_id FROM todo_completions
                                    WHERE time >= ? AND time <= ?)
                           AND (t.interval_secs IS NOT NULL
                                OR t.available_duration_secs IS NOT NULL))
                   ORDER BY t.priority DESC, t.start_time ASC"#,
            )
            .bind(now)
            .bind(now)
            .bind(now)
            .bind(now)
            .bind(now - persist_pending_seconds)
            .bind(now)
            .bind(now)
            .bind(now - persist_pending_seconds)
            .bind(now)
            .fetch_all(pool)
            .await
            .context("Failed to fetch pending recurring/scheduled tasks")?;
            Ok(tasks)
        }
        // `@done` All: done oneshots ∪ scheduled with any entry (completed
        // or failed — D2) ∪ recurring done in the current interval.
        (ViewMode::DoneTasks, ViewVariant::All) => {
            let tasks = sqlx::query_as::<_, TaskRow>(
                r#"SELECT t.*, SUM(tc.count) AS completions,
                       (SELECT MAX(tc2.time) FROM todo_completions tc2 WHERE tc2.todo_id = t.id) AS last_time
                   FROM todos t
                   LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                       AND tc.time >= CASE
                           WHEN t.interval_secs IS NOT NULL AND t.start_time IS NOT NULL THEN
                               CASE WHEN ? <= t.start_time THEN t.start_time
                                    ELSE t.start_time + ((? - t.start_time) / t.interval_secs) * t.interval_secs END
                           ELSE 0 END
                   WHERE (t.interval_secs IS NULL AND t.available_duration_secs IS NULL)
                      OR (t.interval_secs IS NULL AND t.available_duration_secs IS NOT NULL)
                      OR (t.interval_secs IS NOT NULL
                          AND (t.start_time IS NULL OR t.start_time <= ?)
                          AND (t.end_time IS NULL OR t.end_time > ?))
                   GROUP BY t.id
                   HAVING (t.interval_secs IS NULL AND t.available_duration_secs IS NULL
                           AND completions IS NOT NULL AND completions >= t.target_count)
                       OR (t.interval_secs IS NULL AND t.available_duration_secs IS NOT NULL
                           AND completions IS NOT NULL)
                       OR (t.interval_secs IS NOT NULL
                           AND completions IS NOT NULL AND completions >= t.target_count)
                   ORDER BY COALESCE(MAX(tc.time), t.start_time + COALESCE(t.available_duration_secs, 0)) DESC"#,
            )
            .bind(now)
            .bind(now)
            .bind(now)
            .bind(now)
            .fetch_all(pool)
            .await
            .context("Failed to fetch done tasks")?;
            Ok(tasks)
        }
        // `@done` A: done oneshot tasks only (`completions >= target_count`).
        (ViewMode::DoneTasks, ViewVariant::A) => {
            let tasks = sqlx::query_as::<_, TaskRow>(
                r#"SELECT t.*, SUM(tc.count) AS completions, MAX(tc.time) AS last_time
                   FROM todos t
                   LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                   WHERE t.interval_secs IS NULL AND t.available_duration_secs IS NULL
                   GROUP BY t.id
                   HAVING completions IS NOT NULL AND completions >= t.target_count
                   ORDER BY COALESCE(MAX(tc.time), t.start_time) DESC"#,
            )
            .fetch_all(pool)
            .await
            .context("Failed to fetch done oneshot tasks")?;
            Ok(tasks)
        }
        // `@done` B: ALL recurring tasks (one row per task — history scope,
        // no completions filter, includes expired and never-completed rows;
        // D3) ∪ scheduled with any entry or auto-completed (no entry,
        // window elapsed — D2).
        (ViewMode::DoneTasks, ViewVariant::B) => {
            let tasks = sqlx::query_as::<_, TaskRow>(
                r#"SELECT t.*, SUM(tc.count) AS completions, MAX(tc.time) AS last_time
                   FROM todos t
                   LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                   WHERE t.interval_secs IS NOT NULL
                      OR (t.interval_secs IS NULL AND t.available_duration_secs IS NOT NULL)
                   GROUP BY t.id
                   HAVING t.interval_secs IS NOT NULL
                       OR (t.interval_secs IS NULL AND t.available_duration_secs IS NOT NULL
                           AND (completions IS NOT NULL
                                OR t.start_time + t.available_duration_secs < ?))
                   ORDER BY COALESCE(MAX(tc.time),
                       CASE WHEN t.interval_secs IS NULL
                            THEN t.start_time + COALESCE(t.available_duration_secs, 0)
                            ELSE t.start_time END) DESC"#,
            )
            .bind(now)
            .fetch_all(pool)
            .await
            .context("Failed to fetch done recurring/scheduled tasks")?;
            Ok(tasks)
        }
    }
}
