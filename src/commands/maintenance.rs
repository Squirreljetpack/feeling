use anyhow::{Context, Result};
use cba::{ebog, ibog};
use sqlx::SqlitePool;

use crate::config::{Config, DEFAULT_CONFIG, DEFAULT_MOODS};
use crate::date;
use crate::editor::open_editor_at;
use crate::paths::default_config_path;

/// `feeling :clear [@date]` — clear/delete all mood entries from that day.
/// If interactive, confirm first, showing the computed date.
pub(super) async fn clear_moods(
    pool: &SqlitePool,
    _config: &Config,
    date_param: Option<String>,
    tui: bool,
) -> Result<()> {
    let target_ts = match date_param {
        Some(ref d_str) => crate::date::parse_datetime(d_str, crate::date::DATE_DIALECT)?,
        None => crate::date::now(),
    };

    let start = crate::date::day_start(target_ts);
    let end = crate::date::day_end(target_ts);
    let formatted_date = crate::date::format_date(start);

    // Count how many feeling entries exist for this day
    let count = crate::db::clear_moods(pool, start, end, false).await?;

    if count == 0 {
        ebog!("No mood entries found for {formatted_date}");
        return Ok(());
    }

    let interactive = tui;
    if interactive {
        let confirmed = crate::prompts::prompt_clear_confirm(count as i64, &formatted_date)?;

        if !confirmed {
            cliclack::outro("Cancelled.")?;
            return Ok(());
        }
    }

    let deleted_count = crate::db::clear_moods(pool, start, end, true).await?;

    if interactive {
        cliclack::outro(format!(
            "Cleared {deleted_count} mood entry/entries for {formatted_date}"
        ))?;
    } else {
        ibog!("Cleared {deleted_count} mood entry/entries for {formatted_date}")
    }

    Ok(())
}

/// `feeling :prune` — deletes completed oneshot tasks (their `short_id` was
/// cleared on completion, so they are no longer addressable) and recurring
/// tasks whose `end_time` has passed.
///
/// Both categories are collected in a single SQL `RETURNING` statement so
/// the per-row log lines below happen against the rows actually deleted,
/// not a SELECT-then-DELETE that could log a row that races with another
/// writer. Foreign-key cascades (see `db.rs`: `todo_completions.todo_id`
/// has `ON DELETE CASCADE`) drop the matching completion rows
/// automatically.
pub(super) async fn prune_tasks(pool: &SqlitePool, _config: &Config) -> Result<()> {
    let now = date::now();
    let pruned = crate::db::prune_tasks(pool, now).await?;

    for task in &pruned {
        match task.short_id {
            Some(short_id) => cba::ibog!(
                "prune";
                "deleted {} task #{} '{}'",
                task.reason,
                short_id,
                task.name
            ),
            None => cba::ibog!("prune"; "deleted {} task '{}'", task.reason, task.name),
        }
    }

    let pruned_cache_count = crate::db::prune_embedding_cache(pool).await?;
    if pruned_cache_count > 0 {
        cba::ibog!("prune"; "pruned {} stale cached embedding(s)", pruned_cache_count);
    }

    if pruned.is_empty() {
        cba::ibog!("prune"; "nothing to prune");
    } else {
        cba::ibog!("prune"; "pruned {} task(s)", pruned.len());
    }

    Ok(())
}

/// `feeling :config` — open the active config in $VISUAL/$EDITOR.
///
/// If the on-disk config doesn't exist yet (common on first run with the
/// release profile), copy the bundled `assets/config.toml` straight to the
/// destination path verbatim — no TOML round-trip through `Config::default` —
/// so the user sees the exact source-of-truth defaults and we save a
/// serialization step. The copy is announced with `ibog!` so a non-interactive
/// invocation (e.g. a piped run, the editor still launches anyway) leaves a
/// legible trail in the log.
pub(super) async fn edit_config() -> Result<()> {
    let path = default_config_path();
    let mut created = false;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config dir {:?}", parent))?;
        }
        std::fs::write(path, DEFAULT_CONFIG.as_bytes())
            .with_context(|| format!("Failed to write default config to {:?}", path))?;
        created = true;
    }
    if created {
        cba::ibog!(
            "config";
            "Created file at {} from bundled defaults; edit and save to apply.",
            path.display()
        );
    }
    open_editor_at(path)
}

/// `feeling :moods` — open the moods file (`[moods] source`, relative to
/// the config directory) in $VISUAL/$EDITOR.
///
/// Like [`handle_config`], a missing file is created from the bundled moods
/// defaults first, announced with `ibog!`. When `[moods] source` is empty
/// (the default) there is no moods file to open: warn that `source` must be
/// set in the config, and do nothing else.
pub(super) async fn edit_moods(config: &Config) -> Result<()> {
    if config.moods.source.as_os_str().is_empty() {
        cba::wbog!(
            "feeling :moods needs a moods file, but [moods] source is unset: add \
             source = \"moods.toml\" to the [moods] section of your config"
        );
        return Ok(());
    }
    let path = crate::paths::config_dir().join(&config.moods.source);
    let mut created = false;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config dir {:?}", parent))?;
        }
        std::fs::write(&path, DEFAULT_MOODS.as_bytes())
            .with_context(|| format!("Failed to write default moods file to {:?}", path))?;
        created = true;
    }
    if created {
        cba::ibog!(
            "moods";
            "Created file at {} from bundled defaults; edit and save to apply.",
            path.display()
        );
    }
    open_editor_at(&path)
}

/// `feeling -` (bare) — tasks-edit entry point. Stub for now: interactive
/// task editing is future work (see TODO.md).
pub(super) async fn edit_tasks() -> Result<()> {
    anyhow::bail!("Task editing is not yet implemented");
}
