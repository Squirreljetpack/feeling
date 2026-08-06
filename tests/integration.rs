//! Integration tests for the feeling CLI.
//!
//! These tests verify the full flow from CLI parsing through database operations.

use feeling::{
    clap::{parse_from, CliOpts},
    config::{Config, TrackerKind},
    db::test_pool,
    handlers::handle_command,
};
use sqlx::{Row, SqlitePool};

/// Helper: create a oneshot task and return its id
async fn create_oneshot_task(pool: &SqlitePool, name: &str) -> i64 {
    let cmd = parse_from(vec!["!".to_string(), name.to_string()]).unwrap();
    let config = Config::default();
    handle_command(
        cmd,
        pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    sqlx::query_scalar::<_, i64>("SELECT id FROM todos WHERE name = ?")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Helper: insert a completion entry with an explicit time and count.
/// Unlike `feeling::sql::update_task` (which stamps `now()` and applies
/// interval logic), this writes the row directly.
async fn update_task(pool: &SqlitePool, todo_id: i64, time: i64, count: i32) {
    sqlx::query("INSERT INTO todo_completions (todo_id, time, count) VALUES (?, ?, ?)")
        .bind(todo_id)
        .bind(time)
        .bind(count)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_create_feeling_entry() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    let cmd = parse_from(vec!["comfortably".to_string(), "numb".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
            kind: TrackerKind::Float,
            colors: None,
        },
    );
    config.tracker.insert(
        "water".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerKind::Float,
            colors: None,
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

    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
            kind: TrackerKind::Float,
            colors: None,
        },
    );

    let cmd = parse_from(vec!["-sleep".to_string(), "10".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
            kind: TrackerKind::Text,
            colors: None,
        },
    );
    // float + interval: re-logging replaces the previous entry in the slot
    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: Some(86400),
            min: None,
            max: None,
            kind: TrackerKind::Float,
            colors: None,
        },
    );
    // number + interval: plain insert, accumulates
    config.tracker.insert(
        "runs".to_string(),
        feeling::config::TrackerSetting {
            interval: Some(86400),
            min: None,
            max: None,
            kind: TrackerKind::Number,
            colors: None,
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
        handle_command(
            cmd,
            &pool,
            &config,
            &CliOpts::default(),
            &mut Vec::new(),
            false,
        )
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
            kind: TrackerKind::Float,
            colors: None,
        },
    );
    for _ in 0..2 {
        let cmd = parse_from(vec!["-water".to_string(), "1".to_string()]).unwrap();
        handle_command(
            cmd,
            &pool,
            &config,
            &CliOpts::default(),
            &mut Vec::new(),
            false,
        )
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

    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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

    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    let task = sqlx::query("SELECT name, start_time, end_time FROM todos")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(task.get::<String, _>("name"), "scheduled task");
    // `@<time>` is the due time: end_time is set to the specified date at
    // midnight, while start_time records the creation moment.
    let end_time: i64 = task.get("end_time");
    assert_eq!(
        end_time,
        feeling::date::parse_datetime("2024-03-20", config.date.dialect).unwrap()
    );
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
            kind: TrackerKind::Float,
            colors: None,
        },
    );

    // min/max are only for binning (color mapping), not for gating
    // insertion: below-min, in-range, and above-max values all store.
    for value in ["3", "7", "11"] {
        let cmd = parse_from(vec!["-sleep".to_string(), value.to_string()]).unwrap();
        handle_command(
            cmd,
            &pool,
            &config,
            &CliOpts::default(),
            &mut Vec::new(),
            false,
        )
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
            kind: TrackerKind::Float,
            colors: None,
        },
    );
    config.tracker.insert(
        "water".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerKind::Float,
            colors: None,
        },
    );
    config.tracker.insert(
        "exercise".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerKind::Float,
            colors: None,
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

    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd1,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    let cmd2 = parse_from(vec!["!".to_string(), "high priority task".to_string()]).unwrap();
    handle_command(
        cmd2,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    // View pending oneshots via @:o (bare `!` is interactive creation now)
    let cmd = parse_from(vec!["@:o".to_string()]).unwrap();
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
        assert_eq!(fields[5], "○", "not-started status: {line:?}");
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    let result = handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await;
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
    let result = handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await;
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    let result = handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await;
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
    let result = handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("2 tasks match"), "got: {msg}");
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
            kind: TrackerKind::Float,
            colors: None,
        },
    );
    config.tracker.insert(
        "water".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerKind::Float,
            colors: None,
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
async fn test_out_of_range_custom_still_inserts() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();

    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: Some(4.0),
            max: Some(10.0),
            kind: TrackerKind::Float,
            colors: None,
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

    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    let result = handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("tab characters"));
}

#[tokio::test]
async fn test_unknown_tracker_rejected() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Unknown tracker should be rejected
    let cmd = parse_from(vec!["-unknown".to_string(), "5".to_string()]).unwrap();

    let result = handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await;
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
    let result = feeling::views::handle_today(
        &pool,
        &config,
        None,
        feeling::clap::ShowVariant::All,
        feeling::views::TodayHorizon::Today,
        &CliOpts::default(),
        &mut out,
    )
    .await;
    assert!(result.is_ok());
    let output = String::from_utf8(out).unwrap();
    assert!(
        output.contains("Nothing logged today."),
        "output: {output:?}"
    );
}

#[tokio::test]
async fn test_today_view_with_data() {
    use feeling::config::{TrackerKind, TrackerSetting};

    let pool = test_pool().await.unwrap();
    let mut config = Config::default();

    // Register custom trackers
    config.tracker.insert(
        "sleep".to_string(),
        TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerKind::Float,
            colors: None,
        },
    );
    config.tracker.insert(
        "water".to_string(),
        TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerKind::Float,
            colors: None,
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
    let result = feeling::views::handle_today(
        &pool,
        &config,
        None,
        feeling::clap::ShowVariant::All,
        feeling::views::TodayHorizon::Today,
        &CliOpts::default(),
        &mut out,
    )
    .await;
    assert!(result.is_ok());
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("good"), "output: {output:?}");
    assert!(output.contains("due today"), "output: {output:?}");
    assert!(output.contains('\t'), "output: {output:?}");
}

