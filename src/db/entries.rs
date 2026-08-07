use anyhow::{Context, Result};
use sqlx::{Row, SqlitePool};

use super::models::{
    CompletionRow, EntryObject, FeelingRow, RecurringTaskMeta, TrackerEntryRow, TrackerValue,
};
use crate::config::TrackerKind;

/// Insert a mood entry and its linked tracker values in one transaction.
/// For Text/Float interval trackers, `replace_slot` deletes the previous
/// entry in the same interval slot before inserting. Returns the feeling
/// row id, or `None` when no feeling row was inserted (tracker-only entry).
pub async fn create_entry(pool: &SqlitePool, entry: &EntryObject) -> Result<Option<i64>> {
    let mut tx = pool.begin().await.context("Failed to begin transaction")?;

    let insert_feeling = !entry.mood.is_empty() || !entry.body.is_empty();
    let feeling_id: Option<i64> = if insert_feeling {
        let id: i64 = if let Some(blob) = &entry.embedding {
            sqlx::query(
                "INSERT INTO feeling (mood, body, time, embedding, score) VALUES (?, ?, ?, ?, ?) RETURNING id",
            )
            .bind(&entry.mood)
            .bind(&entry.body)
            .bind(entry.time)
            .bind(blob)
            .bind(entry.score)
            .fetch_one(&mut *tx)
            .await
            .context("Failed to insert feeling")?
            .get("id")
        } else {
            sqlx::query(
                "INSERT INTO feeling (mood, body, time, score) VALUES (?, ?, ?, ?) RETURNING id",
            )
            .bind(&entry.mood)
            .bind(&entry.body)
            .bind(entry.time)
            .bind(entry.score)
            .fetch_one(&mut *tx)
            .await
            .context("Failed to insert feeling")?
            .get("id")
        };
        Some(id)
    } else {
        None
    };

    for tracker in &entry.trackers {
        // Null trackers: update the slot's existing entry in place (time
        // moves to the new entry's; the score is incremented in count mode
        // and left unchanged when both min/max are set), or insert when the
        // slot is empty.
        if let Some(nu) = &tracker.null_upsert {
            let (slot_start, slot_end) = nu.slot;
            let existing: Option<i64> = sqlx::query(
                "SELECT id FROM tracker \
                 WHERE type = ? AND time >= ? AND time < ? ORDER BY time DESC LIMIT 1",
            )
            .bind(&tracker.tracker_type)
            .bind(slot_start)
            .bind(slot_end)
            .fetch_optional(&mut *tx)
            .await
            .with_context(|| {
                format!(
                    "Failed to find existing entry for tracker '{}' in slot {}..{}",
                    tracker.tracker_type, slot_start, slot_end
                )
            })?
            .map(|r| r.get("id"));
            match existing {
                Some(id) => {
                    if nu.increment {
                        // Count mode: score + 1, time moves to now.
                        sqlx::query("UPDATE tracker SET score = score + 1, time = ? WHERE id = ?")
                            .bind(entry.time)
                            .bind(id)
                            .execute(&mut *tx)
                            .await
                            .with_context(|| {
                                format!(
                                    "Failed to increment tracker '{}' entry {}",
                                    tracker.tracker_type, id
                                )
                            })?;
                    } else {
                        // Time-marker mode: just move the entry to now.
                        sqlx::query("UPDATE tracker SET time = ? WHERE id = ?")
                            .bind(entry.time)
                            .bind(id)
                            .execute(&mut *tx)
                            .await
                            .with_context(|| {
                                format!(
                                    "Failed to update tracker '{}' entry {}",
                                    tracker.tracker_type, id
                                )
                            })?;
                    }
                }
                None => {
                    let mut q = sqlx::query(
                        "INSERT INTO tracker (type, score, time, feeling) VALUES (?, ?, ?, ?)",
                    )
                    .bind(&tracker.tracker_type);
                    q = match &tracker.value {
                        TrackerValue::Text(s) => q.bind(s),
                        TrackerValue::Number(n) => q.bind(n),
                        TrackerValue::Float(f) => q.bind(f),
                    };
                    q.bind(entry.time)
                        .bind(feeling_id)
                        .execute(&mut *tx)
                        .await
                        .with_context(|| {
                            format!("Failed to insert tracker '{}'", tracker.tracker_type)
                        })?;
                }
            }
            continue;
        }

        if let Some((slot_start, slot_end)) = tracker.replace_slot {
            sqlx::query("DELETE FROM tracker WHERE type = ? AND time >= ? AND time < ?")
                .bind(&tracker.tracker_type)
                .bind(slot_start)
                .bind(slot_end)
                .execute(&mut *tx)
                .await
                .with_context(|| {
                    format!(
                        "Failed to delete old entry for tracker '{}' in slot {}..{}",
                        tracker.tracker_type, slot_start, slot_end
                    )
                })?;
        }

        let mut q =
            sqlx::query("INSERT INTO tracker (type, score, time, feeling) VALUES (?, ?, ?, ?)")
                .bind(&tracker.tracker_type);
        q = match &tracker.value {
            TrackerValue::Text(s) => q.bind(s),
            TrackerValue::Number(n) => q.bind(n),
            TrackerValue::Float(f) => q.bind(f),
        };
        q.bind(entry.time)
            .bind(feeling_id)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("Failed to insert tracker '{}'", tracker.tracker_type))?;
    }

    tx.commit().await.context("Failed to commit transaction")?;
    Ok(feeling_id)
}

