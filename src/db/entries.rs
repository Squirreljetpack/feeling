use anyhow::{Context, Result};
use sqlx::{Row, SqlitePool};

use super::models::{
    CompletionRow, EntryObject, FeelingRow, RecurringTaskMeta, TaskRow, TrackerEntryRow,
    TrackerScoreKindRow, TrackerValue,
};
use super::views::attach_full_completions;
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

/// For each tracker entry in `[start, end]`, the time of the previous
/// entry of the same type, keyed by entry id — the today-view preview's
/// `prev:` field. "Previous" is by time, with the row id as tiebreaker
/// (same-second entries: the one inserted first wins). Entries without an
/// earlier entry map to `None`.
pub async fn fetch_tracker_prev_times(
    pool: &SqlitePool,
    start: i64,
    end: i64,
) -> Result<std::collections::HashMap<i64, Option<i64>>> {
    let rows = sqlx::query(
        "SELECT t1.id, \
         (SELECT MAX(t2.time) FROM tracker t2 \
          WHERE t2.type = t1.type \
            AND (t2.time < t1.time OR (t2.time = t1.time AND t2.id < t1.id))) AS prev \
         FROM tracker t1 WHERE t1.time >= ? AND t1.time <= ?",
    )
    .bind(start)
    .bind(end)
    .fetch_all(pool)
    .await
    .context("Failed to fetch tracker prev times")?;
    Ok(rows
        .iter()
        .map(|r| (r.get::<i64, _>("id"), r.get::<Option<i64>, _>("prev")))
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

/// Tracker entries attached to feelings (the `tracker.feeling` column),
/// grouped by feeling id, oldest first within each group. Feelings without
/// attached tracker rows are absent from the map; an empty input returns an
/// empty map.
pub async fn fetch_feeling_trackers(
    pool: &SqlitePool,
    feeling_ids: &[i64],
) -> Result<std::collections::HashMap<i64, Vec<TrackerEntryRow>>> {
    let mut map = std::collections::HashMap::new();
    if feeling_ids.is_empty() {
        return Ok(map);
    }
    let sql = format!(
        "SELECT id, type, CAST(score AS TEXT) AS score, time, feeling FROM tracker \
         WHERE feeling IN ({}) ORDER BY time ASC",
        feeling_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",")
    );
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
    for id in feeling_ids {
        q = q.bind(id);
    }
    let rows = q
        .fetch_all(pool)
        .await
        .context("Failed to fetch tracker rows linked to feelings")?;
    for row in rows {
        let entry = TrackerEntryRow {
            id: row.get("id"),
            tracker_type: row.get("type"),
            score: row.get("score"),
            time: row.get("time"),
        };
        map.entry(row.get::<i64, _>("feeling"))
            .or_insert_with(Vec::new)
            .push(entry);
    }
    Ok(map)
}

/// Tasks linked to feelings via `task_moods`, grouped by feeling id,
/// ordered by name. Completions/last_time follow the today-view convention
/// (full completion scoping via [`attach_full_completions`]). An empty
/// input returns an empty map.
pub async fn fetch_feeling_tasks(
    pool: &SqlitePool,
    feeling_ids: &[i64],
) -> Result<std::collections::HashMap<i64, Vec<TaskRow>>> {
    let mut map = std::collections::HashMap::new();
    if feeling_ids.is_empty() {
        return Ok(map);
    }
    let sql = format!(
        "SELECT t.*, tm.feeling_id, NULL AS completions, NULL AS last_time \
         FROM todos t JOIN task_moods tm ON tm.todo_id = t.id \
         WHERE tm.feeling_id IN ({}) ORDER BY t.name ASC",
        feeling_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",")
    );
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
    for id in feeling_ids {
        q = q.bind(id);
    }
    let rows = q
        .fetch_all(pool)
        .await
        .context("Failed to fetch tasks linked to feelings")?;
    // Reconstruct a TaskRow per link row (the query carries the extra
    // `feeling_id` column, which query_as::<TaskRow> would drop), then
    // attach the completion aggregates to the unique tasks.
    let mut links: Vec<(i64, i64)> = Vec::new();
    let mut tasks: Vec<TaskRow> = Vec::new();
    for row in rows {
        links.push((row.get("feeling_id"), row.get("id")));
        tasks.push(TaskRow {
            id: row.get("id"),
            short_id: row.get("short_id"),
            name: row.get("name"),
            body: row.get("body"),
            priority: row.get("priority"),
            start_time: row.get("start_time"),
            available_duration_secs: row.get("available_duration_secs"),
            interval_secs: row.get("interval_secs"),
            target_count: row.get("target_count"),
            optional: row.get("optional"),
            end_time: row.get("end_time"),
            parent: row.get("parent"),
            completions: None,
            last_time: None,
        });
    }
    let by_id: std::collections::HashMap<i64, TaskRow> =
        attach_full_completions(pool, tasks, crate::date::now())
            .await?
            .into_iter()
            .map(|t| (t.id, t))
            .collect();
    for (feeling_id, task_id) in links {
        if let Some(task) = by_id.get(&task_id) {
            map.entry(feeling_id).or_insert_with(Vec::new).push(task.clone());
        }
    }
    Ok(map)
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

/// One deletion rule for `:db doctor`, computed from the tracker's current
/// configured kind. Rules are applied in one transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackerPruneRule {
    /// Keep only entries whose SQLite storage class equals `keep`; delete
    /// the rest. `keep` is `text` for kind text, `integer` for number/null,
    /// `real` for float — the storage class every writer binds for that
    /// kind (`create_entry`, `update_tracker_score`).
    Storage {
        tracker_type: String,
        keep: &'static str,
    },
    /// Delete every entry with `score != 0` (any storage class). Time-marker
    /// null trackers — `null` with both min and max set — always write
    /// score 0, so nonzero rows are stale count-mode leftovers.
    NonzeroScore { tracker_type: String },
    /// Delete every entry of a type with no `[tracker.<type>]` section in
    /// the config (renamed/removed tracker; the today view errors on such
    /// rows).
    All { tracker_type: String },
}