/// `feeling @<date>` anchors the today view to an arbitrary day.
#[tokio::test]
async fn test_today_view_with_date() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config
        .moods
        .init_with(&pool, feeling::embed::global_embedder())
        .await
        .unwrap();

    // Seed a feeling on a fixed past date directly.
    let target = feeling::date::parse_datetime("2024-03-15 09:00", Default::default()).unwrap();
    sqlx::query("INSERT INTO feeling (mood, body, time) VALUES ('ancient', '', ?)")
        .bind(target)
        .execute(&pool)
        .await
        .unwrap();

    // `feeling @2024-03-15` lists it.
    let cmd = parse_from(vec!["@2024-03-15".to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("ancient"), "output: {output:?}");

    // Plain `feeling` (today) does not.
    let cmd = parse_from(vec![]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(!output.contains("ancient"), "output: {output:?}");
}

/// `feeling.score` round-trips through the sql layer (nullable REAL column).
#[tokio::test]
async fn test_feeling_score_roundtrip() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // CLI-created entries compute the saliency at insert time.
    let cmd = parse_from(vec!["vivid".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();
    let rows = feeling::sql::fetch_feelings_between(&pool, 0, i64::MAX)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].score.is_some(),
        "CLI-created entries carry their computed saliency"
    );

    // Rows without a score (e.g. seed_db inserts) read back as None and
    // round-trip through update_feeling_score.
    let id = rows[0].id;
    sqlx::query("UPDATE feeling SET score = NULL WHERE id = ?")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let rows = feeling::sql::fetch_feelings_between(&pool, 0, i64::MAX)
        .await
        .unwrap();
    assert_eq!(rows[0].score, None);
    feeling::sql::update_feeling_score(&pool, id, 0.42)
        .await
        .unwrap();
    let rows = feeling::sql::fetch_feelings_between(&pool, 0, i64::MAX)
        .await
        .unwrap();
    assert!((rows[0].score.unwrap() - 0.42).abs() < 1e-6);
}

/// The first render pass backfills `feeling.score` (mood saliency); a
/// pre-seeded score is left untouched (read-back path).
#[tokio::test]
async fn test_today_view_backfills_feeling_score() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config
        .moods
        .init_with(&pool, feeling::embed::global_embedder())
        .await
        .unwrap();

    // Two moods: one fresh, one pre-seeded.
    let cmd = parse_from(vec!["vivid".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();
    let cmd = parse_from(vec!["glum".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();
    let glum_id: i64 = sqlx::query_scalar("SELECT id FROM feeling WHERE mood = 'glum'")
        .fetch_one(&pool)
        .await
        .unwrap();
    feeling::sql::update_feeling_score(&pool, glum_id, 0.5)
        .await
        .unwrap();

    // A directly-inserted row (no score) exercises the backfill path.
    sqlx::query("INSERT INTO feeling (mood, body, time) VALUES ('dull', '', ?)")
        .bind(feeling::date::now())
        .execute(&pool)
        .await
        .unwrap();

    // A fresh render pass (new color cache) runs the pipeline and backfills.
    let mut out = Vec::new();
    feeling::views::handle_today(
        &pool,
        &config,
        None,
        feeling::clap::ShowVariant::All,
        feeling::views::TodayHorizon::Today,
        &CliOpts::default(),
        &mut out,
    )
    .await
    .unwrap();

    let scores: Vec<Option<f32>> = sqlx::query_scalar("SELECT score FROM feeling ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(scores.len(), 3);
    assert!(
        scores[0].is_some(),
        "CLI-created row carries its computed score"
    );
    assert_eq!(scores[1], Some(0.5), "pre-seeded score must be unchanged");
    assert!(
        scores[2].is_some(),
        "directly-inserted row must be backfilled with a score"
    );
}

#[tokio::test]
async fn test_view_done_tasks() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Create a oneshot task, then complete it
    let task_id = create_oneshot_task(&pool, "finished task").await;
    let update_cmd = parse_from(vec!["-".to_string(), task_id.to_string()]).unwrap();
    handle_command(
        update_cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    // @done should list the completed task; done oneshots render ✓.
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
        // Done oneshot task → ✓ badge (no "DONE" suffix anymore).
        assert!(fields[5].contains('✓'), "badge dot expected: {line:?}");
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    // @due opens the TodayView at ShowVariant::B — today-view rows have
    // 4 tab-separated columns: time, badge, label, detail.
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
            4,
            "line not tab-separated into 4 today-view columns: {line:?}"
        );
    }
}

/// Insert a recurring task row directly and return its id.
async fn insert_recurring_task(
    pool: &SqlitePool,
    name: &str,
    start_time: i64,
    interval: i64,
    available_duration: Option<i64>,
    target_count: i32,
    end_time: Option<i64>,
) -> i64 {
    sqlx::query(
        "INSERT INTO todos (name, body, priority, interval_secs, available_duration_secs, target_count, optional, start_time, end_time) \
         VALUES (?, '', 5, ?, ?, ?, 0, ?, ?)",
    )
    .bind(name)
    .bind(interval)
    .bind(available_duration)
    .bind(target_count)
    .bind(start_time)
    .bind(end_time)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query_scalar::<_, i64>("SELECT id FROM todos WHERE name = ?")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Run a view command and return its raw output.
async fn run_view(pool: &SqlitePool, config: &Config, args: &[&str]) -> String {
    let cmd = parse_from(args.iter().map(|s| s.to_string()).collect()).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, pool, config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    String::from_utf8(out).unwrap()
}

/// `@done:O` shows ALL recurring tasks (one row per task, no completions
/// filter): history rows with entries in earlier intervals (unscoped sum),
/// expired tasks, and even never-completed ones — unlike `@done` (All),
/// which needs done-in-current-interval and excludes expired tasks (D3).
/// entry ever (unscoped sum), including expired ones — unlike `@done` (All),
/// which needs done-in-current-interval and excludes expired tasks (D3).
#[tokio::test]
async fn test_view_done_b_recurring_history() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();
    let interval = 86_400i64;
    let now = feeling::date::now();

    // Recurring with completions only in the FIRST interval: unscoped sum 2
    // (history), current-interval sum 0 (not done now).
    let start = now - 2 * interval - 500;
    let history_id =
        insert_recurring_task(&pool, "history task", start, interval, None, 2, None).await;
    update_task(&pool, history_id, start + 100, 1).await;
    update_task(&pool, history_id, start + 200, 1).await;

    // Expired recurring with a single completion ever (end_time passed).
    let expired_id = insert_recurring_task(
        &pool,
        "expired task",
        now - 10 * interval,
        interval,
        None,
        1,
        Some(now - 1000),
    )
    .await;
    update_task(&pool, expired_id, now - 10 * interval + 100, 1).await;

    // Never-completed recurring (zero entries ever).
    let _fresh_id = insert_recurring_task(
        &pool,
        "fresh task",
        now - 2 * interval,
        interval,
        None,
        1,
        None,
    )
    .await;

    // @done (All): none appear — history task isn't done in the current
    // interval, expired task is excluded.
    let all = run_view(&pool, &config, &["@done"]).await;
    assert!(!all.contains("history task"), "@done All: {all:?}");
    assert!(!all.contains("expired task"), "@done All: {all:?}");

    // @done:O (B): all three appear (ALL R — unscoped history, expired and
    // never-completed rows included).
    let b = run_view(&pool, &config, &["@done:O"]).await;
    assert!(b.contains("history task"), "@done:O: {b:?}");
    assert!(b.contains("expired task"), "@done:O: {b:?}");
    assert!(b.contains("fresh task"), "@done:O: {b:?}");
}

/// `@done:O` partial-history rows (recurring with target > 1, entries ever
/// but sum < target — not `is_done()`) sort by their last completion
/// entry, not by a future window end (which would push them to the bottom
/// of the done list as if "due in the future").
#[tokio::test]
async fn test_done_b_partial_history_sorts_by_last_completion() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();
    let interval = 86_400i64;
    let now = feeling::date::now();

    // Partial history: target 3, one entry 2 days ago. Its availability
    // window end is in the future — the buggy sort key.
    let partial = insert_recurring_task(
        &pool,
        "partial task",
        now - 10 * interval,
        interval,
        None,
        3,
        None,
    )
    .await;
    update_task(&pool, partial, now - 2 * interval, 1).await;

    // Done task completed 3 days ago (older than the partial's entry).
    let older_done = insert_recurring_task(
        &pool,
        "older done task",
        now - 10 * interval,
        interval,
        None,
        1,
        None,
    )
    .await;
    update_task(&pool, older_done, now - 3 * interval, 1).await;

    // Done task completed 1 hour ago.
    let recent_done = insert_recurring_task(
        &pool,
        "recent done task",
        now - 10 * interval,
        interval,
        None,
        1,
        None,
    )
    .await;
    update_task(&pool, recent_done, now - 3600, 1).await;

    // Date-descending: recent done, partial (2d ago), older done (3d ago).
    // With the buggy key (partial = future window end) the partial row
    // would land last.
    let done = run_view(&pool, &config, &["@done:O"]).await;
    let recent_pos = done.find("recent done task").expect("recent row");
    let partial_pos = done.find("partial task").expect("partial row");
    let older_pos = done.find("older done task").expect("older row");
    assert!(recent_pos < partial_pos, "order: {done:?}");
    assert!(
        partial_pos < older_pos,
        "partial-history row sorts by last completion, not a future window end: {done:?}"
    );
}

/// `@:O` shows recurring tasks whose availability window has passed (not
/// expired), while `@` (All) filters them out via `recurring_available`.
#[tokio::test]
async fn test_view_pending_b_not_availability_filtered() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();
    let interval = 86_400i64;
    let now = feeling::date::now();

    // Availability window [now-2h, now-1h) — passed, but not expired.
    let id = insert_recurring_task(
        &pool,
        "window passed task",
        now - 7200,
        interval,
        Some(3600),
        0,
        None,
    )
    .await;

    // @ (All): excluded by the availability filter.
    let all = run_view(&pool, &config, &["@"]).await;
    assert!(!all.contains("window passed task"), "@ All: {all:?}");

    // @:O (B): included — no availability post-filter.
    let b = run_view(&pool, &config, &["@:O"]).await;
    assert!(b.contains("window passed task"), "@:O: {b:?}");

    // The row itself exists (sanity).
    let _ = id;
}