/// Count mood entries in `[start_time, end_time]`; when `delete` is true,
/// delete them (plus their linked tracker rows, in a transaction) and return
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
        "DELETE FROM tracker WHERE feeling IN (SELECT id FROM feeling WHERE time >= ? AND time <= ?)",
    )
    .bind(start_time)
    .bind(end_time)
    .execute(&mut *tx)
    .await
    .context("Failed to delete linked tracker entries")?;

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
        "SELECT id, mood, body, time, embedding, score FROM feeling WHERE time >= ? AND time <= ? ORDER BY time ASC",
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
            score: row.get("score"),
        })
        .collect())
}

/// Entries of one tracker in `[start, end]`, oldest first.
pub async fn fetch_tracker_entries(
    pool: &SqlitePool,
    tracker_type: &str,
    start: i64,
    end: i64,
) -> Result<Vec<TrackerEntryRow>> {
    let rows = sqlx::query(
        "SELECT id, type, CAST(score AS TEXT) AS score, time FROM tracker WHERE type = ? AND time >= ? AND time <= ? ORDER BY time ASC",
    )
    .bind(tracker_type)
    .bind(start)
    .bind(end)
    .fetch_all(pool)
    .await
    .context("Failed to fetch tracker entries")?;

    Ok(rows
        .iter()
        .map(|row| TrackerEntryRow {
            id: row.get("id"),
            tracker_type: row.get("type"),
            score: row.get("score"),
            time: row.get("time"),
        })
        .collect())
}

/// The most recent entry time per tracker type (unscoped), for the today
/// view's preview `last:` field. Trackers without entries are absent.
pub async fn fetch_tracker_last_times(
    pool: &SqlitePool,
) -> Result<std::collections::HashMap<String, i64>> {
    let rows = sqlx::query("SELECT type, MAX(time) AS last FROM tracker GROUP BY type")
        .fetch_all(pool)
        .await
        .context("Failed to fetch tracker last times")?;
    Ok(rows
        .iter()
        .map(|r| (r.get::<String, _>("type"), r.get::<i64, _>("last")))
        .collect())
}