/// Storage-class distribution of tracker entries, grouped by type and
/// `typeof(score)`, for `:db doctor`. `nonzero` counts `score != 0` rows
/// within the bucket (COALESCE'd; only meaningful for integer buckets).
pub async fn fetch_tracker_score_kinds(pool: &SqlitePool) -> Result<Vec<TrackerScoreKindRow>> {
    let rows = sqlx::query(
        "SELECT type, typeof(score) AS storage, COUNT(*) AS count, \
         COALESCE(SUM(CASE WHEN score != 0 THEN 1 ELSE 0 END), 0) AS nonzero \
         FROM tracker GROUP BY type, typeof(score) ORDER BY type, storage",
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch tracker score kinds")?;

    Ok(rows
        .iter()
        .map(|r| TrackerScoreKindRow {
            tracker_type: r.get("type"),
            storage: r.get("storage"),
            count: r.get("count"),
            nonzero: r.get("nonzero"),
        })
        .collect())
}

/// Apply `:db doctor` prune rules in one transaction; returns the total
/// number of rows deleted. `NonzeroScore` and `All` may overlap a `Storage`
/// rule's rows, but each rule deletes only rows still present, so the
/// per-rule `rows_affected` counts are disjoint.
pub async fn prune_tracker_rules(pool: &SqlitePool, rules: &[TrackerPruneRule]) -> Result<u64> {
    let mut tx = pool.begin().await.context("Failed to begin transaction")?;
    let mut deleted = 0u64;
    for rule in rules {
        let res = match rule {
            TrackerPruneRule::Storage { tracker_type, keep } => {
                sqlx::query("DELETE FROM tracker WHERE type = ? AND typeof(score) != ?")
                    .bind(tracker_type)
                    .bind(keep)
                    .execute(&mut *tx)
                    .await
                    .with_context(|| {
                        format!("Failed to prune mismatched entries for tracker '{tracker_type}'")
                    })?
            }
            TrackerPruneRule::NonzeroScore { tracker_type } => {
                sqlx::query("DELETE FROM tracker WHERE type = ? AND score != 0")
                    .bind(tracker_type)
                    .execute(&mut *tx)
                    .await
                    .with_context(|| {
                        format!("Failed to prune nonzero entries for tracker '{tracker_type}'")
                    })?
            }
            TrackerPruneRule::All { tracker_type } => {
                sqlx::query("DELETE FROM tracker WHERE type = ?")
                    .bind(tracker_type)
                    .execute(&mut *tx)
                    .await
                    .with_context(|| {
                        format!("Failed to prune orphan entries for tracker '{tracker_type}'")
                    })?
            }
        };
        deleted += res.rows_affected();
    }
    tx.commit().await.context("Failed to commit transaction")?;
    Ok(deleted)
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

/// Re-log a null tracker entry in place: move its time to `now`; in count
/// mode (either `min`/`max` bound missing) also increment the score by 1 —
/// mirroring the CLI's null-tracker upsert. Returns affected rows.
pub async fn relog_null_tracker(
    pool: &SqlitePool,
    id: i64,
    now: i64,
    increment: bool,
) -> Result<u64> {
    let sql = if increment {
        "UPDATE tracker SET time = ?, score = score + 1 WHERE id = ?"
    } else {
        "UPDATE tracker SET time = ? WHERE id = ?"
    };
    let res = sqlx::query(sql)
        .bind(now)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to re-log null tracker entry")?;
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
