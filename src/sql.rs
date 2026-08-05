//! Typed database access. Every SQL query in the crate lives here — callers
//! outside this module must not touch `sqlx::query*` directly. Schema
//! bootstrap (pool creation + migrations) lives in `crate::db`.
//!
//! Query results are returned as plain structs (`TaskObject`, `FeelingRow`,
//! ...) so callers never see `sqlx` row types.

use anyhow::{Context, Result};
use sqlx::{FromRow, Row, SqlitePool};

use crate::clap::ViewMode;
use crate::config::TrackerType;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A task as seen by the creation/edit flows. `id` is the stable row id
/// (`Some` for existing tasks; `None` for new tasks — the row id is
/// autoassigned at insert time). `short_id` is the user-facing id: always
/// `None` on new tasks (the SQL layer allocates it), and `None` for
/// existing oneshot tasks once they are completed.
#[derive(Debug, Clone)]
pub struct TaskObject {
    pub id: Option<i64>,
    pub short_id: Option<i64>,
    pub name: String,
    pub body: String,
    pub priority: i32,
    pub start_time: Option<i64>,
    pub available_duration_secs: Option<i64>,
    pub interval_secs: Option<i64>,
    pub target_count: i32,
    pub optional: bool,
    pub end_time: Option<i64>,
}

impl TaskObject {
    pub fn is_recurring(&self) -> bool {
        self.interval_secs.is_some()
    }

    /// A scheduled task: no recurrence interval, with an availability
    /// window. See [`TaskRow::is_scheduled`].
    pub fn is_scheduled(&self) -> bool {
        self.interval_secs.is_none() && self.available_duration_secs.is_some()
    }
}

/// The recurring-task fields editable via the interactive edit flow.
#[derive(Debug, Clone)]
pub struct UpdateTaskObject {
    pub id: i64,
    pub interval_secs: Option<i64>,
    pub available_duration_secs: Option<i64>,
    pub target_count: i32,
    pub optional: bool,
    pub end_time: Option<i64>,
}

/// A logged mood entry plus any linked custom-tracker values.
///
/// `customs` carries the pre-resolved `CustomValue`s and, for Text/Float
/// trackers with an interval, the slot `(start, end)` whose previous entry
/// is replaced inside the insert transaction.
#[derive(Debug, Clone)]
pub struct EntryObject {
    pub mood: String,
    pub body: String,
    pub time: i64,
    pub embedding: Option<Vec<u8>>,
    pub customs: Vec<CustomObject>,
}

#[derive(Debug, Clone)]
pub struct CustomObject {
    pub tracker_type: String,
    pub value: CustomValue,
    pub replace_slot: Option<(i64, i64)>,
}

/// Typed payload of a custom tracker entry, determined by its configured kind.
#[derive(Debug, Clone)]
pub enum CustomValue {
    Text(String),
    Number(i64),
    Float(f64),
}

impl std::fmt::Display for CustomValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CustomValue::Text(s) => write!(f, "{}", s),
            CustomValue::Number(n) => write!(f, "{}", n),
            CustomValue::Float(x) => write!(f, "{}", x),
        }
    }
}

/// A full todos row plus the aggregate completion count for the current
/// view/interval context (the `completions` column comes from the query).
#[derive(Debug, Clone, FromRow)]
pub struct TaskRow {
    pub id: i64,
    pub short_id: Option<i64>,
    pub name: String,
    pub body: String,
    pub priority: i32,
    pub start_time: Option<i64>,
    pub available_duration_secs: Option<i64>,
    pub interval_secs: Option<i64>,
    pub target_count: i32,
    pub optional: i32,
    pub end_time: Option<i64>,
    pub completions: Option<i32>,
}

impl TaskRow {
    pub fn is_recurring(&self) -> bool {
        self.interval_secs.is_some()
    }

    /// A scheduled task: no recurrence interval, with an availability
    /// window (`available_duration_secs`). Recurring tasks can carry an
    /// available duration too, so the interval check is what separates them.
    pub fn is_scheduled(&self) -> bool {
        self.interval_secs.is_none() && self.available_duration_secs.is_some()
    }

    pub fn is_done(&self) -> bool {
        if self.is_scheduled() {
            // Scheduled tasks are done when they have a completed entry
            // (>= 1) or their window has fully elapsed with no entry
            // (auto-completed). A failed entry (0) is not done.
            match self.completions {
                Some(c) if c > 0 => true,
                Some(_) => false,
                None => match (self.start_time, self.available_duration_secs) {
                    (Some(st), Some(dur)) => st + dur < crate::date::now(),
                    _ => false,
                },
            }
        } else {
            crate::task::is_task_done(self.target_count, self.completions)
        }
    }

    pub fn start_datetime(&self) -> Option<String> {
        self.start_time.map(crate::date::format_date_time)
    }

    pub fn end_datetime(&self) -> Option<String> {
        self.end_time.map(crate::date::format_date_time)
    }
}

/// A feeling row for the tracker/today views.
#[derive(Debug, Clone)]
pub struct FeelingRow {
    pub id: i64,
    pub mood: String,
    pub body: String,
    pub time: i64,
    pub embedding: Option<Vec<u8>>,
}

/// A custom-tracker row with the score decoded as text (the `score` column
/// is a BLOB with dynamic typing; `CAST(score AS TEXT)` makes every storage
/// type decodable).
#[derive(Debug, Clone)]
pub struct CustomEntryRow {
    pub id: i64,
    pub tracker_type: String,
    pub score: String,
    pub time: i64,
}

/// Recurring-task metadata used by the completion-dots tracker.
#[derive(Debug, Clone)]
pub struct RecurringTaskMeta {
    pub id: i64,
    pub interval_secs: Option<i64>,
    pub target_count: i32,
}

/// A completion event (time, count) for a task.
#[derive(Debug, Clone)]
pub struct CompletionRow {
    pub time: i64,
    pub count: i64,
}

/// A task deleted by `prune_tasks`, with the reason it was pruned. The
/// `short_id` is `None` for completed oneshot tasks (their id is cleared on
/// completion).
#[derive(Debug, Clone)]
pub struct PrunedTask {
    pub id: i64,
    pub short_id: Option<i64>,
    pub name: String,
    pub reason: String,
}

/// Task identity + completion state for the `- <short-id> [count]` update
/// command. `id` is the stable row id; `short_id` is the user-facing id
/// (`None` once the task is completed).
#[derive(Debug, Clone)]
pub struct TaskUpdateInfo {
    pub id: i64,
    pub short_id: Option<i64>,
    pub name: String,
    pub target_count: i32,
    pub prior_completions: i32,
}