/// The today view includes any task with a completion entry on the anchored
/// day, even when it would not appear in the regular task lists (recurring
/// availability window passed), and the merged row's time cell is the last
/// completion timestamp (VIEWS.md time-label rule).
#[tokio::test]
async fn test_today_view_completed_today_inclusion_and_time_label() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    let embedder = feeling::embed::global_embedder();
    config.moods.init_with(&pool, embedder).await.unwrap();

    let interval = 86_400i64;
    let anchored_day = feeling::date::today_start() - 2 * 86_400;

    // A: always available (no duration); completion at 10:30 on the anchored
    // day, outside the current interval.
    let a = insert_recurring_task(
        &pool,
        "completed always",
        anchored_day + 6 * 3600,
        interval,
        None,
        0,
        None,
    )
    .await;
    let a_time = anchored_day + 10 * 3600 + 30 * 60;
    update_task(&pool, a, a_time, 1).await;

    // B: availability window passed on the anchored day (08:00-09:00), so
    // the regular availability-filtered recurring fetch drops it; only the
    // completed-today merge surfaces it.
    let b = insert_recurring_task(
        &pool,
        "completed window passed",
        anchored_day + 8 * 3600,
        interval,
        Some(3600),
        0,
        None,
    )
    .await;
    let b_time = anchored_day + 10 * 3600;
    update_task(&pool, b, b_time, 1).await;

    let mut color_cache = std::collections::HashMap::new();
    let entries = feeling::views::fetch_today_entries(
        &pool,
        &config,
        feeling::views::TodayHorizon::Today,
        Some(anchored_day),
        feeling::clap::ShowVariant::All,
        &mut color_cache,
    )
    .await
    .unwrap();

    let row = |name: &str| {
        entries
            .iter()
            .find(|e| e.kind.is_task() && e.label == name)
            .expect("task row must appear in the today view")
    };
    // Time cell = last completion timestamp on the anchored day.
    assert_eq!(row("completed always").time_label, "10:30");
    assert_eq!(row("completed window passed").time_label, "10:00");
    // Both are done in the current interval? No — the badge reflects the
    // current-interval state (D8): zero completions in the current interval
    // → not done ↻.
    let _ = row("completed always").badge;
    let _ = row("completed window passed").badge;

    // The A variant filters completed tasks out and shows no completions.
    // Neither task is done in the current interval, so both keep their
    // regular rows — the window-passed task is active earlier in the day
    // (interval-aware period overlap) and shows its window-end time cell.
    let entries_a = feeling::views::fetch_today_entries(
        &pool,
        &config,
        feeling::views::TodayHorizon::Today,
        Some(anchored_day),
        feeling::clap::ShowVariant::A,
        &mut color_cache,
    )
    .await
    .unwrap();
    let a_win = entries_a
        .iter()
        .find(|e| e.label == "completed window passed")
        .expect("window-passed task stays in A (active earlier in the day)");

    let now = feeling::date::now();
    let st = anchored_day + 8 * 3600;
    let interval_start = if now <= st {
        st
    } else {
        st + ((now - st).div_euclid(interval)) * interval
    };
    let window_end = interval_start + 3600;
    let expected_a_win_time = if now < window_end {
        window_end
    } else {
        interval_start + interval
    };
    let expected_a_win_label = if feeling::date::day_start(expected_a_win_time) == anchored_day {
        feeling::date::format_time(expected_a_win_time)
    } else {
        format!(
            "{} {}",
            feeling::date::format_weekday(expected_a_win_time),
            feeling::date::format_time(expected_a_win_time)
        )
    };

    assert_eq!(
        a_win.time_label, expected_a_win_label,
        "window-passed recurring row shows the next interval start"
    );
    let a_row = entries_a
        .iter()
        .find(|e| e.label == "completed always")
        .expect("always-available task stays in A");
    assert_eq!(
        a_row.time_label, "",
        "regular recurring row has no time cell"
    );
}

