//! Integration tests for the feeling CLI.
//!
//! These tests verify the full flow from CLI parsing through database operations.

use feeling::{
    clap::{parse_from, CliOpts},
    config::{Config, TrackerType},
    db::test_pool,
    handlers::handle_command,
};
use sqlx::{Row, SqlitePool};
use std::sync::Mutex;

/// Serializes tests that mutate process-wide env vars (EDITOR / VISUAL /
/// FEELING_CONFIG_DIR) so they never observe each other's values. Poison
/// recovery keeps a panicking test from deadlocking the rest.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Write a fake-editor shell script that appends `text` (plus newline) to
/// its first argument — the temp file the body editor points at. Lets the
/// editor-hint tests simulate a user typing below the hint line without
/// spawning a real editor.
fn fake_editor_appending(dir: &std::path::Path, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!("#!/bin/sh\nprintf '%s\\n' '{}' >> \"$1\"\n", text),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    path
}

/// Helper: create a oneshot task and return its id
async fn create_oneshot_task(pool: &SqlitePool, name: &str) -> i64 {
    let cmd = parse_from(vec!["!".to_string(), name.to_string()]).unwrap();
    let config = Config::default();
    handle_command(cmd, pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    sqlx::query_scalar::<_, i64>("SELECT id FROM todos WHERE name = ?")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn test_create_feeling_entry() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    let cmd = parse_from(vec!["comfortably".to_string(), "numb".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let row = sqlx::query("SELECT mood, body FROM feeling")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(row.get::<String, _>("mood"), "comfortably numb");
    assert_eq!(row.get::<String, _>("body"), "");
}

#[tokio::test]
async fn test_create_feeling_with_customs() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );
    config.tracker.insert(
        "water".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );

    let cmd = parse_from(vec![
        "-sleep".to_string(), // -sleep 8
        "8".to_string(),
        "-water".to_string(), // -water 5
        "5".to_string(),
        "good".to_string(),
    ])
    .unwrap();

    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let feeling = sqlx::query("SELECT id, mood FROM feeling")
        .fetch_one(&pool)
        .await
        .unwrap();

    let feeling_id: i64 = feeling.get("id");
    assert_eq!(feeling.get::<String, _>("mood"), "good");

    // Verify custom trackers were inserted and linked
    let rows = sqlx::query("SELECT type, score, feeling FROM custom ORDER BY type")
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<String, _>("type"), "sleep");
    assert_eq!(rows[0].get::<f64, _>("score"), 8.0);
    assert_eq!(rows[0].get::<Option<i64>, _>("feeling"), Some(feeling_id));
    assert_eq!(rows[1].get::<String, _>("type"), "water");
    assert_eq!(rows[1].get::<f64, _>("score"), 5.0);
    assert_eq!(rows[1].get::<Option<i64>, _>("feeling"), Some(feeling_id));
}

#[tokio::test]
async fn test_create_custom_only() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );

    let cmd = parse_from(vec!["-sleep".to_string(), "10".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    // No feeling should be inserted
    let feeling_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feeling")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(feeling_count, 0);

    // Custom tracker inserted without feeling link
    let custom = sqlx::query("SELECT type, score, feeling FROM custom")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(custom.get::<String, _>("type"), "sleep");
    assert_eq!(custom.get::<f64, _>("score"), 10.0);
    assert_eq!(custom.get::<Option<i64>, _>("feeling"), None);
}

#[tokio::test]
async fn test_custom_tracker_interval_insert_strategies() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    // text + interval: re-logging replaces the previous entry in the slot
    config.tracker.insert(
        "affirmation".to_string(),
        feeling::config::TrackerSetting {
            interval: Some(86400),
            min: None,
            max: None,
            kind: TrackerType::Text,
        },
    );
    // float + interval: re-logging replaces the previous entry in the slot
    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: Some(86400),
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );
    // number + interval: plain insert, accumulates
    config.tracker.insert(
        "runs".to_string(),
        feeling::config::TrackerSetting {
            interval: Some(86400),
            min: None,
            max: None,
            kind: TrackerType::Number,
        },
    );

    // Two inserts back-to-back land in the same interval slot.
    for (tracker, value) in [
        ("-sleep", "8"),
        ("-sleep", "6"),
        ("-runs", "2"),
        ("-runs", "3"),
        ("-affirmation", "first"),
        ("-affirmation", "second"),
    ] {
        let cmd = parse_from(vec![tracker.to_string(), value.to_string()]).unwrap();
        handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
            .await
            .unwrap();
    }

    // Float: replaced by the latest value in the slot (1 row, score 6).
    let sleep_rows: Vec<(f64,)> = sqlx::query_as("SELECT score FROM custom WHERE type = 'sleep'")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        sleep_rows.len(),
        1,
        "float+interval must replace the slot entry"
    );
    assert_eq!(sleep_rows[0].0, 6.0);

    // Text: replaced by the latest value in the slot (1 row, 'second').
    let text_rows: Vec<(String,)> =
        sqlx::query_as("SELECT score FROM custom WHERE type = 'affirmation'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        text_rows.len(),
        1,
        "text+interval must replace the slot entry"
    );
    assert_eq!(text_rows[0].0, "second");

    // Number: plain insert, both rows kept (the view sums them).
    let runs_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM custom WHERE type = 'runs'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs_count, 2, "number+interval must accumulate");

    // The replace is slot-scoped: a 1s-interval float logged 1.1s apart lands
    // in two different slots, so both entries are kept.
    config.tracker.insert(
        "water".to_string(),
        feeling::config::TrackerSetting {
            interval: Some(1),
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );
    for _ in 0..2 {
        let cmd = parse_from(vec!["-water".to_string(), "1".to_string()]).unwrap();
        handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    }
    let water_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM custom WHERE type = 'water'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        water_count, 2,
        "replace must be scoped to the interval slot, not global"
    );
}

#[tokio::test]
async fn test_create_oneshot_task() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    let cmd = parse_from(vec!["!".to_string(), "urgent task".to_string()]).unwrap();

    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let task = sqlx::query("SELECT name, body, priority, interval_secs, target_count FROM todos")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(task.get::<String, _>("name"), "urgent task");
    assert_eq!(task.get::<String, _>("body"), "");
    assert_eq!(task.get::<i32, _>("priority"), 5); // default priority
    assert_eq!(task.get::<Option<i64>, _>("interval_secs"), None);
    // Oneshot tasks must default to target_count = 0 (single-completion tasks;
    // the editor flow's `prompt_target_count` also blanks to 0). Without this
    // the preview would render a useless progress bar with capacity 1.
    assert_eq!(task.get::<i32, _>("target_count"), 0); // default target_count
}

#[tokio::test]
async fn test_create_oneshot_task_with_date() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    let cmd = parse_from(vec![
        "!".to_string(),
        "scheduled task".to_string(),
        "@2024-03-20".to_string(),
    ])
    .unwrap();

    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let task = sqlx::query("SELECT name, start_time FROM todos")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(task.get::<String, _>("name"), "scheduled task");
    // Verify start_time is set to the specified date at midnight
    let start_time: i64 = task.get("start_time");
    assert!(start_time > 0);
}

#[tokio::test]
async fn test_custom_tracker_range_not_enforced() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();

    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: Some(4.0),
            max: Some(10.0),
            kind: TrackerType::Float,
        },
    );

    // min/max are only for binning (color mapping), not for gating
    // insertion: below-min, in-range, and above-max values all store.
    for value in ["3", "7", "11"] {
        let cmd = parse_from(vec!["-sleep".to_string(), value.to_string()]).unwrap();
        handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
            .await
            .unwrap();
    }

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM custom")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 3);
}

#[tokio::test]
async fn test_multiple_customs_same_feeling() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );
    config.tracker.insert(
        "water".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );
    config.tracker.insert(
        "exercise".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );

    let cmd = parse_from(vec![
        "-sleep".to_string(),
        "8".to_string(),
        "-water".to_string(),
        "6".to_string(),
        "-exercise".to_string(),
        "30".to_string(),
        "great".to_string(),
    ])
    .unwrap();

    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let feeling_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feeling")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(feeling_count, 1);

    let custom_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM custom")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(custom_count, 3);

    let linked_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM custom c JOIN feeling f ON c.feeling = f.id")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(linked_count, 3);
}