/// All tracker entries in `[start, end]`, oldest first (today view).
pub async fn fetch_tracker_entries_today(
    pool: &SqlitePool,
    start: i64,
    end: i64,
) -> Result<Vec<TrackerEntryRow>> {
    let rows = sqlx::query(
        "SELECT id, type, CAST(score AS TEXT) AS score, time FROM tracker WHERE time >= ? AND time <= ? ORDER BY time ASC",
    )
    .bind(start)
    .bind(end)
    .fetch_all(pool)
    .await
    .context("Failed to fetch today's tracker entries")?;

    Ok(rows
        .iter()
        .map(|row| TrackerEntryRow {
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
            "SELECT id, start_time, interval_secs, target_count FROM todos WHERE short_id = ? AND interval_secs IS NOT NULL",
        )
        .bind(short_id)
        .fetch_optional(pool)
        .await
        .context("Failed to fetch recurring task")?
    } else {
        sqlx::query(
            "SELECT id, start_time, interval_secs, target_count FROM todos WHERE name = ? AND interval_secs IS NOT NULL",
        )
        .bind(name_or_id)
        .fetch_optional(pool)
        .await
        .context("Failed to fetch recurring task")?
    };

    Ok(row.map(|r| RecurringTaskMeta {
        id: r.get("id"),
        start_time: r.get("start_time"),
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

/// Link a feeling entry to tasks (by stable row id) in one transaction.
/// Duplicate links are ignored (`INSERT OR IGNORE`).
pub async fn link_feeling_to_tasks(
    pool: &SqlitePool,
    feeling_id: i64,
    task_ids: &[i64],
) -> Result<()> {
    let mut tx = pool.begin().await.context("Failed to begin transaction")?;
    for task_id in task_ids {
        sqlx::query("INSERT OR IGNORE INTO task_moods (todo_id, feeling_id) VALUES (?, ?)")
            .bind(task_id)
            .bind(feeling_id)
            .execute(&mut *tx)
            .await
            .context("Failed to insert task-mood link")?;
    }
    tx.commit().await.context("Failed to commit transaction")?;
    Ok(())
}

/// The feeling entries linked to a task, oldest first (the task preview's
/// `moods:` field).
pub async fn fetch_linked_moods(pool: &SqlitePool, task_id: i64) -> Result<Vec<FeelingRow>> {
    let rows = sqlx::query(
        "SELECT f.id, f.mood, f.body, f.time, f.embedding, f.score FROM feeling f \
         JOIN task_moods tm ON tm.feeling_id = f.id \
         WHERE tm.todo_id = ? ORDER BY f.time ASC",
    )
    .bind(task_id)
    .fetch_all(pool)
    .await
    .context("Failed to fetch linked moods")?;

    Ok(rows
        .iter()
        .map(|r| FeelingRow {
            id: r.get("id"),
            mood: r.get("mood"),
            body: r.get("body"),
            time: r.get("time"),
            embedding: r.get("embedding"),
            score: r.get("score"),
        })
        .collect())
}

/// Delete a tracker entry row.
pub async fn delete_tracker_entry(pool: &SqlitePool, id: i64) -> Result<u64> {
    let result = sqlx::query("DELETE FROM tracker WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to delete tracker row")?;
    Ok(result.rows_affected())
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

/// Update a tracker entry's score, binding the value per its configured
/// kind (Text → string, Number → i64, Float → f64). Returns affected rows.
pub async fn update_tracker_score(
    pool: &SqlitePool,
    id: i64,
    kind: TrackerKind,
    value: &str,
) -> Result<u64> {
    let mut q = sqlx::query("UPDATE tracker SET score = ? WHERE id = ?");
    q = match kind {
        TrackerKind::Text => q.bind(value),
        TrackerKind::Number => q.bind(value.parse::<i64>().unwrap_or(0)),
        TrackerKind::Float => q.bind(value.parse::<f64>().unwrap_or(0.0)),
        // Null trackers never go through the edit modal.
        TrackerKind::Null => q.bind(0),
    };
    let res = q
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to update tracker score")?;
    Ok(res.rows_affected())
}

/// Delete a feeling row and any linked tracker rows in a transaction
/// (`tracker.feeling` has a FK with no `ON DELETE CASCADE`).
pub async fn delete_feeling(pool: &SqlitePool, id: i64) -> Result<()> {
    let mut tx = pool.begin().await.context("Failed to begin transaction")?;

    sqlx::query("DELETE FROM tracker WHERE feeling = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("Failed to delete linked tracker rows")?;

    sqlx::query("DELETE FROM feeling WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("Failed to delete feeling row")?;

    tx.commit().await.context("Failed to commit transaction")?;
    Ok(())
}