/// D9: a just-completed task stays visible in `@` (All) within
/// `persist_pending_seconds`, and disappears once the window passes.
#[tokio::test]
async fn test_persist_pending_seconds() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();
    assert_eq!(config.tasks_view.persist_pending_seconds, 5 * 60);

    // Create + complete a oneshot task.
    let task_id = create_oneshot_task(&pool, "just finished").await;
    let update_cmd = parse_from(vec!["-".to_string(), task_id.to_string()]).unwrap();
    handle_command(
        update_cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    // Still in `@` right after completing (the persist window holds it).
    let pending = run_view(&pool, &config, &["@"]).await;
    assert!(pending.contains("just finished"), "@ All: {pending:?}");

    // Backdate the completion past the persist window: it disappears from
    // `@` (done + outside the window) but stays in `@done`.
    sqlx::query("UPDATE todo_completions SET time = time - 400")
        .execute(&pool)
        .await
        .unwrap();
    let pending = run_view(&pool, &config, &["@"]).await;
    assert!(!pending.contains("just finished"), "@ All: {pending:?}");
    let done = run_view(&pool, &config, &["@done"]).await;
    assert!(done.contains("just finished"), "@done All: {done:?}");
}

/// D9 applies to every pending variant, kind-scoped: `@:o` keeps a
/// just-completed oneshot (and only oneshots) within the persist window;
/// `@:O` keeps a just-completed recurring task (and only sched/recur).
#[tokio::test]
async fn test_persist_pending_variant_scoping() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    let oneshot = create_oneshot_task(&pool, "just finished oneshot").await;
    let cmd = parse_from(vec!["-".to_string(), oneshot.to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    // A completed recurring task, also just completed.
    let interval = 86_400i64;
    let now = feeling::date::now();
    let recurring = insert_recurring_task(
        &pool,
        "just finished recurring",
        now - 3600,
        interval,
        Some(7200),
        1,
        None,
    )
    .await;
    feeling::sql::update_task(&pool, recurring, 1)
        .await
        .unwrap();

    // @:o holds the oneshot (D9, oneshot scope) but not the recurring.
    let a = run_view(&pool, &config, &["@:o"]).await;
    assert!(a.contains("just finished oneshot"), "@:o: {a:?}");
    assert!(!a.contains("just finished recurring"), "@:o: {a:?}");

    // @:O holds the recurring (D9, sched/recur scope) but not the oneshot.
    let b = run_view(&pool, &config, &["@:O"]).await;
    assert!(b.contains("just finished recurring"), "@:O: {b:?}");
    assert!(!b.contains("just finished oneshot"), "@:O: {b:?}");

    // Once the persist window passes, both disappear from their variant
    // (done + outside the window).
    sqlx::query("UPDATE todo_completions SET time = time - 400")
        .execute(&pool)
        .await
        .unwrap();
    let a = run_view(&pool, &config, &["@:o"]).await;
    assert!(!a.contains("just finished oneshot"), "@:o: {a:?}");
    let b = run_view(&pool, &config, &["@:O"]).await;
    assert!(!b.contains("just finished recurring"), "@:O: {b:?}");
}

/// `@:O` scheduled rows are non-done `S` with `window_open`: ongoing and
/// failed-with-open-window show; failed with a closed window and
/// auto-completed (no entry, window elapsed) belong to `@done` only.
#[tokio::test]
async fn test_pending_b_window_open_scheduled() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();
    let now = feeling::date::now();

    // Ongoing: window open, no entry.
    let ongoing = insert_scheduled(&pool, "ongoing task", now - 7200, 3 * 3600, None).await;
    // Failed with window still open.
    let failed_open = insert_scheduled(&pool, "failed open task", now - 7200, 3 * 3600, None).await;
    // Failed with a closed window.
    let failed_closed = insert_scheduled(&pool, "failed closed task", now - 7200, 3600, None).await;
    // Auto-completed: window elapsed, no entry.
    let auto = insert_scheduled(&pool, "auto completed task", now - 7200, 3600, None).await;

    // Entries outside the persist window so only the window logic decides.
    let t = now - 600;
    for (id, count) in [(failed_open, 0), (failed_closed, 0), (auto, 1)] {
        update_task(&pool, id, t, count).await;
    }

    let b = run_view(&pool, &config, &["@:O"]).await;
    assert!(b.contains("ongoing task"), "@:O: {b:?}");
    assert!(b.contains("failed open task"), "@:O: {b:?}");
    assert!(!b.contains("failed closed task"), "@:O: {b:?}");
    assert!(!b.contains("auto completed task"), "@:O: {b:?}");

    // The failed-with-closed-window task lives in @done instead.
    let done = run_view(&pool, &config, &["@done"]).await;
    assert!(done.contains("failed closed task"), "@done: {done:?}");
    let _ = ongoing;
}