#[tokio::test]
async fn test_view_oneshot_tasks() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Create some oneshot tasks
    let cmd1 = parse_from(vec!["!".to_string(), "low priority task".to_string()]).unwrap();
    handle_command(cmd1, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let cmd2 = parse_from(vec!["!".to_string(), "high priority task".to_string()]).unwrap();
    handle_command(cmd2, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    // View oneshot tasks and capture the tab-separated output
    let cmd = parse_from(vec!["!".to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();

    // Verify both tasks appear as tab-separated rows:
    //   id \t interval \t next_available \t pri \t name \t status
    assert!(output.contains("low priority task"), "output: {output:?}");
    assert!(output.contains("high priority task"), "output: {output:?}");
    for line in output.lines() {
        assert_eq!(
            line.split('\t').count(),
            6,
            "line not tab-separated: {line:?}"
        );
        let fields: Vec<&str> = line.split('\t').collect();
        assert!(fields[0].parse::<i64>().is_ok(), "id not numeric: {line:?}");
        // Oneshot tasks render a single space in interval/next_available.
        assert_eq!(fields[1], " ", "oneshot interval: {line:?}");
        assert_eq!(fields[2], " ", "oneshot next_available: {line:?}");
        assert_eq!(fields[3], "5", "default priority: {line:?}");
        assert_eq!(fields[5], "◯", "not-started status: {line:?}");
    }

    // Verify both tasks exist
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM todos WHERE interval_secs IS NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn test_update_oneshot_task_simple() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    let task_id = create_oneshot_task(&pool, "test task").await;

    // Mark as done: - <short id>. On a fresh pool the row id equals the
    // short id, so `create_oneshot_task`'s return value works directly.
    let cmd = parse_from(vec!["-".to_string(), task_id.to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    // The user-facing short id is cleared once the task is completed.
    let short_id: Option<i64> =
        sqlx::query_scalar("SELECT short_id FROM todos WHERE name = 'test task'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(short_id.is_none(), "completed task must lose its short id");

    // Verify completions: derived as SUM(count) from todo_completions.
    let completions: Option<i32> =
        sqlx::query_scalar("SELECT SUM(count) FROM todo_completions WHERE todo_id = ?")
            .bind(task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(completions, Some(1));

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM todo_completions WHERE todo_id = ?")
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_update_nonexistent_oneshot_fails() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    let cmd = parse_from(vec!["-".to_string(), "99999".to_string()]).unwrap();
    let result = handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[tokio::test]
async fn test_update_at_name_fails_as_query() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // The `- @name` recurring form was removed; `- @name` is now a word
    // query that never matches (task names don't carry the '@' prefix).
    let cmd = parse_from(vec!["-".to_string(), "@nonexistent".to_string()]).unwrap();
    let result = handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("No task matches"));
}

#[tokio::test]
async fn test_update_by_query_words() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    create_oneshot_task(&pool, "buy milk").await;
    create_oneshot_task(&pool, "walk the dog").await;

    // "milk" matches exactly one task.
    let cmd = parse_from(vec!["-".to_string(), "milk".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let completions: Option<i32> = sqlx::query_scalar(
        "SELECT SUM(count) FROM todo_completions tc JOIN todos t ON t.id = tc.todo_id \
         WHERE t.name = 'buy milk'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(completions, Some(1));

    // The other task is untouched.
    let other: Option<i32> = sqlx::query_scalar(
        "SELECT SUM(count) FROM todo_completions tc JOIN todos t ON t.id = tc.todo_id \
         WHERE t.name = 'walk the dog'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(other, None);
}

#[tokio::test]
async fn test_update_by_query_words_multiword_in_order() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    create_oneshot_task(&pool, "buy milk and eggs").await;
    create_oneshot_task(&pool, "buy eggs only").await;

    // "milk eggs": matches only the first (words in order, gaps allowed).
    let cmd = parse_from(vec![
        "-".to_string(),
        "milk".to_string(),
        "eggs".to_string(),
    ])
    .unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let first: Option<i32> = sqlx::query_scalar(
        "SELECT SUM(count) FROM todo_completions tc JOIN todos t ON t.id = tc.todo_id \
         WHERE t.name = 'buy milk and eggs'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(first, Some(1));

    let second: Option<i32> = sqlx::query_scalar(
        "SELECT SUM(count) FROM todo_completions tc JOIN todos t ON t.id = tc.todo_id \
         WHERE t.name = 'buy eggs only'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(second, None, "out-of-order words must not match");
}

#[tokio::test]
async fn test_update_by_query_words_with_count() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    create_oneshot_task(&pool, "buy milk").await;

    let cmd = parse_from(vec!["-".to_string(), "milk".to_string(), "3".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let completions: Option<i32> = sqlx::query_scalar(
        "SELECT SUM(count) FROM todo_completions tc JOIN todos t ON t.id = tc.todo_id \
         WHERE t.name = 'buy milk'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(completions, Some(3));
}

#[tokio::test]
async fn test_update_by_query_words_no_match_fails() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    create_oneshot_task(&pool, "buy milk").await;

    let cmd = parse_from(vec!["-".to_string(), "walk".to_string()]).unwrap();
    let result = handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("No task matches"));
}

#[tokio::test]
async fn test_update_by_query_words_multiple_matches_fail() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    create_oneshot_task(&pool, "buy milk").await;
    create_oneshot_task(&pool, "buy milk again").await;

    let cmd = parse_from(vec!["-".to_string(), "buy".to_string()]).unwrap();
    let result = handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false).await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("2 tasks match"), "got: {msg}");
}

#[tokio::test]
async fn test_editor_hint_on_strips_hint_line() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let editor = fake_editor_appending(dir.path(), "editor_append.sh", "my body text");

    std::env::set_var("VISUAL", &editor);
    std::env::set_var("EDITOR", "true"); // fallback, unused

    let pool = test_pool().await.unwrap();
    let config = Config::default(); // [editor] hint defaults to true

    let cmd = parse_from(vec!["ok".to_string(), "..".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let body: String = sqlx::query_scalar("SELECT body FROM feeling")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(body, "my body text");
}

#[tokio::test]
async fn test_editor_hint_off_keeps_first_line() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let editor = fake_editor_appending(dir.path(), "editor_append.sh", "first line\nsecond line");

    std::env::set_var("VISUAL", &editor);
    std::env::set_var("EDITOR", "true");

    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.editor.hint = false;

    let cmd = parse_from(vec!["ok".to_string(), "..".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let body: String = sqlx::query_scalar("SELECT body FROM feeling")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(body, "first line\nsecond line");
}

#[tokio::test]
async fn test_create_feeling_tracker_in_final_position() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );
    config.tracker.insert(
        "water".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );

    // Trackers after the mood: `feeling good -sleep 8 -water 5`.
    let cmd = parse_from(vec![
        "good".to_string(),
        "-sleep".to_string(),
        "8".to_string(),
        "-water".to_string(),
        "5".to_string(),
    ])
    .unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let feeling_id: i64 = sqlx::query_scalar("SELECT id FROM feeling WHERE mood = 'good'")
        .fetch_one(&pool)
        .await
        .unwrap();

    let rows = sqlx::query("SELECT type, score, feeling FROM custom ORDER BY type")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<String, _>("type"), "sleep");
    assert_eq!(rows[0].get::<f64, _>("score"), 8.0);
    assert_eq!(rows[0].get::<Option<i64>, _>("feeling"), Some(feeling_id));
    assert_eq!(rows[1].get::<String, _>("type"), "water");
    assert_eq!(rows[1].get::<f64, _>("score"), 5.0);
    assert_eq!(rows[1].get::<Option<i64>, _>("feeling"), Some(feeling_id));
}

#[tokio::test]
async fn test_no_feeling_no_custom_bails() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Prevent the test from spawning a real editor: `true` exits 0 without
    // modifying the temp file, so body stays empty and the "Nothing to log"
    // check fires correctly.
    std::env::set_var("EDITOR", "true");
    std::env::set_var("VISUAL", "true");

    // Empty entry (no feeling, no customs) should fail
    let cmd = parse_from(vec!["..".to_string()]).unwrap();
    let result = handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Nothing to log"));
}

#[tokio::test]
async fn test_out_of_range_custom_still_inserts() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();

    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: Some(4.0),
            max: Some(10.0),
            kind: TrackerType::Float,
        },
    );

    // sleep=2 is below min=4, but min/max only affect binning:
    // the feeling and its custom entry are still inserted.
    let cmd = parse_from(vec![
        "-sleep".to_string(),
        "2".to_string(),
        "ok".to_string(),
    ])
    .unwrap();

    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let feeling_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feeling")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(feeling_count, 1);

    let custom_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM custom")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(custom_count, 1);
}

#[tokio::test]
async fn test_tab_in_mood_rejected() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Mood with tab should be rejected
    let cmd = parse_from(vec!["ok\tfeeling".to_string()]).unwrap();
    let result = handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("tab characters"));
}