// ---------------------------------------------------------------------------
// Task CRUD
// ---------------------------------------------------------------------------

/// Insert a new task. Both the stable row id and the user-facing `short_id`
/// are assigned by the database layer — the caller must not pass either
/// (`task.id` and `task.short_id` must be `None`). Returns the row id and
/// the allocated short id.
pub async fn create_task(pool: &SqlitePool, task: &TaskObject) -> Result<(i64, i64)> {
    assert!(
        task.short_id.is_none(),
        "create_task assigns the short id itself; the task must not carry one"
    );
    assert!(
        task.id.is_none(),
        "create_task assigns the row id itself; the task must not carry one"
    );
    let short_id = allocate_short_id(pool).await?;

    let row = sqlx::query(
        r#"INSERT INTO todos (name, body, priority, short_id, start_time, available_duration_secs, interval_secs, target_count, optional, end_time)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
           RETURNING id"#,
    )
    .bind(&task.name)
    .bind(&task.body)
    .bind(task.priority)
    .bind(short_id)
    .bind(task.start_time)
    .bind(task.available_duration_secs)
    .bind(task.interval_secs)
    .bind(task.target_count)
    .bind(if task.optional { 1 } else { 0 })
    .bind(task.end_time)
    .fetch_one(pool)
    .await
    .context("Failed to create task")?;

    let id: i64 = row.get("id");
    Ok((id, short_id))
}

/// Update the recurring-task fields of an existing task. Returns the number
/// of affected rows.
pub async fn edit_task(pool: &SqlitePool, update: &UpdateTaskObject) -> Result<u64> {
    let res = sqlx::query(
        r#"UPDATE todos SET interval_secs = ?, available_duration_secs = ?, target_count = ?,
                   optional = ?, end_time = ? WHERE id = ?"#,
    )
    .bind(update.interval_secs)
    .bind(update.available_duration_secs)
    .bind(update.target_count)
    .bind(if update.optional { 1 } else { 0 })
    .bind(update.end_time)
    .bind(update.id)
    .execute(pool)
    .await
    .context("Failed to update recurring task")?;
    Ok(res.rows_affected())
}

/// Delete a task row; `todo_completions` rows cascade via `ON DELETE CASCADE`.
pub async fn delete_task(pool: &SqlitePool, id: i64) -> Result<u64> {
    let res = sqlx::query("DELETE FROM todos WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to delete task")?;
    Ok(res.rows_affected())
}

/// Apply a completion delta to a task and return the new total.
///
/// Positive deltas append a new completion event with the delta as its count;
/// negative deltas consume the most recent events within the current interval
/// (recurring tasks: entries from before the current interval started are
/// never touched, and the returned total is the sum within the current
/// interval only; oneshot tasks: full history).
///
/// After applying the delta the task's `short_id` is synced to its completion
/// state (see [`sync_short_id`]): a oneshot task that just completed loses
/// its short id; a oneshot task that just became not-done again is assigned
/// the smallest free one.
pub async fn update_task(pool: &SqlitePool, todo_id: i64, delta: i32) -> Result<i32> {
    // Determine the current interval boundary for recurring tasks so we never
    // touch completion events from before the current interval started.
    let interval_start: Option<i64> =
        sqlx::query("SELECT start_time, interval_secs FROM todos WHERE id = ?")
            .bind(todo_id)
            .fetch_optional(pool)
            .await?
            .and_then(|row| {
                let start: Option<i64> = row.get("start_time");
                let interval: Option<i64> = row.get("interval_secs");
                match (start, interval) {
                    (Some(st), Some(iv)) if iv > 0 => Some(crate::task::current_interval_start(
                        st,
                        iv,
                        crate::date::now(),
                    )),
                    _ => None,
                }
            });

    if delta > 0 {
        sqlx::query("INSERT INTO todo_completions (todo_id, time, count) VALUES (?, ?, ?)")
            .bind(todo_id)
            .bind(crate::date::now())
            .bind(delta)
            .execute(pool)
            .await?;
    } else if delta < 0 {
        let rows = match interval_start {
            Some(boundary) => sqlx::query(
                "SELECT id, count FROM todo_completions WHERE todo_id = ? AND time >= ? ORDER BY id ASC",
            )
            .bind(todo_id)
            .bind(boundary)
            .fetch_all(pool)
            .await?,
            None => sqlx::query(
                "SELECT id, count FROM todo_completions WHERE todo_id = ? ORDER BY id ASC",
            )
            .bind(todo_id)
            .fetch_all(pool)
            .await?,
        };
        let ids: Vec<i64> = rows.iter().map(|r| r.get("id")).collect();
        let counts: Vec<i32> = rows.iter().map(|r| r.get("count")).collect();
        let new_counts = crate::task::apply_delta_to_counts(&counts, delta);
        // Trailing entries were fully consumed → delete them in a single batch query.
        let to_delete = &ids[new_counts.len()..];
        if !to_delete.is_empty() {
            let sql = format!(
                "DELETE FROM todo_completions WHERE id IN ({})",
                to_delete.iter().map(|_| "?").collect::<Vec<_>>().join(",")
            );
            let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
            for id in to_delete {
                q = q.bind(id);
            }
            q.execute(pool).await?;
        }
        // The last surviving entry may have been partially reduced.
        if let Some(&nc) = new_counts.last() {
            let orig = counts[new_counts.len() - 1];
            if nc != orig {
                sqlx::query("UPDATE todo_completions SET count = ? WHERE id = ?")
                    .bind(nc)
                    .bind(ids[new_counts.len() - 1])
                    .execute(pool)
                    .await?;
            }
        }
    }
    // Return the new total: within the current interval for recurring tasks,
    // the full sum otherwise.
    let total: i32 = match interval_start {
        Some(boundary) => sqlx::query_scalar(
            "SELECT COALESCE(SUM(count), 0) FROM todo_completions WHERE todo_id = ? AND time >= ?",
        )
        .bind(todo_id)
        .bind(boundary)
        .fetch_one(pool)
        .await?,
        None => {
            sqlx::query_scalar(
                "SELECT COALESCE(SUM(count), 0) FROM todo_completions WHERE todo_id = ?",
            )
            .bind(todo_id)
            .fetch_one(pool)
            .await?
        }
    };
    sync_short_id(pool, todo_id).await?;
    Ok(total)
}