/// The today view's recurring fetch is interval-aware: a task started long
/// ago still shows when its current-interval availability window overlaps
/// the anchored day, and a task whose windows skip the day does not.
#[tokio::test]
async fn test_today_view_interval_aware_recurring_overlap() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    let embedder = feeling::embed::global_embedder();
    config.moods.init_with(&pool, embedder).await.unwrap();

    let today_start = feeling::date::today_start();

    // Started 60 days ago at 06:00, daily, window 06:00-07:00 each day:
    // active today, even though start_time + duration is far in the past
    // (the raw-overlap formula would drop it).
    let _active = insert_recurring_task(
        &pool,
        "old but active today",
        today_start - 60 * 86_400 + 6 * 3600,
        86_400,
        Some(3600),
        0,
        None,
    )
    .await;

    // Started yesterday at 06:00 on a 2-day interval: windows are
    // yesterday 06:00 and tomorrow 06:00 — none overlap today.
    let skipping = insert_recurring_task(
        &pool,
        "no window today",
        today_start - 86_400 + 6 * 3600,
        2 * 86_400,
        Some(3600),
        0,
        None,
    )
    .await;

    let mut color_cache = std::collections::HashMap::new();
    let entries = feeling::views::fetch_today_entries(
        &pool,
        &config,
        feeling::views::TodayHorizon::Today,
        None,
        feeling::clap::ShowVariant::All,
        &mut color_cache,
    )
    .await
    .unwrap();
    let active_row = entries
        .iter()
        .find(|e| e.label == "old but active today")
        .expect("interval-aware overlap must surface the old recurring task");
    // The time cell follows the now-anchored availability rule: the window
    // end while still open (now < interval_start + dur), else the start of
    // the next interval — computed here, never hardcoded, because the
    // 06:00-07:00 window's phase (open / closed / deferred) depends on the
    // run time.
    let now = feeling::date::now();
    let st = today_start - 60 * 86_400 + 6 * 3600;
    let interval_start = if now <= st {
        st
    } else {
        st + ((now - st).div_euclid(86_400)) * 86_400
    };
    let window_end = interval_start + 3600;
    let expected_time = if now < window_end {
        window_end
    } else {
        interval_start + 86_400
    };
    let expected_label = if feeling::date::day_start(expected_time) == today_start {
        feeling::date::format_time(expected_time)
    } else {
        format!(
            "{} {}",
            feeling::date::format_weekday(expected_time),
            feeling::date::format_time(expected_time)
        )
    };
    assert_eq!(active_row.time_label, expected_label);
    assert!(
        !entries.iter().any(|e| e.label == "no window today"),
        "task with no window overlapping today must not show"
    );
    let _ = skipping;
}

/// Today-view All time label for complete tasks = completion time
/// (generalizes the scheduled time label): a scheduled task completed in
/// its window shows the completion time, an auto-completed one shows
/// start + duration; the B variant (@due) filters completed tasks out.
#[tokio::test]
async fn test_today_view_done_time_label_and_b_filter() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    let embedder = feeling::embed::global_embedder();
    config.moods.init_with(&pool, embedder).await.unwrap();

    let yesterday_start = feeling::date::today_start() - 86_400;

    // Scheduled window [10:00, 16:00) on the anchored day, completed at
    // 14:30 on that day.
    let completed = insert_scheduled(
        &pool,
        "completed scheduled",
        yesterday_start + 10 * 3600,
        6 * 3600,
        None,
    )
    .await;
    let done_at = yesterday_start + 14 * 3600 + 30 * 60;
    update_task(&pool, completed, done_at, 1).await;

    // Auto-completed: window [10:00, 12:00) on the anchored day, no entry.
    insert_scheduled(
        &pool,
        "auto completed scheduled",
        yesterday_start + 10 * 3600,
        2 * 3600,
        None,
    )
    .await;

    let mut color_cache = std::collections::HashMap::new();
    let entries = feeling::views::fetch_today_entries(
        &pool,
        &config,
        feeling::views::TodayHorizon::Today,
        Some(yesterday_start),
        feeling::clap::ShowVariant::All,
        &mut color_cache,
    )
    .await
    .unwrap();
    let row = |name: &str| {
        entries
            .iter()
            .find(|e| e.kind.is_task() && e.label == name)
            .expect("task row must appear in the today view")
    };
    assert_eq!(row("completed scheduled").time_label, "14:30");
    assert_eq!(row("auto completed scheduled").time_label, "12:00");

    // @due (B) is the same as All but tasks-only (no trackers/mood): a
    // task completed a minute ago in a window that is still open today
    // stays, with its completion-time label. The yesterday-anchored tasks
    // don't overlap today, so they don't show.
    let now = feeling::date::now();
    let completed_today =
        insert_scheduled(&pool, "completed today", now - 2 * 3600, 4 * 3600, None).await;
    update_task(&pool, completed_today, now - 60, 1).await;
    let due = run_view(&pool, &config, &["@due"]).await;
    assert!(
        due.contains("completed today"),
        "@due (B) shows completed tasks like All: {due:?}"
    );
    let expected = feeling::date::format_time(now - 60);
    let line = due
        .lines()
        .find(|l| l.contains("completed today"))
        .expect("completed today row");
    let fields: Vec<&str> = line.split('\t').collect();
    assert_eq!(fields[0], expected, "completion-time label: {line:?}");
    assert!(!due.contains("completed scheduled"), "@due: {due:?}");
    assert!(!due.contains("auto completed scheduled"), "@due: {due:?}");
}

