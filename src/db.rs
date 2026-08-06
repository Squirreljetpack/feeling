use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::path::Path;

pub async fn init_database(db_path: &Path) -> anyhow::Result<SqlitePool> {
    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .after_connect(|conn, _| {
            Box::pin(async move {
                // WAL leaves -wal/-shm sidecar files; only enable it in
                // release builds so dev runs don't litter the state dir.
                // (Setting DELETE explicitly in debug also converts a
                // pre-existing WAL-mode db file, so the mode is deterministic.)
                #[cfg(debug_assertions)]
                sqlx::query("PRAGMA journal_mode = DELETE;")
                    .execute(&mut *conn)
                    .await?;
                #[cfg(not(debug_assertions))]
                sqlx::query("PRAGMA journal_mode = WAL;")
                    .execute(&mut *conn)
                    .await?;
                sqlx::query("PRAGMA synchronous = NORMAL;")
                    .execute(&mut *conn)
                    .await?;
                sqlx::query("PRAGMA foreign_keys = ON;")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&db_url)
        .await?;

    run_migrations(&pool).await?;

    log::debug!("Database initialized at {:?}", db_path);
    Ok(pool)
}

/// Create an in-memory SQLite pool for testing.
pub async fn test_pool() -> anyhow::Result<SqlitePool> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("PRAGMA foreign_keys = ON;")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect("sqlite::memory:")
        .await?;

    run_migrations(&pool).await?;

    Ok(pool)
}

pub async fn run_migrations(pool: &SqlitePool) -> anyhow::Result<()> {
    // Create tables if they don't exist
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS feeling (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            mood TEXT NOT NULL,
            body TEXT NOT NULL DEFAULT '',
            time INTEGER NOT NULL DEFAULT (unixepoch()),
            embedding BLOB,
            -- Cached emotional-saliency score for the mood text (nullable;
            -- backfilled by mood_color_cached). No migration: an existing
            -- DB without the column is deleted by the user.
            score REAL
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS custom (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            type TEXT NOT NULL,
            -- BLOB decltype = no type affinity: storage class is preserved
            -- exactly (integer/text/real) so sqlx can decode by value type.
            score BLOB NOT NULL CHECK (typeof(score) IN ('integer', 'text', 'real')),
            time INTEGER NOT NULL DEFAULT (unixepoch()),
            feeling INTEGER,
            FOREIGN KEY (feeling) REFERENCES feeling(id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS todos (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            body TEXT NOT NULL DEFAULT '',
            priority INTEGER NOT NULL DEFAULT 5,
            -- User-facing short id: allocated by the db layer (first free
            -- gap); NULL once the task is completed (oneshot) or for
            -- recurring tasks done in the current interval.
            short_id INTEGER UNIQUE,
            -- Reserved for a name-derived embedding; never populated.
            name_embedding BLOB,
            start_time INTEGER,
            available_duration_secs INTEGER,
            interval_secs INTEGER,
            target_count INTEGER NOT NULL DEFAULT 0,
            optional INTEGER NOT NULL DEFAULT 0,
            end_time INTEGER
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS todo_completions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            todo_id INTEGER NOT NULL,
            time INTEGER NOT NULL DEFAULT (unixepoch()),
            count INTEGER NOT NULL DEFAULT 1,
            FOREIGN KEY (todo_id) REFERENCES todos(id) ON DELETE CASCADE
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS embedding_cache (
            text TEXT PRIMARY KEY,
            embedding BLOB NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Add indexes for common queries
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_feeling_time ON feeling(time)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_custom_time ON custom(time)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_custom_feeling ON custom(feeling)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_custom_type ON custom(type)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_todos_short_id ON todos(short_id)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_todos_interval ON todos(interval_secs)")
        .execute(pool)
        .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_todos_start_time ON todos(start_time)")
        .execute(pool)
        .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_todo_completions_todo_id ON todo_completions(todo_id)",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_todo_completions_todo_time ON todo_completions(todo_id, time)")
        .execute(pool)
        .await?;

    log::debug!("Database migrations completed");
    Ok(())
}