#[tokio::test]
async fn test_unknown_tracker_rejected() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Unknown tracker should be rejected
    let cmd = parse_from(vec!["-unknown".to_string(), "5".to_string()]).unwrap();

    let result = handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false).await;
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Unknown custom tracker"));
}

#[tokio::test]
async fn test_today_view_no_data() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    // handle_today needs the embedder built (init_with) — handle_command does
    // this before dispatching, so the direct call must too.
    config
        .moods
        .init_with(&pool, feeling::embed::global_embedder())
        .await
        .unwrap();
    // handle_today should succeed even with no data
    let mut out = Vec::new();
    let result = feeling::views::handle_today(&pool, &config, &CliOpts::default(), &mut out).await;
    assert!(result.is_ok());
    let output = String::from_utf8(out).unwrap();
    assert!(
        output.contains("Nothing logged today."),
        "output: {output:?}"
    );
}

#[tokio::test]
async fn test_today_view_with_data() {
    use feeling::config::{TrackerSetting, TrackerType};

    let pool = test_pool().await.unwrap();
    let mut config = Config::default();

    // Register custom trackers
    config.tracker.insert(
        "sleep".to_string(),
        TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );
    config.tracker.insert(
        "water".to_string(),
        TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );

    // Create a feeling entry via the CLI path
    handle_command(
        parse_from(vec!["good".to_string()]).unwrap(),
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    // handle_today needs the embedder built (init_with) — handle_command does
    // this before dispatching, so the direct call must too.
    config
        .moods
        .init_with(&pool, feeling::embed::global_embedder())
        .await
        .unwrap();

    // Create a custom-only entry via the CLI path
    handle_command(
        parse_from(vec!["-sleep".to_string(), "8".to_string()]).unwrap(),
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    // Create a oneshot task due today via the CLI path: ! desc @YYYY-MM-DD
    let today_str = chrono::Local::now().format("%Y-%m-%d").to_string();
    handle_command(
        parse_from(vec![
            "!".to_string(),
            "due today".to_string(),
            format!("@{today_str}"),
        ])
        .unwrap(),
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    // handle_today should succeed with data and emit tab-separated rows
    let mut out = Vec::new();
    let result = feeling::views::handle_today(&pool, &config, &CliOpts::default(), &mut out).await;
    assert!(result.is_ok());
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("good"), "output: {output:?}");
    assert!(output.contains("due today"), "output: {output:?}");
    assert!(output.contains('\t'), "output: {output:?}");
}

#[tokio::test]
async fn test_view_done_tasks() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Create a oneshot task, then complete it
    let task_id = create_oneshot_task(&pool, "finished task").await;
    let update_cmd = parse_from(vec!["-".to_string(), task_id.to_string()]).unwrap();
    handle_command(update_cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    // @done should list the completed task
    let cmd = parse_from(vec!["@done".to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("finished task"), "output: {output:?}");
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 6, "line not tab-separated: {line:?}");
        // Completed tasks show no id — the id column is empty.
        assert!(fields[0].is_empty(), "completed task shows no id: {line:?}");
        // Done oneshot task → colored "●" badge (no "DONE" suffix anymore).
        assert!(fields[5].contains('●'), "badge dot expected: {line:?}");
        assert!(
            !fields[5].ends_with("DONE"),
            "DONE suffix dropped: {line:?}"
        );
    }
}

#[tokio::test]
async fn test_view_due_tasks() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Create a oneshot task due today via the CLI path: ! desc @YYYY-MM-DD
    let today_str = chrono::Local::now().format("%Y-%m-%d").to_string();
    let cmd = parse_from(vec![
        "!".to_string(),
        "due today task".to_string(),
        format!("@{today_str}"),
    ])
    .unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    // @due should list the task
    let cmd = parse_from(vec!["@due".to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("due today task"), "output: {output:?}");
    for line in output.lines() {
        assert_eq!(
            line.split('\t').count(),
            6,
            "line not tab-separated: {line:?}"
        );
    }
}

#[tokio::test]
async fn test_embed_utility() {
    use std::io::Cursor;

    let mut input = Cursor::new(b"happy day\nsad night\n");
    let mut out = Vec::new();
    feeling::handlers::handle_embed(&mut input, &mut out).unwrap();

    let output = String::from_utf8(out).unwrap();
    let mut lines = output.lines();
    let v1: Vec<f64> = lines
        .next()
        .expect("expected first embedding")
        .split_whitespace()
        .map(|s| s.parse().unwrap())
        .collect();
    let v2: Vec<f64> = lines
        .next()
        .expect("expected second embedding")
        .split_whitespace()
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(v1.len(), feeling::embed::EMBED_DIM);
    assert_eq!(v2.len(), feeling::embed::EMBED_DIM);
    assert!(lines.next().is_none(), "expected exactly two lines");
}

#[tokio::test]
async fn test_tracker_mood_dots() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Create a feeling entry (with a mood and body so it gets a dot)
    let cmd = parse_from(vec!["happy".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    // : (mood tracker) should print a header and dot rows
    let cmd = parse_from(vec![":".to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("Mood tracker (Week)"), "output: {output:?}");
    assert!(output.contains('●'), "expected a filled dot: {output:?}");
}

#[tokio::test]
async fn test_tracker_custom_dots() {
    use feeling::config::{TrackerSetting, TrackerType};

    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "sleep".to_string(),
        TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );

    // Custom entry via CLI
    let cmd = parse_from(vec!["-sleep".to_string(), "8".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    // : sleep should show a filled dot
    let cmd = parse_from(vec![":".to_string(), "sleep".to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("Tracker 'sleep'"), "output: {output:?}");
    assert!(output.contains('●'), "expected a filled dot: {output:?}");
}

#[tokio::test]
async fn test_tracker_recurring_dots() {
    let pool = test_pool().await.unwrap();

    // Create a recurring task via CLI (interactive prompts — use a direct DB insert
    // for the task itself, then mark completion via update)
    let name = "exercise";
    sqlx::query(
        "INSERT INTO todos (name, body, priority, interval_secs, available_duration_secs, target_count, optional, start_time) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(name)
    .bind("")
    .bind(5)
    .bind(86400) // 1 day interval
    .bind::<Option<i64>>(None)
    .bind(0)
    .bind(0)
    .bind(0)
    .execute(&pool)
    .await
    .unwrap();

    // Mark it complete via the sql API (the CLI `- @name` form was removed).
    let config = Config::default();
    let task_id: i64 = sqlx::query_scalar("SELECT id FROM todos WHERE name = ?")
        .bind(name)
        .fetch_one(&pool)
        .await
        .unwrap();
    feeling::sql::update_task(&pool, task_id, 1).await.unwrap();

    // : @exercise should show the task with a success dot
    let cmd = parse_from(vec![":".to_string(), format!("@{name}")]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(
        output.contains(&format!("Task '{name}'")),
        "output: {output:?}"
    );
    assert!(output.contains('●'), "expected a filled dot: {output:?}");
}

#[tokio::test]
async fn test_tracker_recurring_year_uses_middle_dot() {
    let pool = test_pool().await.unwrap();

    // A recurring task with no completions: every interval slot is 0%.
    let name = "brush teeth";
    sqlx::query(
        "INSERT INTO todos (name, body, priority, interval_secs, available_duration_secs, target_count, optional, start_time) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(name)
    .bind("")
    .bind(5)
    .bind(86400) // 1 day interval
    .bind::<Option<i64>>(None)
    .bind(0)
    .bind(0)
    .bind(0)
    .execute(&pool)
    .await
    .unwrap();

    let config = Config::default();
    // Year range: empty intervals render the compact · instead of the large ◯.
    let cmd = parse_from(vec![":year".to_string(), format!("@{name}")]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(
        output.contains(&format!("Task '{name}' (Year)")),
        "output: {output:?}"
    );
    assert!(
        output.contains('·'),
        "expected middle dots for empty intervals in a year grid: {output:?}"
    );
    assert!(
        !output.contains('◯'),
        "year grid must not use the large ◯: {output:?}"
    );
}

#[tokio::test]
async fn test_recurring_negative_delta_does_not_touch_previous_intervals() {
    let pool = test_pool().await.unwrap();

    // A recurring task with a 1-day interval that started 3 days and 500s ago.
    // The current interval therefore began at now - 500s.
    let interval = 86_400i64;
    let now = feeling::date::now();
    let start_time = now - 3 * interval - 500;
    let name = "water plants";
    sqlx::query(
        "INSERT INTO todos (name, body, priority, interval_secs, available_duration_secs, target_count, optional, start_time) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(name)
    .bind("")
    .bind(5)
    .bind(interval)
    .bind::<Option<i64>>(None)
    .bind(0)
    .bind(0)
    .bind(start_time)
    .execute(&pool)
    .await
    .unwrap();

    let task_id: i64 = sqlx::query_scalar("SELECT id FROM todos WHERE name = ?")
        .bind(name)
        .fetch_one(&pool)
        .await
        .unwrap();

    // The boundary between the previous and current intervals, computed with
    // the same helper the update path uses.
    let interval_start = feeling::task::current_interval_start(start_time, interval, now);

    // One completion in the previous interval (count 2), one in the current
    // interval (count 3).
    sqlx::query("INSERT INTO todo_completions (todo_id, time, count) VALUES (?, ?, ?)")
        .bind(task_id)
        .bind(interval_start - 100)
        .bind(2)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO todo_completions (todo_id, time, count) VALUES (?, ?, ?)")
        .bind(task_id)
        .bind(interval_start + 100)
        .bind(3)
        .execute(&pool)
        .await
        .unwrap();

    // Apply -5 via the sql API (the CLI `- @name` form was removed): the
    // current interval only holds 3, so the remaining 2 must NOT reach back
    // into the previous interval.
    feeling::sql::update_task(&pool, task_id, -5).await.unwrap();

    // Previous-interval completion is untouched.
    let prev_sum: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(count), 0) FROM todo_completions WHERE todo_id = ? AND time < ?",
    )
    .bind(task_id)
    .bind(interval_start)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(prev_sum, 2, "previous interval must not be touched");

    // Current-interval completion was fully consumed.
    let cur_sum: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(count), 0) FROM todo_completions WHERE todo_id = ? AND time >= ?",
    )
    .bind(task_id)
    .bind(interval_start)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cur_sum, 0, "current interval should be consumed");

    // The interval-scoped total returned by the shared helper is 0.
    let total = feeling::task::apply_completion_delta(&pool, task_id, 0)
        .await
        .unwrap();
    assert_eq!(total, 0, "interval-scoped total must be 0");
}

#[tokio::test]
async fn test_recurring_previous_interval_completions_still_shown() {
    let pool = test_pool().await.unwrap();

    // A recurring task with target_count 2, started 2 intervals + 500s ago.
    // Its only completions live in the FIRST interval, so the current-interval
    // sum is 0 even though the all-time sum already reaches the target.
    let interval = 86_400i64;
    let now = feeling::date::now();
    let start_time = now - 2 * interval - 500;
    let name = "brush teeth";
    sqlx::query(
        "INSERT INTO todos (name, body, priority, interval_secs, available_duration_secs, target_count, optional, start_time) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(name)
    .bind("")
    .bind(5)
    .bind(interval)
    .bind::<Option<i64>>(None)
    .bind(2)
    .bind(0)
    .bind(start_time)
    .execute(&pool)
    .await
    .unwrap();

    let task_id: i64 = sqlx::query_scalar("SELECT id FROM todos WHERE name = ?")
        .bind(name)
        .fetch_one(&pool)
        .await
        .unwrap();

    // Two completions in the first interval only (count 1 each).
    sqlx::query("INSERT INTO todo_completions (todo_id, time, count) VALUES (?, ?, ?)")
        .bind(task_id)
        .bind(start_time + 100)
        .bind(1)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO todo_completions (todo_id, time, count) VALUES (?, ?, ?)")
        .bind(task_id)
        .bind(start_time + 200)
        .bind(1)
        .execute(&pool)
        .await
        .unwrap();

    let config = Config::default();

    // @ (CLI) must still show the task: it is not done in the current
    // interval, so it renders as the not-started badge.
    let mut out = Vec::new();
    let cmd = parse_from(vec!["@".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains(name), "@ should show the task: {output:?}");
    for line in output.lines() {
        if line.contains(name) {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields[5], "◯", "not-started badge expected: {line:?}");
        }
    }

    // Completing it in the current interval hides it from the CLI @ view.
    // (The `- @name` CLI update form was removed, so bump directly.)
    feeling::sql::update_task(&pool, task_id, 2).await.unwrap();

    let mut out = Vec::new();
    let cmd = parse_from(vec!["@".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(
        !output.contains(name),
        "@ must hide a task done in the current interval: {output:?}"
    );
}

// ---------- Custom tracker payload types (text | number | float) ----------

#[tokio::test]
async fn test_text_tracker_entry_today_badge_and_listing() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "accomplishment".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Text,
        },
    );

    // feeling -accomplishment "fixed 2 bugs" via the CLI path
    let cmd = parse_from(vec![
        "-accomplishment".to_string(),
        "fixed 2 bugs".to_string(),
        "good".to_string(),
    ])
    .unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    // Stored as text with the string payload
    let row = sqlx::query("SELECT score, typeof(score) AS t FROM custom")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("score"), "fixed 2 bugs");
    assert_eq!(row.get::<String, _>("t"), "text");

    // Today view: text entries use the · badge with the text as label
    // (bare `feeling` → Today; `-` alone is the TasksEdit stub).
    let cmd = parse_from(vec![]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(
        output.contains("accomplishment: fixed 2 bugs"),
        "output: {output:?}"
    );
    assert!(
        output.contains('\t') && output.contains('·'),
        "text custom entries must use the · badge: {output:?}"
    );

    // : accomplishment lists entries as dark-gray '> text' lines
    let cmd = parse_from(vec![":".to_string(), "accomplishment".to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    // The dark-gray '> ' prefix is ANSI-wrapped, so assert the pieces.
    assert!(output.contains("> "), "output: {output:?}");
    assert!(
        output.contains("fixed 2 bugs"),
        "expected the entry text after the prefix: {output:?}"
    );
}

#[tokio::test]
async fn test_text_tracker_lists_all_entries_in_range() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "accomplishment".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Text,
        },
    );

    for text in ["fixed 2 bugs", "shipped the feature", "wrote docs"] {
        let cmd = parse_from(vec!["-accomplishment".to_string(), text.to_string()]).unwrap();
        handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
            .await
            .unwrap();
    }

    let cmd = parse_from(vec![":".to_string(), "accomplishment".to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    for text in ["fixed 2 bugs", "shipped the feature", "wrote docs"] {
        assert!(output.contains(text), "output: {output:?}");
    }
    assert_eq!(output.matches("> ").count(), 3, "output: {output:?}");
}

#[tokio::test]
async fn test_custom_tracker_parse_errors() {
    let pool = test_pool().await.unwrap();

    // Float tracker: non-numeric argument must error with a clear message
    let mut config = Config::default();
    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );
    let cmd = parse_from(vec!["-sleep".to_string(), "good".to_string()]).unwrap();
    let result = handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false).await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Cannot parse 'good' as a number for tracker 'sleep'"),
        "expected a clear float parse error"
    );

    // Number tracker: non-integer argument must error
    config.tracker.insert(
        "bugs".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Number,
        },
    );
    let cmd = parse_from(vec!["-bugs".to_string(), "3.5".to_string()]).unwrap();
    let result = handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false).await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Cannot parse '3.5' as an integer for tracker 'bugs'"),
        "expected a clear integer parse error"
    );

    // Nothing was stored
    let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM custom")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("n");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_number_tracker_stored_as_integer() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "bugs".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: Some(0.0),
            max: Some(10.0),
            kind: TrackerType::Number,
        },
    );

    let cmd = parse_from(vec!["-bugs".to_string(), "3".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    // Number trackers store INTEGER payloads.
    let row = sqlx::query("SELECT score, typeof(score) AS t FROM custom")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>("score"), 3);
    assert_eq!(row.get::<String, _>("t"), "integer");

    // Values outside min/max still insert (min/max only affect binning).
    let cmd = parse_from(vec!["-bugs".to_string(), "11".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let row = sqlx::query("SELECT score, typeof(score) AS t FROM custom ORDER BY id DESC")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>("score"), 11);
    assert_eq!(row.get::<String, _>("t"), "integer");

    // Today view shows the integer value (bare `feeling` → Today).
    let cmd = parse_from(vec![]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("bugs: 3"), "output: {output:?}");
}

#[tokio::test]
async fn test_today_view_include_overdue() {
    let pool = test_pool().await.unwrap();

    // Seed an overdue oneshot task (due two days ago) directly.
    let name = "overdue chore";
    sqlx::query("INSERT INTO todos (name, body, priority, start_time) VALUES (?, '', 5, ?)")
        .bind(name)
        .bind(feeling::date::now() - 2 * 86400)
        .execute(&pool)
        .await
        .unwrap();

    // Default (include_overdue = false): overdue tasks are hidden.
    let config = Config::default();
    let cmd = parse_from(vec![]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(
        !output.contains(name),
        "overdue tasks must be hidden by default: {output:?}"
    );

    // include_overdue = true: the overdue task shows with the OVERDUE marker.
    let mut config = Config::default();
    config.today_view.include_overdue = true;
    let mut out = Vec::new();
    handle_command(parse_from(vec![]).unwrap(), &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains(name), "output: {output:?}");
    assert!(output.contains("OVERDUE"), "output: {output:?}");
}

#[tokio::test]
async fn test_config_view_sections_deserialize() {
    // New subtables deserialize; include_overdue defaults to false; kinds
    // default to text; grid defaults: week_rolling=false,
    // month_rolling=true, week_start=Monday.
    let config: Config = toml::from_str(
        r#"
        [grid]
        week_rolling = true
        month_rolling = false
        week_start = "sunday"
        [tasks_view]
        [today_view]
        include_overdue = true
        "#,
    )
    .unwrap();
    assert!(config.today_view.include_overdue);
    assert!(config.grid.week_rolling);
    assert!(!config.grid.month_rolling);
    assert_eq!(config.grid.week_start, chrono::Weekday::Sun);

    // Unknown sections are rejected (serde deny_unknown_fields on Config).
    let err = toml::from_str::<Config>("[custom.accomplishment]\n").unwrap_err();
    assert!(
        err.to_string().contains("unknown field"),
        "unexpected error: {err}"
    );

    let default: Config = toml::from_str("").unwrap();
    assert!(!default.today_view.include_overdue);
    assert!(
        !default.grid.week_rolling,
        "week_rolling must default to false"
    );
    assert!(
        default.grid.month_rolling,
        "month_rolling must default to true"
    );
    assert_eq!(default.grid.week_start, chrono::Weekday::Mon);
    assert_eq!(default.tracker.get("accomplishment").map(|t| t.kind), None);
}

#[tokio::test]
async fn test_priority_capped_at_max_priority_constant() {
    // The MAX_PRIORITY constant is the single source of truth for the
    // priority validation bound; ensure it stays at 999. Helpers (the
    // cliclack `validate` closures in handle_oneshot_task_creation /
    // handle_recurring_task_creation and the bounds check in
    // `prompt_priority`) all read this constant.
    assert_eq!(
        feeling::prompts::MAX_PRIORITY,
        999,
        "TODO.md requires priority capped to 999 — update ingestion if this changes"
    );
    // And the inclusive range used by validation (1..=999) accepts both
    // boundaries but rejects 0 and 1000.
    let range = 1..=feeling::prompts::MAX_PRIORITY;
    assert!(range.contains(&1), "lower bound must accept 1");
    assert!(range.contains(&999), "upper bound must accept 999");
    assert!(!range.contains(&0), "zero must be rejected");
    assert!(!range.contains(&1000), "1000 must be rejected");
}

// ---------- Mood tracker grid (◯ empty, spaced dots, grid config) ----------

/// Helper: run `:` / `:week` / `:month` / `:year` and return the raw output.
async fn run_tracker(pool: &SqlitePool, config: &Config, arg: &str) -> String {
    let cmd = parse_from(vec![arg.to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, pool, config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    String::from_utf8(out).unwrap()
}

#[tokio::test]
async fn test_mood_tracker_grid_week_rolling_true_full_week() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.grid.week_rolling = true;

    // One mood entry today, via the CLI path.
    let cmd = parse_from(vec!["good".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let output = run_tracker(&pool, &config, ":").await;

    // week_rolling=true always renders the full week: exactly 7 dots.
    assert!(output.contains("Mood tracker (Week)"), "output: {output:?}");
    assert_eq!(output.matches('◯').count(), 6, "output: {output:?}");
    assert_eq!(output.matches('●').count(), 1, "output: {output:?}");
    assert_eq!(output.matches('◯').count() + output.matches('●').count(), 7);
    assert!(output.contains("◯  ◯"), "dots must be spaced: {output:?}");
    assert!(
        !output.contains('·'),
        "empty days must use ◯, not ·: {output:?}"
    );
}

#[tokio::test]
async fn test_mood_tracker_grid_week_default_non_rolling() {
    let pool = test_pool().await.unwrap();
    // Defaults: week_rolling=false, week_start=Monday.
    let config = Config::default();

    let cmd = parse_from(vec!["good".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let output = run_tracker(&pool, &config, ":").await;

    // Non-rolling week = week_start (Monday) through today, so the dot count
    // depends on today's weekday — computed here, never hardcoded.
    use chrono::Datelike;
    let expected = chrono::Local::now().weekday().num_days_from_monday() as i64 + 1;
    assert!(output.contains("Mood tracker (Week)"), "output: {output:?}");
    assert_eq!(
        output.matches('◯').count() as i64,
        expected - 1,
        "output: {output:?}"
    );
    assert_eq!(output.matches('●').count(), 1, "output: {output:?}");
    assert_eq!(
        output.matches('◯').count() as i64 + output.matches('●').count() as i64,
        expected
    );
}

#[tokio::test]
async fn test_mood_tracker_grid_week_start_config() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.grid.week_start = chrono::Weekday::Sun;

    let cmd = parse_from(vec!["good".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let output = run_tracker(&pool, &config, ":").await;

    // Non-rolling week anchored to Sunday: days since the last Sunday.
    use chrono::Datelike;
    let expected = chrono::Local::now().weekday().num_days_from_sunday() as i64 + 1;
    assert_eq!(
        output.matches('◯').count() as i64 + output.matches('●').count() as i64,
        expected,
        "output: {output:?}"
    );
    assert_eq!(output.matches('●').count(), 1, "output: {output:?}");
}

#[tokio::test]
async fn test_mood_tracker_grid_month_rolling_default() {
    let pool = test_pool().await.unwrap();
    // Defaults: month_rolling=true = the subrepo's rolling "last 4 weeks"
    // window: today - 27 days advanced to the week start (Monday), through today.
    let config = Config::default();

    let cmd = parse_from(vec!["good".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let output = run_tracker(&pool, &config, ":month").await;

    // Compute the expected window start independently of the implementation,
    // so the assertion never depends on when the test runs.
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    let mut start = today - chrono::Duration::days(27);
    while start.weekday() != chrono::Weekday::Mon {
        start += chrono::Duration::days(1);
    }
    let expected = (today - start).num_days() + 1;

    assert!(
        output.contains("Mood tracker (Month)"),
        "output: {output:?}"
    );
    assert_eq!(
        output.matches('◯').count() as i64,
        expected - 1,
        "output: {output:?}"
    );
    assert_eq!(output.matches('●').count(), 1, "output: {output:?}");
    assert_eq!(
        output.matches('◯').count() as i64 + output.matches('●').count() as i64,
        expected
    );
    assert!(output.contains("◯  ◯"), "dots must be spaced: {output:?}");
    assert!(
        !output.contains('·'),
        "empty days must use ◯, not ·: {output:?}"
    );
}

#[tokio::test]
async fn test_mood_tracker_grid_month_rolling_false() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.grid.month_rolling = false;

    let cmd = parse_from(vec!["good".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let output = run_tracker(&pool, &config, ":month").await;

    // Non-rolling month = month start through today: day-of-month dots.
    use chrono::Datelike;
    let expected = chrono::Local::now().date_naive().day() as i64;
    assert_eq!(
        output.matches('◯').count() as i64 + output.matches('●').count() as i64,
        expected,
        "output: {output:?}"
    );
    assert_eq!(output.matches('●').count(), 1, "output: {output:?}");
}

// ---- year grid layout tests (grid.year_rolling) ----

#[tokio::test]
async fn test_mood_tracker_grid_year_default_rolling() {
    let pool = test_pool().await.unwrap();
    // Default: year_rolling = true → the calendar-year heatmap: 7 weekday
    // rows, one column per week, dots for Jan 1 through today. The first
    // partial week may open with blank cells when Jan 1 isn't week_start.
    let config = Config::default();

    let cmd = parse_from(vec!["good".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let output = run_tracker(&pool, &config, ":year").await;

    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();

    assert!(output.contains("Mood tracker (Year)"), "output: {output:?}");
    // Header line + exactly 7 grid rows (one per weekday, Monday first).
    assert_eq!(output.lines().count(), 8, "output: {output:?}");
    // One dot per day Jan 1..=today; today is the only filled day.
    assert_eq!(output.matches('●').count(), 1, "output: {output:?}");
    assert_eq!(
        output.matches('·').count() as i64,
        today.ordinal() as i64 - 1,
        "output: {output:?}"
    );
    assert!(
        !output.contains('◯'),
        "year grid must not use the large ◯: {output:?}"
    );
}

#[tokio::test]
async fn test_mood_tracker_grid_year_not_rolling_calendar_layout() {
    let pool = test_pool().await.unwrap();
    // year_rolling = false → calendar year: Jan 1 through today. Unlike the
    // rolling (aligned-to-week-start) mode, the first partial week may open
    // with blank cells when Jan 1 isn't week_start.
    let mut config = Config::default();
    config.grid.year_rolling = false;

    let cmd = parse_from(vec!["good".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let output = run_tracker(&pool, &config, ":year").await;

    use chrono::{Datelike, Weekday};
    let today = chrono::Local::now().date_naive();
    let jan1 = today.with_ordinal(1).unwrap();

    assert!(output.contains("Mood tracker (Year)"), "output: {output:?}");
    // Header line + exactly 7 grid rows (one per weekday, Monday first).
    assert_eq!(output.lines().count(), 8, "output: {output:?}");
    // One dot per day Jan 1..=today; today is the only filled day.
    assert_eq!(output.matches('●').count(), 1, "output: {output:?}");
    assert_eq!(
        output.matches('·').count() as i64,
        today.ordinal() as i64 - 1,
        "output: {output:?}"
    );
    assert!(
        !output.contains('◯'),
        "year grid must not use the large ◯: {output:?}"
    );
    // Calendar-year grid indents the first partial week when Jan 1 is not
    // week_start (the aligned mode is what avoids leading blank cells).
    if jan1.weekday() != Weekday::Mon {
        let first_row = output.lines().nth(1).unwrap();
        assert!(
            first_row.starts_with(' '),
            "calendar-year grid must indent the first partial week: {first_row:?}"
        );
    }
}

// ---- short-id allocation policy tests ----

/// Helper: all short ids in table order. `None` entries are completed
/// (oneshot) tasks whose short id was cleared on completion.
async fn fetch_all_short_ids(pool: &SqlitePool) -> Vec<Option<i64>> {
    let rows = sqlx::query("SELECT short_id FROM todos")
        .fetch_all(pool)
        .await
        .unwrap();
    rows.iter()
        .map(|r| r.get::<Option<i64>, _>("short_id"))
        .collect()
}

#[tokio::test]
async fn test_short_id_allocator_smallest_free_positive() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Create three oneshot tasks; they get short ids 1, 2, 3 in order.
    for s in ["task a", "task b", "task c"] {
        let cmd = parse_from(vec!["!".to_string(), s.to_string()]).unwrap();
        handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
            .await
            .unwrap();
    }
    let mut ids = fetch_all_short_ids(&pool).await;
    ids.sort();
    assert_eq!(
        ids,
        vec![Some(1), Some(2), Some(3)],
        "first three get short ids 1..3: {ids:?}"
    );

    // Delete the middle row directly so the allocator must recycle the gap.
    sqlx::query("DELETE FROM todos WHERE id = 2")
        .execute(&pool)
        .await
        .unwrap();
    let cmd = parse_from(vec!["!".to_string(), "task d".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    let mut ids = fetch_all_short_ids(&pool).await;
    ids.sort();
    // After deleting id=2 (short id 2) the remaining short ids are {1, 3};
    // the smallest free is 2, so task d gets short id 2 (the _set_ becomes
    // {1, 2, 3}, not {1, 2, 4}).
    assert_eq!(
        ids,
        vec![Some(1), Some(2), Some(3)],
        "deleted short id 2 should be reused: {ids:?}"
    );
}

#[tokio::test]
async fn test_completions_clear_short_ids_active_keeps_its() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    for s in ["first", "second", "third"] {
        let cmd = parse_from(vec!["!".to_string(), s.to_string()]).unwrap();
        handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
            .await
            .unwrap();
    }
    // Complete third first, then first. Both lose their short id; "second"
    // stays active and keeps short id 2.
    for s in ["third", "first"] {
        let id: i64 = sqlx::query_scalar("SELECT id FROM todos WHERE name = ?")
            .bind(s)
            .fetch_one(&pool)
            .await
            .unwrap();
        let cmd = parse_from(vec!["-".to_string(), id.to_string()]).unwrap();
        handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
            .await
            .unwrap();
    }

    let mut ids = fetch_all_short_ids(&pool).await;
    ids.sort();
    assert_eq!(
        ids,
        vec![None, None, Some(2)],
        "completed tasks lose their short ids; active 'second' keeps id 2: {ids:?}"
    );

    // A completed task's former short id is immediately free for reuse.
    let cmd = parse_from(vec!["!".to_string(), "fourth".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    let mut ids = fetch_all_short_ids(&pool).await;
    ids.sort();
    assert_eq!(
        ids,
        vec![None, None, Some(1), Some(2)],
        "freed short id 1 is reused by the next task: {ids:?}"
    );
}

#[tokio::test]
async fn test_untoggle_reassigns_smallest_free_short_id() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    let cmd = parse_from(vec!["!".to_string(), "toggle".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    // Complete it: the short id is cleared.
    let cmd = parse_from(vec!["-".to_string(), "1".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    let short_id: Option<i64> =
        sqlx::query_scalar("SELECT short_id FROM todos WHERE name = 'toggle'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(short_id.is_none(), "after complete: {short_id:?}");

    // Undo via the word query form (`- <words> -1`): a completed task has no
    // short id, so it's only addressable by words. Untoggling reassigns the
    // smallest free short id (1 — the completed task's own former slot).
    let cmd = parse_from(vec![
        "-".to_string(),
        "toggle".to_string(),
        "-1".to_string(),
    ])
    .unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    let short_id: Option<i64> =
        sqlx::query_scalar("SELECT short_id FROM todos WHERE name = 'toggle'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(short_id, Some(1), "after undo: {short_id:?}");
}

#[tokio::test]
async fn test_reset_reassigns_short_id_to_completed_task() {
    // Untoggling by *removing todo_completion entries* (the TUI @done reset
    // path) must also reassign the smallest free short id.
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    let cmd = parse_from(vec!["!".to_string(), "restore me".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    let cmd = parse_from(vec!["-".to_string(), "1".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    let short_id: Option<i64> =
        sqlx::query_scalar("SELECT short_id FROM todos WHERE name = 'restore me'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(short_id.is_none(), "after complete: {short_id:?}");

    // Remove the completion rows directly (what the TUI reset does).
    let row_id: i64 = sqlx::query_scalar("SELECT id FROM todos WHERE name = 'restore me'")
        .fetch_one(&pool)
        .await
        .unwrap();
    feeling::sql::reset_task_completions(&pool, row_id, None)
        .await
        .unwrap();

    let short_id: Option<i64> =
        sqlx::query_scalar("SELECT short_id FROM todos WHERE name = 'restore me'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        short_id,
        Some(1),
        "removing completion entries must reassign the smallest free short id: {short_id:?}"
    );
}

// ---- :prune command ----

/// Helper: count completions for a given task id (using its post-reassign id).
async fn completion_count(pool: &SqlitePool, name: &str) -> i64 {
    let id: i64 = sqlx::query_scalar("SELECT id FROM todos WHERE name = ?")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query_scalar("SELECT COUNT(*) FROM todo_completions WHERE todo_id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// `:prune` deletes completed oneshot tasks and their cascaded completions.
#[tokio::test]
async fn test_prune_deletes_completed_task_and_cascades_completions() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Create a oneshot and complete it via the short id (fresh pool: row id
    // == short id == 1). Completion clears the short id but keeps the row.
    let cmd = parse_from(vec!["!".to_string(), "park me".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    let cmd = parse_from(vec!["-".to_string(), "1".to_string(), "3".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    assert_eq!(completion_count(&pool, "park me").await, 1);
    let short_id: Option<i64> =
        sqlx::query_scalar("SELECT short_id FROM todos WHERE name = 'park me'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        short_id.is_none(),
        "pre-prune short id should be cleared: {short_id:?}"
    );

    // :prune should drop the row and the cascaded completion (via ON DELETE
    // CASCADE on todo_completions in db.rs).
    let cmd = parse_from(vec![":prune".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM todos WHERE name = 'park me')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!exists, "pruned task should be gone");

    let completed_orphans: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM todo_completions WHERE todo_id = ?")
            .bind(1)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        completed_orphans, 0,
        "FK cascade should drop the completion: {completed_orphans}"
    );
}

/// `:prune` deletes recurring tasks whose end_time is in the past, leaving
/// open-ended and not-yet-expired recurrings alone.
#[tokio::test]
async fn test_prune_deletes_expired_recurring_task() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    let past = feeling::date::now() - 3600;
    sqlx::query(
        "INSERT INTO todos (name, body, priority, start_time, interval_secs, target_count, optional, end_time) \
         VALUES ('expired', '', 5, ?, 86400, 1, 0, ?)",
    )
    .bind(past - 86_400)
    .bind(past)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO todos (name, body, priority, start_time, interval_secs, target_count, optional, end_time) \
         VALUES ('still going', '', 5, ?, 86400, 1, 0, ?)",
    )
    .bind(past)
    .bind(past + 86_400)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO todos (name, body, priority, start_time, interval_secs, target_count, optional, end_time) \
         VALUES ('forever', '', 5, ?, 86400, 1, 0, NULL)",
    )
    .bind(past)
    .execute(&pool)
    .await
    .unwrap();

    let cmd = parse_from(vec![":prune".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let names: Vec<String> = sqlx::query("SELECT name FROM todos")
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.get::<String, _>("name"))
        .collect();
    assert!(!names.contains(&"expired".to_string()));
    assert!(names.contains(&"still going".to_string()));
    assert!(names.contains(&"forever".to_string()));
}

/// `:prune` clears the `embedding_cache` table entirely — it is a cache;
/// entries are lazily re-embedded on the next use.
#[tokio::test]
async fn test_prune_clears_embedding_cache() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    let key1 = format!("{}happy", config.moods.axes.prefix_string);
    let key2 = format!("{}obsolete_mood", config.moods.axes.prefix_string);

    // Populate the embedding cache with two entries
    sqlx::query("INSERT INTO embedding_cache (text, embedding) VALUES ($1, x'00000000')")
        .bind(&key1)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO embedding_cache (text, embedding) VALUES ($1, x'00000000')")
        .bind(&key2)
        .execute(&pool)
        .await
        .unwrap();

    let cache_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM embedding_cache")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cache_before, 2);

    // Run :prune
    let cmd = parse_from(vec![":prune".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let cache_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM embedding_cache")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cache_after, 0, "prune should clear the whole embedding cache");
}

// ---- FK cascade on delete (CASCADE propagation to todo_completions) ----

/// Deleting a task must remove its `todo_completions` rows via the
/// `ON DELETE CASCADE` declared in db.rs.
#[tokio::test]
async fn test_delete_task_cascades_completions() {
    let pool = test_pool().await.unwrap();
    // let config = Config::default();

    let id = create_oneshot_task(&pool, "to cull").await;
    sqlx::query("INSERT INTO todo_completions (todo_id, time, count) VALUES (?, ?, 1)")
        .bind(id)
        .bind(feeling::date::now())
        .execute(&pool)
        .await
        .unwrap();
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM todo_completions WHERE todo_id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, 1, "seed completion");

    sqlx::query("DELETE FROM todos WHERE id = ?")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM todo_completions WHERE todo_id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after, 0, "FK CASCADE should drop completions");
}

/// The id-allocator's reassignment (positive to negative on completion) must
/// cascade to `todo_completions.todo_id` via `ON UPDATE CASCADE`.
///
/// The id-reassignment design was removed in favor of a stable autoincrement
/// row id plus a nullable user-facing `short_id`, so `ON UPDATE CASCADE` no
/// longer exists and there is nothing to cascade — this test is obsolete.
/// The short-id lifecycle is covered by the short-id allocation tests.

// ---- :config bundled-copy behavior ----

/// When the live config path doesn't exist, `:config` must copy the bundled
/// `assets/config.toml` verbatim. `FEELING_CONFIG_DIR` redirects that path
/// to a temporary directory for isolation; `EDITOR=true` short-circuits the
/// spawned editor so the test runs without leaving us at a real prompt.
#[tokio::test]
async fn test_config_copies_bundled_when_missing() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let prev = std::env::var("FEELING_CONFIG_DIR").ok();
    // SAFETY: env-mutation in tests is process-wide; other tests that
    // read FEELING_CONFIG_DIR are sequenced rather than concurrent within
    // tokio::test runs.
    std::env::set_var("FEELING_CONFIG_DIR", temp.path());

    let pool = test_pool().await.unwrap();
    let config = Config::default();

    let prev_editor = std::env::var("EDITOR").ok();
    std::env::set_var("EDITOR", "true");

    let cmd = parse_from(vec![":config".to_string()]).unwrap();
    let r = handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false).await;
    if let Some(p) = prev_editor {
        std::env::set_var("EDITOR", p);
    } else {
        std::env::remove_var("EDITOR");
    }

    assert!(r.is_ok(), ":config copy-on-missing should succeed: {r:?}");

    // paths.rs appends a profile-suffixed file name (dev.toml / toml);
    // we look for any *.toml in the redirected temp dir.
    let entries: Vec<_> = std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("toml"))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "exactly one config target should be created: {:?}",
        entries.iter().map(|e| e.path()).collect::<Vec<_>>()
    );
    let written = std::fs::read_to_string(entries[0].path()).unwrap();
    assert_eq!(
        written,
        feeling::config::DEFAULT_CONFIG,
        "copied file must be byte-identical to the bundled defaults"
    );

    if let Some(p) = prev {
        std::env::set_var("FEELING_CONFIG_DIR", p);
    } else {
        std::env::remove_var("FEELING_CONFIG_DIR");
    }
}

#[tokio::test]
async fn test_bundled_config_defaults_load_through_serde() {
    // The bundled `assets/config.toml` ships with hex RGB endpoints via the
    // crossterm serde `#RRGGBB` format. If this test fails on a serde
    // error, the bundled config has drifted from the parse path (e.g.
    // someone changed a hex literal in `default_axes()` without updating
    // `assets/config.toml`). It deliberately does NOT assert exact RGB
    // values — tweak those freely for palette work without breaking the
    // contract.
    let cfg: Config = toml::from_str(feeling::config::DEFAULT_CONFIG)
        .expect("bundled DEFAULT_CONFIG must deserialize");
    assert!(!cfg.moods.pairs.is_empty());

    // Mood names and order are valid.
    assert_eq!(cfg.moods.pairs[0].mood, "happy");
    assert_eq!(cfg.moods.pairs[1].mood, "sad");
}

// ---------- Event-loop architecture & new TUI actions ----------

/// The TUI render loops use the same SQL operations the CLI does; these
/// tests pin the semantics the action handlers rely on.

#[tokio::test]
async fn test_delete_feeling_removes_linked_custom_rows() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );

    // Insert a feeling with a linked custom row (like `feeling ok -sleep 8`).
    let cmd = parse_from(vec![
        "ok".to_string(),
        "-sleep".to_string(),
        "8".to_string(),
    ])
    .unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let feeling_id: i64 = sqlx::query_scalar("SELECT id FROM feeling WHERE mood = 'ok'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let linked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM custom WHERE feeling = ?")
        .bind(feeling_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(linked, 1);

    // The today TUI delete path: delete custom rows first (FK, no cascade),
    // then the feeling row, in a transaction.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM custom WHERE feeling = ?")
        .bind(feeling_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM feeling WHERE id = ?")
        .bind(feeling_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let feelings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feeling")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(feelings, 0);
    let customs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM custom")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(customs, 0);
}

#[tokio::test]
async fn test_delete_feeling_without_cascade_fails_with_fk_enforced() {
    // The custom.feeling FK has no ON DELETE CASCADE, so deleting a feeling
    // row while linked custom rows still exist must fail under PRAGMA
    // foreign_keys = ON. This is why the today delete path deletes custom
    // rows first.
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );

    let cmd = parse_from(vec![
        "ok".to_string(),
        "-sleep".to_string(),
        "8".to_string(),
    ])
    .unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    let feeling_id: i64 = sqlx::query_scalar("SELECT id FROM feeling WHERE mood = 'ok'")
        .fetch_one(&pool)
        .await
        .unwrap();

    let r = sqlx::query("DELETE FROM feeling WHERE id = ?")
        .bind(feeling_id)
        .execute(&pool)
        .await;
    assert!(
        r.is_err(),
        "FK must block deleting a feeling with linked customs"
    );
}

#[tokio::test]
async fn test_edit_todo_body_updates_in_place() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Create a task with a body via `! name .. body`.
    let cmd = parse_from(vec![
        "!".to_string(),
        "ship it".to_string(),
        "..".to_string(),
        "initial body".to_string(),
    ])
    .unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    let task_id: i64 = sqlx::query_scalar("SELECT id FROM todos WHERE name = 'ship it'")
        .fetch_one(&pool)
        .await
        .unwrap();

    // The TUI edit path: UPDATE todos SET body = ? WHERE id = ?.
    sqlx::query("UPDATE todos SET body = ? WHERE id = ?")
        .bind("rewritten body")
        .bind(task_id)
        .execute(&pool)
        .await
        .unwrap();

    let body: String = sqlx::query_scalar("SELECT body FROM todos WHERE id = ?")
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(body, "rewritten body");
    // Name is untouched.
    let name: String = sqlx::query_scalar("SELECT name FROM todos WHERE id = ?")
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "ship it");
}

#[tokio::test]
async fn test_edit_custom_text_payload() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "note".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Text,
        },
    );

    let cmd = parse_from(vec!["-note".to_string(), "hello".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    let custom_id: i64 = sqlx::query_scalar("SELECT id FROM custom WHERE type = 'note'")
        .fetch_one(&pool)
        .await
        .unwrap();

    sqlx::query("UPDATE custom SET score = ? WHERE id = ?")
        .bind("edited text")
        .bind(custom_id)
        .execute(&pool)
        .await
        .unwrap();

    let score: String = sqlx::query_scalar("SELECT score FROM custom WHERE id = ?")
        .bind(custom_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(score, "edited text");
}

#[tokio::test]
async fn test_edit_custom_float_payload() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );

    let cmd = parse_from(vec!["-sleep".to_string(), "8".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    let custom_id: i64 = sqlx::query_scalar("SELECT id FROM custom WHERE type = 'sleep'")
        .fetch_one(&pool)
        .await
        .unwrap();

    // The EditTracker modal path: float kind parses f64, then UPDATE.
    sqlx::query("UPDATE custom SET score = ? WHERE id = ?")
        .bind(7.5f64)
        .bind(custom_id)
        .execute(&pool)
        .await
        .unwrap();

    let score: f64 = sqlx::query_scalar("SELECT score FROM custom WHERE id = ?")
        .bind(custom_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(score, 7.5);
}

#[tokio::test]
async fn test_edit_feeling_body() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    let cmd = parse_from(vec![
        "calm".to_string(),
        "..".to_string(),
        "original note".to_string(),
    ])
    .unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    let feeling_id: i64 = sqlx::query_scalar("SELECT id FROM feeling WHERE mood = 'calm'")
        .fetch_one(&pool)
        .await
        .unwrap();

    sqlx::query("UPDATE feeling SET body = ? WHERE id = ?")
        .bind("revised note")
        .bind(feeling_id)
        .execute(&pool)
        .await
        .unwrap();

    let body: String = sqlx::query_scalar("SELECT body FROM feeling WHERE id = ?")
        .bind(feeling_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(body, "revised note");
}

#[tokio::test]
async fn test_reset_progress_oneshot_clears_all_completions() {
    let pool = test_pool().await.unwrap();

    let task_id = create_oneshot_task(&pool, "reset me").await;
    sqlx::query("INSERT INTO todo_completions (todo_id, time, count) VALUES (?, ?, ?)")
        .bind(task_id)
        .bind(feeling::date::now())
        .bind(1)
        .execute(&pool)
        .await
        .unwrap();

    // The @done reset path for a oneshot task: delete all completions.
    sqlx::query("DELETE FROM todo_completions WHERE todo_id = ?")
        .bind(task_id)
        .execute(&pool)
        .await
        .unwrap();

    let total = feeling::task::apply_completion_delta(&pool, task_id, 0)
        .await
        .unwrap();
    assert_eq!(
        total, 0,
        "oneshot task should have no completions after reset"
    );
}

#[tokio::test]
async fn test_reset_progress_recurring_only_current_interval() {
    let pool = test_pool().await.unwrap();

    // Recurring task with a 1-day interval, started 3 days + 500s ago.
    let interval = 86_400i64;
    let now = feeling::date::now();
    let start_time = now - 3 * interval - 500;
    let name = "daily reset";
    sqlx::query(
        "INSERT INTO todos (name, body, priority, interval_secs, available_duration_secs, target_count, optional, start_time) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(name)
    .bind("")
    .bind(5)
    .bind(interval)
    .bind::<Option<i64>>(None)
    .bind(0)
    .bind(0)
    .bind(start_time)
    .execute(&pool)
    .await
    .unwrap();
    let task_id: i64 = sqlx::query_scalar("SELECT id FROM todos WHERE name = ?")
        .bind(name)
        .fetch_one(&pool)
        .await
        .unwrap();

    let interval_start = feeling::task::current_interval_start(start_time, interval, now);
    sqlx::query("INSERT INTO todo_completions (todo_id, time, count) VALUES (?, ?, ?)")
        .bind(task_id)
        .bind(interval_start - 100)
        .bind(2)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO todo_completions (todo_id, time, count) VALUES (?, ?, ?)")
        .bind(task_id)
        .bind(interval_start + 100)
        .bind(3)
        .execute(&pool)
        .await
        .unwrap();

    // The @done reset path for a recurring task: delete only completions
    // at/after the current interval start (same floor as the views use).
    sqlx::query("DELETE FROM todo_completions WHERE todo_id = ? AND time >= ?")
        .bind(task_id)
        .bind(interval_start)
        .execute(&pool)
        .await
        .unwrap();

    let prev_sum: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(count), 0) FROM todo_completions WHERE todo_id = ? AND time < ?",
    )
    .bind(task_id)
    .bind(interval_start)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(prev_sum, 2, "previous intervals must be preserved");

    let total = feeling::task::apply_completion_delta(&pool, task_id, 0)
        .await
        .unwrap();
    assert_eq!(total, 0, "current interval must be empty after reset");
}

#[tokio::test]
async fn test_fetch_today_entries_carries_custom_ids() {
    // The today view's Edit/Delete dispatch relies on custom entries
    // carrying their row id so the SQL update/delete can target them.
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerType::Float,
        },
    );

    let cmd = parse_from(vec!["-sleep".to_string(), "8".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let embedder = feeling::embed::global_embedder();
    config.moods.init_with(&pool, embedder).await.unwrap();

    let mut color_cache = std::collections::HashMap::new();
    let entries = feeling::views::fetch_today_entries(
        &pool,
        &config,
        feeling::views::TodayHorizon::Today,
        &mut color_cache,
    )
    .await
    .unwrap();
    let custom = entries
        .iter()
        .find(|e| e.entry_type == "custom")
        .expect("custom entry must appear in today view");
    assert!(custom.id.is_some(), "custom entry must carry its row id");

    // And the id must match the DB row.
    let db_id: i64 = sqlx::query_scalar("SELECT id FROM custom WHERE type = 'sleep'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(custom.id, Some(db_id));
}

#[tokio::test]
async fn test_fetch_today_entries_completion_carries_task_id() {
    // The today view's Enter-on-completed-task support relies on ✓
    // completion entries carrying their todo_id so the render loop can
    // resolve the task (toggle once-tasks, count dialog for recurring).
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Create a oneshot task due today, then complete it.
    let today_str = chrono::Local::now().format("%Y-%m-%d").to_string();
    let cmd = parse_from(vec![
        "!".to_string(),
        "completed task".to_string(),
        format!("@{today_str}"),
    ])
    .unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();
    let task_id: i64 = sqlx::query_scalar("SELECT id FROM todos WHERE name = 'completed task'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let update_cmd = parse_from(vec!["-".to_string(), task_id.to_string()]).unwrap();
    handle_command(update_cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let mut config = config;
    let embedder = feeling::embed::global_embedder();
    config.moods.init_with(&pool, embedder).await.unwrap();

    let mut color_cache = std::collections::HashMap::new();
    let entries = feeling::views::fetch_today_entries(
        &pool,
        &config,
        feeling::views::TodayHorizon::Today,
        &mut color_cache,
    )
    .await
    .unwrap();
    let completion = entries
        .iter()
        .find(|e| e.entry_type == "completion")
        .expect("completion entry must appear in today view");
    assert_eq!(
        completion.task_id,
        Some(task_id),
        "completion entry must carry its todo_id"
    );
}

#[tokio::test]
async fn test_clear_command() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Create a feeling entry for today
    let cmd = parse_from(vec!["feeling".to_string(), "good".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feeling")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    // Clear entries for today (non-interactive mode in tests)
    let clear_cmd = parse_from(vec![":clear".to_string()]).unwrap();
    handle_command(clear_cmd, &pool, &config, &CliOpts::default(), &mut Vec::new(), false)
        .await
        .unwrap();

    let count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feeling")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count_after, 0);
}