/// Insert a scheduled task row directly and return its id.
async fn insert_scheduled(
    pool: &SqlitePool,
    name: &str,
    start_time: i64,
    duration: i64,
    end_time: Option<i64>,
) -> i64 {
    sqlx::query(
        "INSERT INTO todos (name, body, priority, interval_secs, available_duration_secs, target_count, optional, start_time, end_time) \
         VALUES (?, '', 5, NULL, ?, 0, 0, ?, ?)",
    )
    .bind(name)
    .bind(duration)
    .bind(start_time)
    .bind(end_time)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query_scalar::<_, i64>("SELECT id FROM todos WHERE name = ?")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    // : (mood tracker) should print a header and dot rows
    let cmd = parse_from(vec![":".to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    // Titles are verbose-only now: default output has no header, just the
    // dot rows; -v adds the bare title, -vv the ' (Week)' suffix.
    assert!(!output.contains("Mood tracker"), "output: {output:?}");
    assert!(output.contains('●'), "expected a filled dot: {output:?}");

    let verbose_cmd = parse_from(vec![":".to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(
        verbose_cmd,
        &pool,
        &config,
        &CliOpts { qv: [0, 1] },
        &mut out,
        false,
    )
    .await
    .unwrap();
    assert!(output.contains('●'), "expected a filled dot: {output:?}");
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("Moods"), "output: {output:?}");
    assert!(!output.contains("Moods (Week)"), "output: {output:?}");

    let vv_cmd = parse_from(vec![":".to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(
        vv_cmd,
        &pool,
        &config,
        &CliOpts { qv: [0, 2] },
        &mut out,
        false,
    )
    .await
    .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("Moods (Week)"), "output: {output:?}");
}

#[tokio::test]
async fn test_tracker_custom_dots() {
    use feeling::config::{TrackerKind, TrackerSetting};

    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "sleep".to_string(),
        TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerKind::Float,
            colors: None,
        },
    );

    // Custom entry via CLI
    let cmd = parse_from(vec!["-sleep".to_string(), "8".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    // : sleep should show a filled dot
    let cmd = parse_from(vec![":".to_string(), "sleep".to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    // Titles are verbose-only: no "Tracker 'sleep'" header by default;
    // -vv shows the bare label with the period suffix.
    assert!(!output.contains("Tracker 'sleep'"), "output: {output:?}");
    assert!(output.contains('●'), "expected a filled dot: {output:?}");

    let vv_cmd = parse_from(vec![":".to_string(), "sleep".to_string()]).unwrap();
    let mut out = Vec::new();
    handle_command(
        vv_cmd,
        &pool,
        &config,
        &CliOpts { qv: [0, 2] },
        &mut out,
        false,
    )
    .await
    .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("sleep (Week)"), "output: {output:?}");
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
    // Titles are verbose-only; -vv shows the bare @name with ' (Week)'.
    assert!(!output.contains("Task 'exercise'"), "output: {output:?}");
    assert!(output.contains('●'), "expected a filled dot: {output:?}");

    let vv_cmd = parse_from(vec![":".to_string(), format!("@{name}")]).unwrap();
    let mut out = Vec::new();
    handle_command(
        vv_cmd,
        &pool,
        &config,
        &CliOpts { qv: [0, 2] },
        &mut out,
        false,
    )
    .await
    .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("@exercise (Week)"), "output: {output:?}");
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
    // Titles are verbose-only: no "Task '…' (Year)" header by default.
    assert!(
        !output.contains(&format!("Task '{name}' (Year)")),
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

    // -vv shows the @name with the ' (Year)' suffix.
    let vv_cmd = parse_from(vec![":year".to_string(), format!("@{name}")]).unwrap();
    let mut out = Vec::new();
    handle_command(
        vv_cmd,
        &pool,
        &config,
        &CliOpts { qv: [0, 2] },
        &mut out,
        false,
    )
    .await
    .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(
        output.contains(&format!("@{name} (Year)")),
        "output: {output:?}"
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
    update_task(&pool, task_id, interval_start - 100, 2).await;
    update_task(&pool, task_id, interval_start + 100, 3).await;

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
    update_task(&pool, task_id, start_time + 100, 1).await;
    update_task(&pool, task_id, start_time + 200, 1).await;

    let config = Config::default();

    // @ (CLI) must still show the task: it is not done in the current
    // interval, so it renders with the recurring ↻ badge.
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
            assert_eq!(fields[5], "↻", "recurring badge expected: {line:?}");
        }
    }

    // Completing it in the current interval: D9 keeps it visible in @ within
    // persist_pending_seconds (done ✓ badge); once the completion is outside
    // the persist window it disappears from the CLI @ view.
    feeling::sql::update_task(&pool, task_id, 2).await.unwrap();

    let mut out = Vec::new();
    let cmd = parse_from(vec!["@".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(
        output.contains(name),
        "@ persist window keeps a just-completed task: {output:?}"
    );
    for line in output.lines() {
        if line.contains(name) {
            let fields: Vec<&str> = line.split('\t').collect();
            assert!(
                fields[5].contains('✓'),
                "done badge in pending view: {line:?}"
            );
        }
    }

    // Backdate the completions past the persist window: @ hides it again
    // (done in the current interval, no longer recently completed).
    sqlx::query("UPDATE todo_completions SET time = time - 400")
        .execute(&pool)
        .await
        .unwrap();
    let mut out = Vec::new();
    let cmd = parse_from(vec!["@".to_string()]).unwrap();
    handle_command(cmd, &pool, &config, &CliOpts::default(), &mut out, false)
        .await
        .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(
        !output.contains(name),
        "@ must hide a task done in the current interval once the persist window passes: {output:?}"
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
            kind: TrackerKind::Text,
            colors: None,
        },
    );

    // feeling -accomplishment "fixed 2 bugs" via the CLI path
    let cmd = parse_from(vec![
        "-accomplishment".to_string(),
        "fixed 2 bugs".to_string(),
        "good".to_string(),
    ])
    .unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    // Stored as text with the string payload
    let row = sqlx::query("SELECT score, typeof(score) AS t FROM custom")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("score"), "fixed 2 bugs");
    assert_eq!(row.get::<String, _>("t"), "text");

    // Today view: text entries use the ◆ badge with the text as label
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
        output.contains('\t') && output.contains('◆'),
        "text custom entries must use the ◆ badge: {output:?}"
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
            kind: TrackerKind::Text,
            colors: None,
        },
    );

    for text in ["fixed 2 bugs", "shipped the feature", "wrote docs"] {
        let cmd = parse_from(vec!["-accomplishment".to_string(), text.to_string()]).unwrap();
        handle_command(
            cmd,
            &pool,
            &config,
            &CliOpts::default(),
            &mut Vec::new(),
            false,
        )
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
            kind: TrackerKind::Float,
            colors: None,
        },
    );
    let cmd = parse_from(vec!["-sleep".to_string(), "good".to_string()]).unwrap();
    let result = handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await;
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
            kind: TrackerKind::Number,
            colors: None,
        },
    );
    let cmd = parse_from(vec!["-bugs".to_string(), "3.5".to_string()]).unwrap();
    let result = handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await;
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
            kind: TrackerKind::Number,
            colors: None,
        },
    );

    let cmd = parse_from(vec!["-bugs".to_string(), "3".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        parse_from(vec![]).unwrap(),
        &pool,
        &config,
        &CliOpts::default(),
        &mut out,
        false,
    )
    .await
    .unwrap();
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains(name), "output: {output:?}");
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
        journal_badge = '•'
        "#,
    )
    .unwrap();
    assert!(config.today_view.include_overdue);
    assert_eq!(config.today_view.journal_badge, Some('•'));
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
    assert_eq!(default.tasks_view.persist_pending_seconds, 5 * 60);
    assert!(!default.today_view.coalesce_completions);
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    let output = run_tracker(&pool, &config, ":").await;

    // week_rolling=true always renders the full week: exactly 7 dots.
    // Titles are verbose-only — no header by default.
    assert!(!output.contains("Mood tracker"), "output: {output:?}");
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
async fn test_tracker_grid_uses_colors_override() {
    // A tracker with its own palette must bin with that palette in the grid
    // view (both the interval and per-entry paths), not config.tasks.colors.
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    use crossterm::style::Color as CtColor;
    use feeling::config::ColorBins;
    let override_palette: ColorBins = vec![CtColor::Red, CtColor::White, CtColor::Blue].into();
    config.tracker.insert(
        "run".to_string(),
        feeling::config::TrackerSetting {
            interval: Some(86_400),
            min: Some(0.0),
            max: Some(10.0),
            kind: TrackerKind::Number,
            colors: Some(override_palette.clone()),
        },
    );
    config.tracker.insert(
        "feel".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: Some(0.0),
            max: Some(10.0),
            kind: TrackerKind::Number,
            colors: Some(override_palette),
        },
    );

    // Max score → last palette color (Blue). The default palette's last color
    // is DarkGreen, so a Blue dot proves the override was used.
    let cmd = parse_from(vec!["-run".to_string(), "10".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();
    let cmd = parse_from(vec!["-feel".to_string(), "10".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_mood_tracker_grid_week_default_non_rolling() {
    let pool = test_pool().await.unwrap();
    // Defaults: week_rolling=false, week_start=Monday.
    let config = Config::default();
    let cmd = parse_from(vec!["good".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    let output = run_tracker(&pool, &config, ":").await;

    // Non-rolling week = week_start (Monday) through today, so the dot count
    // depends on today's weekday — computed here, never hardcoded.
    use chrono::Datelike;
    let expected = chrono::Local::now().weekday().num_days_from_monday() as i64 + 1;
    assert!(!output.contains("Mood tracker"), "output: {output:?}");
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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

    assert!(!output.contains("Mood tracker"), "output: {output:?}");
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    let output = run_tracker(&pool, &config, ":year").await;

    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();

    assert!(!output.contains("Mood tracker"), "output: {output:?}");
    // Exactly 7 grid rows (one per weekday, Monday first) — no header line.
    assert_eq!(output.lines().count(), 7, "output: {output:?}");
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    let output = run_tracker(&pool, &config, ":year").await;

    use chrono::{Datelike, Weekday};
    let today = chrono::Local::now().date_naive();
    let jan1 = today.with_ordinal(1).unwrap();

    assert!(!output.contains("Mood tracker"), "output: {output:?}");
    // Exactly 7 grid rows (one per weekday, Monday first) — no header line.
    assert_eq!(output.lines().count(), 7, "output: {output:?}");
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
        handle_command(
            cmd,
            &pool,
            &config,
            &CliOpts::default(),
            &mut Vec::new(),
            false,
        )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
        handle_command(
            cmd,
            &pool,
            &config,
            &CliOpts::default(),
            &mut Vec::new(),
            false,
        )
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
        handle_command(
            cmd,
            &pool,
            &config,
            &CliOpts::default(),
            &mut Vec::new(),
            false,
        )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    // Complete it: the short id is cleared.
    let cmd = parse_from(vec!["-".to_string(), "1".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();
    let cmd = parse_from(vec!["-".to_string(), "1".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();
    let cmd = parse_from(vec!["-".to_string(), "1".to_string(), "3".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    let cache_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM embedding_cache")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        cache_after, 0,
        "prune should clear the whole embedding cache"
    );
}

// ---- Invalid timestamps must fail task creation ----

/// Garbage `@<time>` values (and invalid calendar dates) must fail task
/// creation — oneshot and scheduled — rather than silently landing in the
/// task name.
#[tokio::test]
async fn test_task_creation_invalid_timestamps_fail() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Oneshot with a garbage date: `! task @x`.
    let cmd = parse_from(vec!["!".to_string(), "task".to_string(), "@x".to_string()]).unwrap();
    let err = handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("Failed to parse datetime"),
        "unexpected error: {err:#}"
    );

    // Invalid calendar date: `! task @2024-99-99`.
    let cmd = parse_from(vec![
        "!".to_string(),
        "task".to_string(),
        "@2024-99-99".to_string(),
    ])
    .unwrap();
    let err = handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("Failed to parse datetime"),
        "unexpected error: {err:#}"
    );

    // Scheduled with a garbage start: `! @x`.
    let cmd = parse_from(vec!["!".to_string(), "@x".to_string()]).unwrap();
    let err = handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("Failed to parse datetime"),
        "unexpected error: {err:#}"
    );

    // Nothing was created by any of the failures.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM todos")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

// ---- FK cascade on delete (CASCADE propagation to todo_completions) ----

/// Deleting a task must remove its `todo_completions` rows via the
/// `ON DELETE CASCADE` declared in db.rs.
#[tokio::test]
async fn test_delete_task_cascades_completions() {
    let pool = test_pool().await.unwrap();
    // let config = Config::default();

    let id = create_oneshot_task(&pool, "to cull").await;
    update_task(&pool, id, feeling::date::now(), 1).await;
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
            kind: TrackerKind::Float,
            colors: None,
        },
    );

    // Insert a feeling with a linked custom row (like `feeling ok -sleep 8`).
    let cmd = parse_from(vec![
        "ok".to_string(),
        "-sleep".to_string(),
        "8".to_string(),
    ])
    .unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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

/// `sql::delete_custom` (the today view's custom-entry delete path) removes
/// exactly the targeted row.
#[tokio::test]
async fn test_delete_custom_row() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config.tracker.insert(
        "sleep".to_string(),
        feeling::config::TrackerSetting {
            interval: None,
            min: None,
            max: None,
            kind: TrackerKind::Float,
            colors: None,
        },
    );

    // Two custom entries, one linked to a feeling.
    let cmd = parse_from(vec![
        "ok".to_string(),
        "-sleep".to_string(),
        "8".to_string(),
    ])
    .unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();
    let cmd = parse_from(vec!["-sleep".to_string(), "7".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM custom ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids.len(), 2);

    // Delete the unlinked row; the linked one (and its feeling) survive.
    let affected = feeling::sql::delete_custom(&pool, ids[1]).await.unwrap();
    assert_eq!(affected, 1);
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM custom")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 1);
    let feelings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feeling")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(feelings, 1);
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
            kind: TrackerKind::Float,
            colors: None,
        },
    );

    let cmd = parse_from(vec![
        "ok".to_string(),
        "-sleep".to_string(),
        "8".to_string(),
    ])
    .unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
            kind: TrackerKind::Text,
            colors: None,
        },
    );

    let cmd = parse_from(vec!["-note".to_string(), "hello".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
            kind: TrackerKind::Float,
            colors: None,
        },
    );

    let cmd = parse_from(vec!["-sleep".to_string(), "8".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
    update_task(&pool, task_id, feeling::date::now(), 1).await;

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
    update_task(&pool, task_id, interval_start - 100, 2).await;
    update_task(&pool, task_id, interval_start + 100, 3).await;

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
            kind: TrackerKind::Float,
            colors: None,
        },
    );

    let cmd = parse_from(vec!["-sleep".to_string(), "8".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    let embedder = feeling::embed::global_embedder();
    config.moods.init_with(&pool, embedder).await.unwrap();

    let mut color_cache = std::collections::HashMap::new();
    let entries = feeling::views::fetch_today_entries(
        &pool,
        &config,
        feeling::views::TodayHorizon::Today,
        None,
        feeling::clap::ShowVariant::All,
        &mut color_cache,
    )
    .await
    .unwrap();
    let custom = entries
        .iter()
        .find(|e| e.kind == feeling::views::EntryKind::Custom)
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
async fn test_fetch_today_entries_completed_task_has_check_badge() {
    // The today view renders one row per task; a completed task's row
    // carries the ✓ badge — completion rows are no longer emitted (WP9 9e).
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
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();
    let task_id: i64 = sqlx::query_scalar("SELECT id FROM todos WHERE name = 'completed task'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let update_cmd = parse_from(vec!["-".to_string(), task_id.to_string()]).unwrap();
    handle_command(
        update_cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
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
        None,
        feeling::clap::ShowVariant::All,
        &mut color_cache,
    )
    .await
    .unwrap();
    // (Legacy: the today view used to emit separate completion rows; that
    // behavior is gone and cannot be expressed via EntryKind — the enum has
    // no completion variant.)
    let task_rows: Vec<_> = entries
        .iter()
        .filter(|e| e.kind.is_task() && e.label == "completed task")
        .collect();
    assert_eq!(task_rows.len(), 1, "exactly one task row expected");
    assert_eq!(task_rows[0].badge, Some('✓'), "done task row must carry ✓");
    assert_eq!(task_rows[0].task_id, Some(task_id));
}

/// `[today_view] journal_badge` controls the journal-only entry badge; None
/// renders no badge at all.
#[tokio::test]
async fn test_today_view_journal_badge() {
    let pool = test_pool().await.unwrap();
    let mut config = Config::default();
    config
        .moods
        .init_with(&pool, feeling::embed::global_embedder())
        .await
        .unwrap();

    // Journal-only entry: mood '' with a body (via CLI: `feeling .. text`).
    let cmd = parse_from(vec!["..".to_string(), "a journal note".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    // Default (no journal_badge): no badge at all.
    let mut out = Vec::new();
    feeling::views::handle_today(
        &pool,
        &config,
        None,
        feeling::clap::ShowVariant::All,
        feeling::views::TodayHorizon::Today,
        &CliOpts::default(),
        &mut out,
    )
    .await
    .unwrap();
    let output = String::from_utf8(out).unwrap();
    let line = output
        .lines()
        .find(|l| l.contains("a journal note"))
        .unwrap();
    let cols: Vec<&str> = line.split('\t').collect();
    assert_eq!(
        cols[1], "",
        "journal badge must be empty by default: {line:?}"
    );

    // With a configured badge, the journal entry carries it.
    config.today_view.journal_badge = Some('•');
    let mut out = Vec::new();
    feeling::views::handle_today(
        &pool,
        &config,
        None,
        feeling::clap::ShowVariant::All,
        feeling::views::TodayHorizon::Today,
        &CliOpts::default(),
        &mut out,
    )
    .await
    .unwrap();
    let output = String::from_utf8(out).unwrap();
    let line = output
        .lines()
        .find(|l| l.contains("a journal note"))
        .unwrap();
    let cols: Vec<&str> = line.split('\t').collect();
    assert!(
        cols[1].contains('•'),
        "journal badge must come from config: {line:?}"
    );
}

#[tokio::test]
async fn test_clear_command() {
    let pool = test_pool().await.unwrap();
    let config = Config::default();

    // Create a feeling entry for today
    let cmd = parse_from(vec!["feeling".to_string(), "good".to_string()]).unwrap();
    handle_command(
        cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feeling")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    // Clear entries for today (non-interactive mode in tests)
    let clear_cmd = parse_from(vec![":clear".to_string()]).unwrap();
    handle_command(
        clear_cmd,
        &pool,
        &config,
        &CliOpts::default(),
        &mut Vec::new(),
        false,
    )
    .await
    .unwrap();

    let count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feeling")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count_after, 0);
}