/// Delete completed oneshot tasks and expired (end_time passed) recurring
/// tasks in one `RETURNING` statement so the report reflects exactly the
/// rows deleted. A oneshot task counts as completed when its completion
/// state satisfies `is_task_done`.
pub async fn prune_tasks(pool: &SqlitePool, now: i64) -> Result<Vec<PrunedTask>> {
    let rows = sqlx::query(
        r#"DELETE FROM todos
           WHERE (interval_secs IS NULL AND (
                     (target_count <= 0 AND EXISTS(SELECT 1 FROM todo_completions tc
                                                    WHERE tc.todo_id = todos.id AND tc.count > 0))
                  OR (target_count > 0 AND COALESCE((SELECT SUM(count) FROM todo_completions tc
                                                      WHERE tc.todo_id = todos.id), 0) >= target_count)))
              OR (interval_secs IS NOT NULL
                  AND end_time IS NOT NULL
                  AND end_time < ?)
           RETURNING id, short_id, name,
                     CASE WHEN interval_secs IS NULL THEN 'completed'
                          ELSE 'expired' END AS reason"#,
    )
    .bind(now)
    .fetch_all(pool)
    .await
    .context("Failed to delete pruned tasks")?;

    Ok(rows
        .iter()
        .map(|row| PrunedTask {
            id: row.get("id"),
            short_id: row.get("short_id"),
            name: row.get("name"),
            reason: row.get("reason"),
        })
        .collect())
}

pub async fn prune_embedding_cache(pool: &SqlitePool) -> Result<u64> {
    let rows_affected = sqlx::query("DELETE FROM embedding_cache")
        .execute(pool)
        .await?
        .rows_affected();

    Ok(rows_affected)
}

/// Allocate the smallest free positive short id (>= 1) — the first gap in
/// `short_id` space across all rows. Completed oneshot tasks hold `NULL`
/// short ids, so their former ids are immediately free for reuse.
///
/// The allocation is a read-only query: the id is bound at INSERT/UPDATE
/// time, and the `short_id` column is `UNIQUE`, so a concurrent
/// double-allocation fails loudly rather than silently sharing an id. In
/// practice the CLI is single-threaded per invocation.
pub async fn allocate_short_id(pool: &SqlitePool) -> Result<i64> {
    let taken: Vec<i64> = sqlx::query_scalar(
        "SELECT short_id FROM todos WHERE short_id IS NOT NULL ORDER BY short_id ASC",
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch short ids for allocation")?;
    let mut expected = 1i64;
    for id in taken {
        if id == expected {
            expected += 1;
        } else if id > expected {
            break;
        }
    }
    Ok(expected)
}

/// Ensure a task's `short_id` reflects its completion state:
///
/// * A not-done task must have a short id — when it's `NULL`, the smallest
///   free positive id is allocated (first-available-gap, so untoggling a
///   completion reassigns the task's id).
/// * A done **oneshot** task must not have a short id: it is cleared on
///   completion, freeing the id for reuse. Recurring tasks keep their short
///   id across intervals — their "done" state is interval-scoped and
///   transient, so clearing/reassigning per interval would churn ids.
///
/// Completion state is evaluated with completions scoped to the current
/// interval for recurring tasks (matching [`update_task`]).
pub async fn sync_short_id(pool: &SqlitePool, todo_id: i64) -> Result<()> {
    let row = sqlx::query(
        "SELECT start_time, interval_secs, target_count, short_id FROM todos WHERE id = ?",
    )
    .bind(todo_id)
    .fetch_optional(pool)
    .await
    .context("Failed to fetch task for short-id sync")?;
    let Some(row) = row else { return Ok(()) };

    let start_time: Option<i64> = row.get("start_time");
    let interval_secs: Option<i64> = row.get("interval_secs");
    let target_count: i32 = row.get("target_count");
    let short_id: Option<i64> = row.get("short_id");

    let boundary = match (start_time, interval_secs) {
        (Some(st), Some(iv)) if iv > 0 => Some(crate::task::current_interval_start(
            st,
            iv,
            crate::date::now(),
        )),
        _ => None,
    };
    let sum: i32 = match boundary {
        Some(b) => sqlx::query_scalar(
            "SELECT COALESCE(SUM(count), 0) FROM todo_completions WHERE todo_id = ? AND time >= ?",
        )
        .bind(todo_id)
        .bind(b)
        .fetch_one(pool)
        .await?,
        None => {
            sqlx::query_scalar(
                "SELECT COALESCE(SUM(count), 0) FROM todo_completions WHERE todo_id = ?",
            )
            .bind(todo_id)
            .fetch_one(pool)
            .await?
        }
    };

    // Recurring tasks never lose their short id; only oneshot tasks do.
    if interval_secs.is_some() {
        if short_id.is_none() {
            let new_id = allocate_short_id(pool).await?;
            sqlx::query("UPDATE todos SET short_id = ? WHERE id = ?")
                .bind(new_id)
                .bind(todo_id)
                .execute(pool)
                .await
                .context("Failed to assign short id")?;
        }
        return Ok(());
    }

    let done = crate::task::is_task_done(target_count, Some(sum));
    match (done, short_id) {
        (true, Some(_)) => {
            sqlx::query("UPDATE todos SET short_id = NULL WHERE id = ?")
                .bind(todo_id)
                .execute(pool)
                .await
                .context("Failed to clear short id")?;
        }
        (false, None) => {
            let new_id = allocate_short_id(pool).await?;
            sqlx::query("UPDATE todos SET short_id = ? WHERE id = ?")
                .bind(new_id)
                .bind(todo_id)
                .execute(pool)
                .await
                .context("Failed to assign short id")?;
        }
        _ => {}
    }
    Ok(())
}

/// The current short id of a task (`None` once a oneshot task is completed).
pub async fn fetch_task_short_id(pool: &SqlitePool, id: i64) -> Result<Option<i64>> {
    let short_id: Option<i64> =
        sqlx::query_scalar::<_, Option<i64>>("SELECT short_id FROM todos WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .context("Failed to fetch short id")?;
    Ok(short_id)
}

/// Whether a recurring task with the given name already exists.
pub async fn recurring_task_name_exists(pool: &SqlitePool, name: &str) -> Result<bool> {
    let count: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM todos WHERE name = ? AND interval_secs IS NOT NULL",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .context("Failed to check task name uniqueness")?;
    Ok(count > 0)
}

// ---------------------------------------------------------------------------
// Entry insertion
// ---------------------------------------------------------------------------

/// Insert a mood entry and its linked custom tracker values in one
/// transaction. For Text/Float interval trackers, `replace_slot` deletes the
/// previous entry in the same interval slot before inserting. Returns the
/// feeling row id, or `None` when no feeling row was inserted (custom-only
/// entry).
pub async fn create_entry(pool: &SqlitePool, entry: &EntryObject) -> Result<Option<i64>> {
    let mut tx = pool.begin().await.context("Failed to begin transaction")?;

    let insert_feeling = !entry.mood.is_empty() || !entry.body.is_empty();
    let feeling_id: Option<i64> = if insert_feeling {
        let id: i64 = if let Some(blob) = &entry.embedding {
            sqlx::query(
                "INSERT INTO feeling (mood, body, time, embedding) VALUES (?, ?, ?, ?) RETURNING id",
            )
            .bind(&entry.mood)
            .bind(&entry.body)
            .bind(entry.time)
            .bind(blob)
            .fetch_one(&mut *tx)
            .await
            .context("Failed to insert feeling")?
            .get("id")
        } else {
            sqlx::query("INSERT INTO feeling (mood, body, time) VALUES (?, ?, ?) RETURNING id")
                .bind(&entry.mood)
                .bind(&entry.body)
                .bind(entry.time)
                .fetch_one(&mut *tx)
                .await
                .context("Failed to insert feeling")?
                .get("id")
        };
        Some(id)
    } else {
        None
    };

    for custom in &entry.customs {
        if let Some((slot_start, slot_end)) = custom.replace_slot {
            sqlx::query("DELETE FROM custom WHERE type = ? AND time >= ? AND time < ?")
                .bind(&custom.tracker_type)
                .bind(slot_start)
                .bind(slot_end)
                .execute(&mut *tx)
                .await
                .with_context(|| {
                    format!(
                        "Failed to delete old entry for tracker '{}' in slot {}..{}",
                        custom.tracker_type, slot_start, slot_end
                    )
                })?;
        }

        let mut q =
            sqlx::query("INSERT INTO custom (type, score, time, feeling) VALUES (?, ?, ?, ?)")
                .bind(&custom.tracker_type);
        q = match &custom.value {
            CustomValue::Text(s) => q.bind(s),
            CustomValue::Number(n) => q.bind(n),
            CustomValue::Float(f) => q.bind(f),
        };
        q.bind(entry.time)
            .bind(feeling_id)
            .execute(&mut *tx)
            .await
            .with_context(|| {
                format!("Failed to insert custom tracker '{}'", custom.tracker_type)
            })?;
    }

    tx.commit().await.context("Failed to commit transaction")?;
    Ok(feeling_id)
}

/// Count mood entries in `[start_time, end_time]`; when `delete` is true,
/// delete them (plus their linked custom rows, in a transaction) and return
/// the number deleted instead.
pub async fn clear_moods(
    pool: &SqlitePool,
    start_time: i64,
    end_time: i64,
    delete: bool,
) -> Result<usize> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM feeling WHERE time >= ? AND time <= ?")
            .bind(start_time)
            .bind(end_time)
            .fetch_one(pool)
            .await
            .context("Failed to count mood entries")?;

    if !delete {
        return Ok(count as usize);
    }

    let mut tx = pool.begin().await.context("Failed to begin transaction")?;

    sqlx::query(
        "DELETE FROM custom WHERE feeling IN (SELECT id FROM feeling WHERE time >= ? AND time <= ?)",
    )
    .bind(start_time)
    .bind(end_time)
    .execute(&mut *tx)
    .await
    .context("Failed to delete linked custom entries")?;

    let res = sqlx::query("DELETE FROM feeling WHERE time >= ? AND time <= ?")
        .bind(start_time)
        .bind(end_time)
        .execute(&mut *tx)
        .await
        .context("Failed to delete mood entries")?;

    tx.commit().await.context("Failed to commit transaction")?;
    Ok(res.rows_affected() as usize)
}

// ---------------------------------------------------------------------------
// Fetch helpers
// ---------------------------------------------------------------------------

/// Feelings in `[start, end]`, oldest first.
pub async fn fetch_feelings_between(
    pool: &SqlitePool,
    start: i64,
    end: i64,
) -> Result<Vec<FeelingRow>> {
    let rows = sqlx::query(
        "SELECT id, mood, body, time, embedding FROM feeling WHERE time >= ? AND time <= ? ORDER BY time ASC",
    )
    .bind(start)
    .bind(end)
    .fetch_all(pool)
    .await
    .context("Failed to fetch feeling entries")?;

    Ok(rows
        .iter()
        .map(|row| FeelingRow {
            id: row.get("id"),
            mood: row.get("mood"),
            body: row.get("body"),
            time: row.get("time"),
            embedding: row.get("embedding"),
        })
        .collect())
}

/// Custom-tracker entries of one tracker in `[start, end]`, oldest first.
pub async fn fetch_customs_for_tracker(
    pool: &SqlitePool,
    tracker_type: &str,
    start: i64,
    end: i64,
) -> Result<Vec<CustomEntryRow>> {
    let rows = sqlx::query(
        "SELECT id, type, CAST(score AS TEXT) AS score, time FROM custom WHERE type = ? AND time >= ? AND time <= ? ORDER BY time ASC",
    )
    .bind(tracker_type)
    .bind(start)
    .bind(end)
    .fetch_all(pool)
    .await
    .context("Failed to fetch custom tracker entries")?;

    Ok(rows
        .iter()
        .map(|row| CustomEntryRow {
            id: row.get("id"),
            tracker_type: row.get("type"),
            score: row.get("score"),
            time: row.get("time"),
        })
        .collect())
}

/// All custom-tracker entries in `[start, end]`, oldest first (today view).
pub async fn fetch_customs_today(
    pool: &SqlitePool,
    start: i64,
    end: i64,
) -> Result<Vec<CustomEntryRow>> {
    let rows = sqlx::query(
        "SELECT id, type, CAST(score AS TEXT) AS score, time FROM custom WHERE time >= ? AND time <= ? ORDER BY time ASC",
    )
    .bind(start)
    .bind(end)
    .fetch_all(pool)
    .await
    .context("Failed to fetch today's custom entries")?;

    Ok(rows
        .iter()
        .map(|row| CustomEntryRow {
            id: row.get("id"),
            tracker_type: row.get("type"),
            score: row.get("score"),
            time: row.get("time"),
        })
        .collect())
}

/// Recurring task metadata for the completion-dots tracker; accepts either a
/// numeric short id or the unique task name.
pub async fn fetch_recurring_task_meta(
    pool: &SqlitePool,
    name_or_id: &str,
) -> Result<Option<RecurringTaskMeta>> {
    let row = if let Ok(short_id) = name_or_id.parse::<i64>() {
        sqlx::query(
            "SELECT id, interval_secs, target_count FROM todos WHERE short_id = ? AND interval_secs IS NOT NULL",
        )
        .bind(short_id)
        .fetch_optional(pool)
        .await
        .context("Failed to fetch recurring task")?
    } else {
        sqlx::query(
            "SELECT id, interval_secs, target_count FROM todos WHERE name = ? AND interval_secs IS NOT NULL",
        )
        .bind(name_or_id)
        .fetch_optional(pool)
        .await
        .context("Failed to fetch recurring task")?
    };

    Ok(row.map(|r| RecurringTaskMeta {
        id: r.get("id"),
        interval_secs: r.get("interval_secs"),
        target_count: r.get("target_count"),
    }))
}

/// Completion events (time, count) for a task in `[start, end]`.
pub async fn fetch_completions_between(
    pool: &SqlitePool,
    task_id: i64,
    start: i64,
    end: i64,
) -> Result<Vec<CompletionRow>> {
    let rows = sqlx::query(
        "SELECT time, count FROM todo_completions WHERE todo_id = ? AND time >= ? AND time <= ? ORDER BY time ASC",
    )
    .bind(task_id)
    .bind(start)
    .bind(end)
    .fetch_all(pool)
    .await
    .context("Failed to fetch completion events")?;

    Ok(rows
        .iter()
        .map(|row| CompletionRow {
            time: row.get("time"),
            count: row.get("count"),
        })
        .collect())
}

/// Incomplete oneshot tasks due by `horizon_end` (and >= `floor`, which
/// excludes overdue tasks unless the config opts in).
pub async fn fetch_due_oneshot_tasks(
    pool: &SqlitePool,
    horizon_end: i64,
    floor: i64,
) -> Result<Vec<TaskRow>> {
    // available_duration_secs IS NULL excludes scheduled tasks — the today
    // view fetches those separately via fetch_scheduled_today. Completed
    // tasks are included too: their row carries the ✓ badge (the today view
    // no longer emits separate completion rows).
    let tasks = sqlx::query_as::<_, TaskRow>(
        r#"SELECT t.*, SUM(tc.count) AS completions
           FROM todos t
           LEFT JOIN todo_completions tc ON tc.todo_id = t.id
           WHERE t.interval_secs IS NULL
           AND t.available_duration_secs IS NULL
           AND t.start_time <= ?
           AND t.start_time >= ?
           GROUP BY t.id
           ORDER BY t.priority DESC, t.start_time ASC"#,
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
        r#"SELECT t.*, SUM(tc.count) AS completions
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
pub fn recurring_available(task: &TaskRow, now: i64) -> bool {
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

/// Active recurring tasks with completions scoped to the current interval,
/// filtered to those currently within their availability window.
pub async fn fetch_active_recurring_tasks(pool: &SqlitePool, now: i64) -> Result<Vec<TaskRow>> {
    // Completed tasks are included too: their row carries the ✓ badge (the
    // today view no longer emits separate completion rows).
    let tasks = sqlx::query_as::<_, TaskRow>(
        r#"SELECT t.*, SUM(tc.count) AS completions
           FROM todos t
           LEFT JOIN todo_completions tc ON tc.todo_id = t.id
               AND tc.time >= CASE
                   WHEN t.interval_secs IS NOT NULL AND t.start_time IS NOT NULL THEN
                       CASE WHEN ? <= t.start_time THEN t.start_time
                            ELSE t.start_time + ((? - t.start_time) / t.interval_secs) * t.interval_secs END
                   ELSE 0 END
           WHERE t.interval_secs IS NOT NULL
           AND (t.end_time IS NULL OR t.end_time > ?)
           GROUP BY t.id
           ORDER BY t.priority DESC"#,
    )
    .bind(now)
    .bind(now)
    .bind(now)
    .fetch_all(pool)
    .await
    .context("Failed to fetch active recurring tasks")?;

    Ok(tasks
        .into_iter()
        .filter(|t| recurring_available(t, now))
        .collect())
}

/// Delete a custom tracker entry row.
pub async fn delete_custom(pool: &SqlitePool, id: i64) -> Result<u64> {
    let result = sqlx::query("DELETE FROM custom WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to delete custom row")?;
    Ok(result.rows_affected())
}

/// The full row for one task, with completions scoped to the current
/// interval for recurring tasks (TUI today-view selection).
pub async fn fetch_task_by_id(pool: &SqlitePool, id: i64, now: i64) -> Result<Option<TaskRow>> {
    let row = sqlx::query_as::<_, TaskRow>(
        r#"SELECT t.*, (SELECT SUM(tc.count) FROM todo_completions tc
                           WHERE tc.todo_id = t.id
                           AND tc.time >= CASE
                               WHEN t.interval_secs IS NOT NULL AND t.start_time IS NOT NULL THEN
                                   CASE WHEN ? <= t.start_time THEN t.start_time
                                        ELSE t.start_time + ((? - t.start_time) / t.interval_secs) * t.interval_secs END
                               ELSE 0 END) AS completions
               FROM todos t WHERE t.id = ?"#,
    )
    .bind(now)
    .bind(now)
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("Failed to fetch task")?;
    Ok(row)
}

/// Full recurring task (edit flow), looked up by name.
pub async fn fetch_recurring_task_by_name(
    pool: &SqlitePool,
    name: &str,
) -> Result<Option<TaskObject>> {
    let row = sqlx::query(
        r#"SELECT id, name, body, priority, short_id, start_time,
                   available_duration_secs, interval_secs, target_count,
                   optional, end_time
           FROM todos WHERE name = ? AND interval_secs IS NOT NULL"#,
    )
    .bind(name)
    .fetch_optional(pool)
    .await
    .context("Failed to fetch recurring task")?;

    Ok(row.map(|r| TaskObject {
        id: Some(r.get("id")),
        short_id: r.get("short_id"),
        name: r.get("name"),
        body: r.get("body"),
        priority: r.get("priority"),
        start_time: r.get("start_time"),
        available_duration_secs: r.get("available_duration_secs"),
        interval_secs: r.get("interval_secs"),
        target_count: r.get("target_count"),
        optional: r.get::<i32, _>("optional") != 0,
        end_time: r.get("end_time"),
    }))
}

/// Oneshot task + prior completion count for the `- <short-id> [count]`
/// update command, looked up by its user-facing short id. Completed oneshot
/// tasks have no short id, so they are not addressable by id (use the word
/// query form instead).
pub async fn fetch_oneshot_task_for_update(
    pool: &SqlitePool,
    short_id: i64,
) -> Result<Option<TaskUpdateInfo>> {
    let row = sqlx::query(
        r#"SELECT id, name, target_count, short_id,
                  COALESCE((SELECT SUM(count) FROM todo_completions
                            WHERE todo_id = todos.id), 0) AS prior_completions
           FROM todos WHERE short_id = ? AND interval_secs IS NULL"#,
    )
    .bind(short_id)
    .fetch_optional(pool)
    .await
    .context("Failed to fetch task")?;

    Ok(row.map(|r| TaskUpdateInfo {
        id: r.get("id"),
        short_id: r.get("short_id"),
        name: r.get("name"),
        target_count: r.get("target_count"),
        prior_completions: r.get("prior_completions"),
    }))
}

/// Oneshot tasks whose names contain all `words` in order (a subsequence
/// match over whitespace-split words), with prior completion counts — the
/// candidates for the `feeling - <words…> [count]` update form. The
/// subsequence test is done here in Rust: SQL `LIKE` can't express
/// "in order, with gaps allowed".
pub async fn fetch_oneshot_matching_words(
    pool: &SqlitePool,
    words: &[String],
) -> Result<Vec<TaskUpdateInfo>> {
    let rows = sqlx::query(
        r#"SELECT id, name, target_count, short_id,
                  COALESCE((SELECT SUM(count) FROM todo_completions
                            WHERE todo_id = todos.id), 0) AS prior_completions
           FROM todos WHERE interval_secs IS NULL"#,
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch tasks for word query")?;

    Ok(rows
        .into_iter()
        .filter(|r| {
            let name: String = r.get("name");
            name_contains_words_in_order(&name, words)
        })
        .map(|r| TaskUpdateInfo {
            id: r.get("id"),
            short_id: r.get("short_id"),
            name: r.get("name"),
            target_count: r.get("target_count"),
            prior_completions: r.get("prior_completions"),
        })
        .collect())
}

/// True iff every word in `words` appears in `name` as a whitespace-
/// separated word, in the same relative order (extra words in between are
/// allowed). Empty `words` never matches.
fn name_contains_words_in_order(name: &str, words: &[String]) -> bool {
    if words.is_empty() {
        return false;
    }
    let mut wi = 0;
    for nw in name.split_whitespace() {
        if wi < words.len() && nw == words[wi] {
            wi += 1;
        }
    }
    wi == words.len()
}

/// Task rows for a view mode, with per-mode completion scoping. Shared by the
/// CLI (`views::handle_view`) and the TUI tasks app.
///
/// `include_completed` drops the not-done filter of each mode; `include_scheduled`
/// merges scheduled tasks into the `!`, `@` and `@due` views (and replaces
/// `@done` with scheduled-only rows). See VIEWS.md for the full matrix.
pub async fn fetch_tasks_for_view(
    pool: &SqlitePool,
    mode: ViewMode,
    include_completed: bool,
    include_scheduled: bool,
) -> Result<Vec<TaskRow>> {
    match mode {
        ViewMode::OneShotTasks => {
            // Incomplete oneshot tasks; with include_scheduled, ongoing
            // scheduled tasks (window not yet elapsed) are merged in. The
            // HAVING clause doubles for scheduled rows: target_count is 0,
            // so any completion entry excludes them.
            let now = crate::date::now();
            let (sql, bind_now) = match (include_scheduled, include_completed) {
                (true, true) => (
                    r#"SELECT t.*, SUM(tc.count) AS completions
                       FROM todos t
                       LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                       WHERE (t.interval_secs IS NULL AND t.available_duration_secs IS NULL)
                          OR (t.interval_secs IS NULL AND t.available_duration_secs IS NOT NULL
                              AND t.start_time + t.available_duration_secs >= ?)
                       GROUP BY t.id
                       ORDER BY t.priority DESC, t.start_time ASC"#,
                    true,
                ),
                (true, false) => (
                    r#"SELECT t.*, SUM(tc.count) AS completions
                       FROM todos t
                       LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                       WHERE (t.interval_secs IS NULL AND t.available_duration_secs IS NULL)
                          OR (t.interval_secs IS NULL AND t.available_duration_secs IS NOT NULL
                              AND t.start_time + t.available_duration_secs >= ?)
                       GROUP BY t.id
                       HAVING completions IS NULL OR completions < t.target_count
                       ORDER BY t.priority DESC, t.start_time ASC"#,
                    true,
                ),
                (false, true) => (
                    r#"SELECT t.*, SUM(tc.count) AS completions
                       FROM todos t
                       LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                       WHERE t.interval_secs IS NULL AND t.available_duration_secs IS NULL
                       GROUP BY t.id
                       ORDER BY t.priority DESC, t.start_time ASC"#,
                    false,
                ),
                (false, false) => (
                    r#"SELECT t.*, SUM(tc.count) AS completions
                       FROM todos t
                       LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                       WHERE t.interval_secs IS NULL AND t.available_duration_secs IS NULL
                       GROUP BY t.id
                       HAVING completions IS NULL OR completions < t.target_count
                       ORDER BY t.priority DESC, t.start_time ASC"#,
                    false,
                ),
            };
            let mut q = sqlx::query_as::<_, TaskRow>(sql);
            if bind_now {
                q = q.bind(now);
            }
            let tasks = q
                .fetch_all(pool)
                .await
                .context("Failed to fetch oneshot tasks")?;
            Ok(tasks)
        }
        ViewMode::RecurringTasks => {
            // Active recurring tasks, plus ongoing scheduled tasks when
            // include_scheduled is set. Completions are scoped to the
            // current interval; without include_completed tasks already
            // done in the current interval are excluded. Scheduled rows
            // fall to the unscoped (ELSE 0) completion branch, so their
            // entry-less state is enforced by the same HAVING clause.
            let now = crate::date::now();
            let (sql, bind_scheduled) = match (include_scheduled, include_completed) {
                (true, true) => (
                    r#"SELECT t.*, SUM(tc.count) AS completions
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
                       GROUP BY t.id
                       ORDER BY t.priority DESC, t.start_time ASC"#,
                    true,
                ),
                (true, false) => (
                    r#"SELECT t.*, SUM(tc.count) AS completions
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
                       GROUP BY t.id
                       HAVING completions IS NULL OR completions < t.target_count
                       ORDER BY t.priority DESC, t.start_time ASC"#,
                    true,
                ),
                (false, true) => (
                    r#"SELECT t.*, SUM(tc.count) AS completions
                       FROM todos t
                       LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                           AND tc.time >= CASE
                               WHEN t.interval_secs IS NOT NULL AND t.start_time IS NOT NULL THEN
                                   CASE WHEN ? <= t.start_time THEN t.start_time
                                        ELSE t.start_time + ((? - t.start_time) / t.interval_secs) * t.interval_secs END
                               ELSE 0 END
                       WHERE t.interval_secs IS NOT NULL
                       AND (t.end_time IS NULL OR t.end_time > ?)
                       GROUP BY t.id
                       ORDER BY t.priority DESC, t.start_time ASC"#,
                    false,
                ),
                (false, false) => (
                    r#"SELECT t.*, SUM(tc.count) AS completions
                       FROM todos t
                       LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                           AND tc.time >= CASE
                               WHEN t.interval_secs IS NOT NULL AND t.start_time IS NOT NULL THEN
                                   CASE WHEN ? <= t.start_time THEN t.start_time
                                        ELSE t.start_time + ((? - t.start_time) / t.interval_secs) * t.interval_secs END
                               ELSE 0 END
                       WHERE t.interval_secs IS NOT NULL
                       AND (t.end_time IS NULL OR t.end_time > ?)
                       GROUP BY t.id
                       HAVING completions IS NULL OR completions < t.target_count
                       ORDER BY t.priority DESC, t.start_time ASC"#,
                    false,
                ),
            };
            let mut q = sqlx::query_as::<_, TaskRow>(sql).bind(now).bind(now);
            if bind_scheduled {
                q = q.bind(now).bind(now);
            } else {
                q = q.bind(now);
            }
            let tasks = q
                .fetch_all(pool)
                .await
                .context("Failed to fetch recurring tasks")?;

            // Keep only tasks currently within their availability window
            Ok(tasks
                .into_iter()
                .filter(|t| recurring_available(t, now))
                .collect())
        }
        ViewMode::DoneTasks => {
            // include_scheduled replaces the view with scheduled-only rows:
            // resolved scheduled tasks. Without include_completed those are
            // the auto-completed ones (window fully elapsed, no entry); with
            // it, exactly the scheduled tasks that carry a completion entry
            // (completed early/on time, or marked failed) — no window bound.
            // See VIEWS.md.
            let now = crate::date::now();
            if include_scheduled {
                let sql = if include_completed {
                    r#"SELECT t.*, SUM(tc.count) AS completions, MAX(tc.time) AS last_time
                       FROM todos t
                       LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                       WHERE t.interval_secs IS NULL
                         AND t.available_duration_secs IS NOT NULL
                       GROUP BY t.id
                       HAVING completions IS NOT NULL
                       ORDER BY t.start_time DESC"#
                } else {
                    r#"SELECT t.*, SUM(tc.count) AS completions, MAX(tc.time) AS last_time
                       FROM todos t
                       LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                       WHERE t.interval_secs IS NULL
                         AND t.available_duration_secs IS NOT NULL
                         AND t.start_time + t.available_duration_secs < ?
                       GROUP BY t.id
                       HAVING completions IS NULL
                       ORDER BY t.start_time DESC"#
                };
                let mut q = sqlx::query_as::<_, TaskRow>(sql);
                if !include_completed {
                    q = q.bind(now);
                }
                let tasks = q
                    .fetch_all(pool)
                    .await
                    .context("Failed to fetch done scheduled tasks")?;
                Ok(tasks)
            } else {
                // include_completed=true: done oneshot tasks plus recurring
                // tasks complete within their current interval (and inside
                // their active window). include_completed=false: done
                // oneshot tasks only. The available_duration_secs guard and
                // the interval_secs IS NOT NULL on the recurring branch keep
                // scheduled rows from leaking into either variant.
                let tasks = if include_completed {
                    sqlx::query_as::<_, TaskRow>(
                        r#"SELECT t.*, SUM(tc.count) AS completions, MAX(tc.time) AS last_time
                           FROM todos t
                           LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                               AND tc.time >= CASE
                                   WHEN t.interval_secs IS NOT NULL AND t.start_time IS NOT NULL THEN
                                       CASE WHEN ? <= t.start_time THEN t.start_time
                                            ELSE t.start_time + ((? - t.start_time) / t.interval_secs) * t.interval_secs END
                                   ELSE 0 END
                           WHERE (t.interval_secs IS NULL AND t.available_duration_secs IS NULL)
                              OR (t.interval_secs IS NOT NULL AND (t.start_time IS NULL OR t.start_time <= ?) AND (t.end_time IS NULL OR t.end_time > ?))
                           GROUP BY t.id
                           HAVING completions IS NOT NULL AND completions >= t.target_count
                           ORDER BY COALESCE(MAX(tc.time), t.start_time) DESC"#,
                    )
                    .bind(now)
                    .bind(now)
                    .bind(now)
                    .bind(now)
                    .fetch_all(pool)
                    .await
                    .context("Failed to fetch done tasks")?
                } else {
                    sqlx::query_as::<_, TaskRow>(
                        r#"SELECT t.*, SUM(tc.count) AS completions, MAX(tc.time) AS last_time
                           FROM todos t
                           LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                           WHERE t.interval_secs IS NULL
                             AND t.available_duration_secs IS NULL
                           GROUP BY t.id
                           HAVING completions IS NOT NULL AND completions >= t.target_count
                           ORDER BY COALESCE(MAX(tc.time), t.start_time) DESC"#,
                    )
                    .fetch_all(pool)
                    .await
                    .context("Failed to fetch done tasks")?
                };
                Ok(tasks)
            }
        }
        ViewMode::DueTasks => {
            // Tasks due today or overdue: oneshot tasks with start_time <=
            // today, plus scheduled tasks (the same query — interval_secs
            // IS NULL already matches them) when include_scheduled is set.
            // The HAVING clause keeps only entry-less scheduled rows unless
            // include_completed drops it. See VIEWS.md.
            let today_end = crate::date::today_end();
            let sql = if include_scheduled {
                if include_completed {
                    r#"SELECT t.*, SUM(tc.count) AS completions
                       FROM todos t
                       LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                       WHERE t.interval_secs IS NULL
                       AND t.start_time <= ?
                       GROUP BY t.id
                       ORDER BY t.start_time ASC, t.priority DESC"#
                } else {
                    r#"SELECT t.*, SUM(tc.count) AS completions
                       FROM todos t
                       LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                       WHERE t.interval_secs IS NULL
                       AND t.start_time <= ?
                       GROUP BY t.id
                       HAVING completions IS NULL OR completions < t.target_count
                       ORDER BY t.start_time ASC, t.priority DESC"#
                }
            } else if include_completed {
                r#"SELECT t.*, SUM(tc.count) AS completions
                   FROM todos t
                   LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                   WHERE t.interval_secs IS NULL
                   AND t.available_duration_secs IS NULL
                   AND t.start_time <= ?
                   GROUP BY t.id
                   ORDER BY t.start_time ASC, t.priority DESC"#
            } else {
                r#"SELECT t.*, SUM(tc.count) AS completions
                   FROM todos t
                   LEFT JOIN todo_completions tc ON tc.todo_id = t.id
                   WHERE t.interval_secs IS NULL
                   AND t.available_duration_secs IS NULL
                   AND t.start_time <= ?
                   GROUP BY t.id
                   HAVING completions IS NULL OR completions < t.target_count
                   ORDER BY t.start_time ASC, t.priority DESC"#
            };
            let tasks = sqlx::query_as::<_, TaskRow>(sql)
                .bind(today_end)
                .fetch_all(pool)
                .await
                .context("Failed to fetch due tasks")?;
            Ok(tasks)
        }
    }
}

// ---------------------------------------------------------------------------
// Edits / deletes from the TUI
// ---------------------------------------------------------------------------

/// Update a todo's body. Returns the number of affected rows.
pub async fn update_todo_body(pool: &SqlitePool, id: i64, body: &str) -> Result<u64> {
    let res = sqlx::query("UPDATE todos SET body = ? WHERE id = ?")
        .bind(body)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to update task body")?;
    Ok(res.rows_affected())
}

/// Update a feeling's body. Returns the number of affected rows.
pub async fn update_feeling_body(pool: &SqlitePool, id: i64, body: &str) -> Result<u64> {
    let res = sqlx::query("UPDATE feeling SET body = ? WHERE id = ?")
        .bind(body)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to update feeling body")?;
    Ok(res.rows_affected())
}

/// Update a custom entry's score, binding the value per its configured kind
/// (Text → string, Number → i64, Float → f64). Returns affected rows.
pub async fn update_custom_score(
    pool: &SqlitePool,
    id: i64,
    kind: TrackerType,
    value: &str,
) -> Result<u64> {
    let mut q = sqlx::query("UPDATE custom SET score = ? WHERE id = ?");
    q = match kind {
        TrackerType::Text => q.bind(value),
        TrackerType::Number => q.bind(value.parse::<i64>().unwrap_or(0)),
        TrackerType::Float => q.bind(value.parse::<f64>().unwrap_or(0.0)),
    };
    let res = q
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to update custom score")?;
    Ok(res.rows_affected())
}

/// Backfill a feeling row's stored embedding.
pub async fn update_feeling_embedding(pool: &SqlitePool, id: i64, blob: &[u8]) -> Result<u64> {
    let res = sqlx::query("UPDATE feeling SET embedding = ? WHERE id = ?")
        .bind(blob)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to update feeling embedding")?;
    Ok(res.rows_affected())
}

/// Delete a feeling row and any linked custom rows in a transaction
/// (`custom.feeling` has a FK with no `ON DELETE CASCADE`).
pub async fn delete_feeling(pool: &SqlitePool, id: i64) -> Result<()> {
    let mut tx = pool.begin().await.context("Failed to begin transaction")?;

    sqlx::query("DELETE FROM custom WHERE feeling = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("Failed to delete linked custom rows")?;

    sqlx::query("DELETE FROM feeling WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("Failed to delete feeling row")?;

    tx.commit().await.context("Failed to commit transaction")?;
    Ok(())
}

/// Set a scheduled task's completion entry, replacing any existing one.
/// Scheduled tasks keep at most one completion row: `value` 1 = completed
/// (early, or auto-completed by window elapse), 0 = failed (marked as
/// missed). Runs in a transaction so the replace is atomic, then syncs the
/// short id (a completed task loses its short id; a failed one keeps it).
pub async fn set_scheduled_completion(pool: &SqlitePool, todo_id: i64, value: i32) -> Result<()> {
    let mut tx = pool.begin().await.context("Failed to begin transaction")?;

    sqlx::query("DELETE FROM todo_completions WHERE todo_id = ?")
        .bind(todo_id)
        .execute(&mut *tx)
        .await
        .context("Failed to clear scheduled task completion")?;

    sqlx::query("INSERT INTO todo_completions (todo_id, time, count) VALUES (?, ?, ?)")
        .bind(todo_id)
        .bind(crate::date::now())
        .bind(value)
        .execute(&mut *tx)
        .await
        .context("Failed to insert scheduled task completion")?;

    tx.commit().await.context("Failed to commit transaction")?;

    sync_short_id(pool, todo_id).await?;
    Ok(())
}

/// Clear a task's completion progress. For recurring tasks only completions
/// at/after `floor` (the current interval start) are removed, preserving
/// history from earlier intervals. Returns affected rows.
pub async fn reset_task_completions(pool: &SqlitePool, id: i64, floor: Option<i64>) -> Result<u64> {
    let res = match floor {
        Some(floor) => sqlx::query("DELETE FROM todo_completions WHERE todo_id = ? AND time >= ?")
            .bind(id)
            .bind(floor)
            .execute(pool)
            .await
            .context("Failed to reset task progress")?,
        None => sqlx::query("DELETE FROM todo_completions WHERE todo_id = ?")
            .bind(id)
            .execute(pool)
            .await
            .context("Failed to reset task progress")?,
    };
    // Removing completion rows may untoggle a completed task — sync its
    // short id (a not-done task is reassigned the smallest free id).
    sync_short_id(pool, id).await?;
    Ok(res.rows_affected())
}

// ---------------------------------------------------------------------------
// Embedding cache
// ---------------------------------------------------------------------------

/// Look up a cached embedding BLOB by cache key (prefix + text).
pub async fn get_embedding_cache(pool: &SqlitePool, text: &str) -> Result<Option<Vec<u8>>> {
    let row = sqlx::query("SELECT embedding FROM embedding_cache WHERE text = ?")
        .bind(text)
        .fetch_optional(pool)
        .await
        .context("Failed to query embedding cache")?;
    Ok(row.map(|r| r.get("embedding")))
}

/// Insert or replace a cache entry. Returns affected rows.
pub async fn set_embedding_cache(pool: &SqlitePool, text: &str, blob: &[u8]) -> Result<u64> {
    let res = sqlx::query("INSERT OR REPLACE INTO embedding_cache (text, embedding) VALUES (?, ?)")
        .bind(text)
        .bind(blob)
        .execute(pool)
        .await
        .context("Failed to write embedding cache")?;
    Ok(res.rows_affected())
}
