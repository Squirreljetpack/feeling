use anyhow::{Context, Result};
use cba::{_dbg, ebog, ibog};
use crossterm::style::Stylize;
use sqlx::SqlitePool;
use std::io::{BufRead, Write};

use crate::clap::{CliOpts, Command, TaskType, UpdateTarget};
use crate::config::{Config, TrackerKind, DEFAULT_CONFIG};
use crate::date::{self, format_duration};
use crate::editor::{open_editor_at, open_editor_for_body};
use crate::paths::default_config_path;
use crate::render::Render;
use crate::sql::{CustomObject, CustomValue, EntryObject, TaskObject, TaskUpdateInfo};
use crate::types::{Entry, Task};

pub async fn handle_command<W: Write>(
    cmd: Command,
    pool: &SqlitePool,
    config: &Config,
    opts: &CliOpts,
    out: &mut W,
    tui: bool,
) -> Result<()> {
    match cmd {
        Command::Entry(entry) => handle_entry(pool, config, opts, _dbg!(entry)).await,

        Command::View { mode, show } => {
            if tui {
                crate::render::tasks::TasksApp::new(
                    pool,
                    mode,
                    config.clone(),
                    show,
                    config.tasks_view.persist_pending_seconds,
                )
                .await
                .run()
                .await
            } else {
                crate::views::handle_view(pool, mode, config, show, out).await
            }
        }

        Command::Tracker { period, items } => {
            let mut config = config.clone();
            config
                .moods
                .init_with(pool, crate::embed::global_embedder())
                .await?;
            crate::views::handle_tracker(pool, &config, opts, period, items, out).await
        }

        Command::Task(task) => handle_task(pool, config, opts, _dbg!(task)).await,

        Command::Update { target, count } => handle_update(pool, opts, target, count).await,

        Command::Embed => {
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();
            handle_embed(&mut reader, out)
        }

        Command::Score { .. } => {
            todo!();
            // handle_score(&start, &end, &mut reader, out)
        }

        Command::Today {
            date,
            show,
            horizon,
        } => {
            let mut config = config.clone();
            config
                .moods
                .init_with(pool, crate::embed::global_embedder())
                .await?;
            // `feeling @<date>` anchors the view to that day; re-parse with
            // the configured dialect (the parse-time gate in clap.rs only
            // checks the default Uk dialect).
            let day_epoch = match &date {
                Some(d) => Some(crate::date::parse_date(d, config.date.dialect)?),
                None => None,
            };
            if tui {
                crate::render::today::TodayApp::new(pool, config, day_epoch, show, horizon)
                    .await
                    .run()
                    .await
            } else {
                crate::views::handle_today(pool, &config, day_epoch, show, horizon, opts, out).await
            }
        }

        Command::TasksEdit => handle_tasks_edit().await,

        Command::Help => {
            // assets/help.txt is bundled via `include_str!` so the compiled
            // binary always has the help text even when the working directory
            // does not contain the assets directory.
            const HELP: &str = include_str!("../assets/help.txt");
            out.write_all(HELP.as_bytes())?;
            Ok(())
        }

        Command::Config => handle_config().await,

        Command::Prune => handle_prune(pool, config).await,

        Command::Color { mood } => {
            let mut config = config.clone();
            config
                .moods
                .init_with(pool, crate::embed::global_embedder())
                .await?;
            handle_color(&mood, &config, opts, out)
        }

        Command::Clear { date } => handle_clear(pool, config, date, tui).await,
    }
}

/// `feeling :clear [@date]` — clear/delete all mood entries from that day.
/// If interactive, confirm first, showing the computed date.
async fn handle_clear(
    pool: &SqlitePool,
    config: &Config,
    date_param: Option<String>,
    tui: bool,
) -> Result<()> {
    let target_ts = match date_param {
        Some(ref d_str) => crate::date::parse_datetime(d_str, config.date.dialect)?,
        None => crate::date::now(),
    };

    let start = crate::date::day_start(target_ts);
    let end = crate::date::day_end(target_ts);
    let formatted_date = crate::date::format_date(start);

    // Count how many feeling entries exist for this day
    let count = crate::sql::clear_moods(pool, start, end, false).await?;

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

    let deleted_count = crate::sql::clear_moods(pool, start, end, true).await?;

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
async fn handle_prune(pool: &SqlitePool, _config: &Config) -> Result<()> {
    let now = date::now();
    let pruned = crate::sql::prune_tasks(pool, now).await?;

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

    let pruned_cache_count = crate::sql::prune_embedding_cache(pool).await?;
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
async fn handle_config() -> Result<()> {
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
            "Created config at {} from bundled defaults; edit and save to apply.",
            path.display()
        );
    }
    open_editor_at(path)
}

async fn handle_entry(
    pool: &SqlitePool,
    config: &Config,
    opts: &CliOpts,
    entry: Entry,
) -> Result<()> {
    let feeling = entry.feeling;
    let customs = entry.customs;
    let body = entry.body;
    let open_editor = entry.open_editor;

    // Body resolution: the parser places anything after `..` into `body`.
    // `open_editor` is true (set by the parser) iff `..` was used AND `body`
    // is empty — exactly the case where we want to open the editor. When
    // `body` is already supplied (post-.. text or no `..`), we use it as-is.
    let body = if open_editor {
        open_editor_for_body(config.editor.hint)?
    } else {
        body
    };

    if feeling.is_empty() && customs.is_empty() && body.is_empty() {
        anyhow::bail!("Nothing to log");
    }

    // Validate mood doesn't contain tabs (view output uses tab separators)
    if feeling.contains('\t') {
        anyhow::bail!("Mood cannot contain tab characters");
    }

    // Determine the timestamp (Unix epoch in seconds).
    let time_epoch = date::now();

    // Parse and validate custom tracker values against their declared kind.
    // Raw strings are interpreted here (not in the parser) so the config's
    // kind (text/number/float) determines how each value is stored. Text and
    // float trackers with an interval keep one entry per interval slot (see
    // `interval_slot`): re-logging the same tracker in the same slot replaces
    // the previous entry (handled inside `sql::create_entry`). Number
    // trackers always accumulate.
    let mut custom_objects: Vec<CustomObject> = Vec::with_capacity(customs.len());
    for (tracker_type, raw) in &customs {
        let value = parse_custom_value(config, tracker_type, raw)?;
        let replace_slot = config
            .tracker
            .get(tracker_type)
            .filter(|tracker| matches!(tracker.kind, TrackerKind::Text | TrackerKind::Float))
            .and_then(|tracker| tracker.interval)
            .map(|interval_secs| interval_slot(time_epoch, interval_secs));
        custom_objects.push(CustomObject {
            tracker_type: tracker_type.clone(),
            value,
            replace_slot,
        });
    }

    // Resolve the mood embedding and its saliency score before opening the
    // transaction. Journal-only entries (empty mood) never embed; the model
    // is bundled into the binary, so the embedder is always available — a
    // per-text embedding failure (e.g. an un-tokenizable string) stores no
    // embedding rather than losing the entry. The score is computed here so
    // color passes later skip the ONNX saliency prediction.
    let embedder = crate::embed::global_embedder();
    let (embedding_blob, score) = if feeling.is_empty() {
        (None, None)
    } else {
        match embedder.embed(&feeling, &config.moods.axes.prefix_string) {
            Ok(v) => (
                Some(crate::embed::embedding_to_blob(&v)),
                Some(crate::color::predict_saliency(embedder, &feeling)),
            ),
            Err(_) => (None, None),
        }
    };

    let entry_obj = EntryObject {
        mood: feeling,
        body,
        time: time_epoch,
        embedding: embedding_blob,
        score,
        customs: custom_objects,
    };

    let feeling_id = crate::sql::create_entry(pool, &entry_obj).await?;
    log::debug!("Inserted feeling with id={:?}", feeling_id);

    crate::display::display_entry(&entry_obj, opts)?;

    Ok(())
}

/// Interpret a raw CLI value for a tracker according to its configured kind.
/// Denies unknown tracker types; parses Number/Float values (with a clear
/// error when the argument cannot be parsed) and enforces min/max for both;
/// Text accepts the value as-is (min/max ignored).
fn parse_custom_value(config: &Config, tracker_type: &str, raw: &str) -> Result<CustomValue> {
    let tracker = config.tracker.get(tracker_type).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown custom tracker type '{}' not found in config",
            tracker_type
        )
    })?;

    match tracker.kind {
        TrackerKind::Text => Ok(CustomValue::Text(raw.to_string())),
        TrackerKind::Number => {
            let n: i64 = raw.parse().map_err(|_| {
                anyhow::anyhow!(
                    "Cannot parse '{}' as an integer for tracker '{}'",
                    raw,
                    tracker_type
                )
            })?;
            Ok(CustomValue::Number(n))
        }
        TrackerKind::Float => {
            let f: f64 = raw.parse().map_err(|_| {
                anyhow::anyhow!(
                    "Cannot parse '{}' as a number for tracker '{}'",
                    raw,
                    tracker_type
                )
            })?;
            Ok(CustomValue::Float(f))
        }
    }
}

/// The `[start, end)` replacement slot for an interval-tracker entry: a
/// uniform grid of the timeline, `[k*interval, (k+1)*interval)` aligned to
/// the Unix epoch. Uniform tiling keeps **any** interval working — including
/// sub-day ones like 30 minutes, where a calendar-day anchor would collapse
/// every same-day entry into a single slot.
///
/// KNOWN FLAW (roadmap: calendar-aware intervals): the grid's phase is UTC
/// midnight, so a "1 day" tracker's slots run local 20:00 → 20:00 on a
/// UTC-4 machine, and a "1 week" slot is exactly 604800s (167/169h across
/// a DST change). Replacing this pure-seconds grid with calendar-day/week
/// slots is the roadmap item; do not re-introduce a local-midnight anchor
/// here, it breaks sub-day intervals.
fn interval_slot(time_epoch: i64, interval_secs: i64) -> (i64, i64) {
    let slot_start = (time_epoch / interval_secs) * interval_secs;
    (slot_start, slot_start + interval_secs)
}

async fn handle_task(pool: &SqlitePool, config: &Config, opts: &CliOpts, task: Task) -> Result<()> {
    let task_type = task.task_type;
    let name = task.name;
    let body = task.body;
    let date = task.date;
    let open_editor = task.open_editor;
    let prefill = task.prefill;

    match task_type {
        TaskType::OneShot => {
            // Bare `!` prompts for the name (interactive creation); the
            // `open_editor` flag then runs the priority/target/body flow.
            let name_str = match &name {
                Some(n) => n.clone(),
                None => crate::prompts::prompt_name(None)?,
            };

            // Validate name doesn't contain tabs
            if name_str.contains('\t') {
                anyhow::bail!("Task name cannot contain tab characters");
            }

            // `@<time>` is the due time and lands in `end_time`; `start_time`
            // records the creation moment. Shared chrono-english parsing:
            // accepts dates, datetimes, and relative forms like "yesterday".
            let start_epoch = Some(crate::date::now());
            let end_epoch = match date {
                Some(d) => Some(crate::date::parse_datetime(&d, config.date.dialect)?),
                None => None,
            };

            // Determine priority, target_count and body. All three gate on
            // `open_editor` (the interactive flow). Pre-supplied body text
            // (`.. text`) bypasses both prompts and uses default priority /
            // target_count=0.
            let (priority_val, target_count, body) = if open_editor {
                handle_oneshot_task_creation(config)?
            } else {
                (config.tasks.default_priority, 0, body)
            };

            // Both the stable row id and the user-facing short id are
            // assigned by the database layer (see sql.rs).
            let mut task_obj = TaskObject {
                id: None,
                short_id: None,
                name: name_str.to_string(),
                body,
                priority: priority_val,
                start_time: start_epoch,
                available_duration_secs: None,
                interval_secs: None,
                target_count,
                optional: false,
                end_time: end_epoch,
            };
            let (new_id, new_short_id) = crate::sql::create_task(pool, &task_obj).await?;
            task_obj.id = Some(new_id);
            task_obj.short_id = Some(new_short_id);

            if !opts.quiet() {
                println!(
                    "Created task #{}: {}",
                    task_obj.short_id.unwrap_or_default(),
                    task_obj.name
                );
                if opts.verbose() {
                    crate::display::print_rows(&crate::display::task_rows(&task_obj));
                }
            }
        }
        TaskType::Recurring => {
            // Create new recurring task via interactive flow, with an
            // optional pre-filled name from `feeling ! @ <description>` and
            // an optional body from `.. body` (editor when `..` is bare).
            handle_recurring_task_creation(pool, config, opts, prefill, body, open_editor).await?;
        }
        TaskType::Scheduled => {
            // Scheduled task creation: `! @<time> [:description] [%<duration>]`.
            // The start time parsed from the command line must succeed before
            // any interactive prompt. Creation happens immediately only when
            // the start time, name and duration all came from the command
            // line; otherwise the flow goes interactive with whatever was
            // given pre-filled (a pre-filled value skips its prompt).
            let start_epoch = match &date {
                Some(d) => Some(
                    crate::date::parse_datetime(d, config.date.dialect).with_context(|| {
                        format!(
                            "Invalid scheduled task start time: '{}' \
                             (description starts with ':', duration with '%')",
                            d
                        )
                    })?,
                ),
                None => None,
            };
            let duration_secs = task
                .available_duration
                .as_deref()
                .map(crate::date::parse_duration_secs)
                .transpose()?;

            if let (Some(name_str), Some(start), Some(dur)) =
                (name.as_deref(), start_epoch, duration_secs)
            {
                if name_str.contains('\t') {
                    anyhow::bail!("Task name cannot contain tab characters");
                }
                let body = if open_editor {
                    open_editor_for_body(config.editor.hint)?
                } else {
                    body
                };
                let mut task_obj = TaskObject {
                    id: None,
                    short_id: None,
                    name: name_str.to_string(),
                    body,
                    priority: config.tasks.default_scheduled_priority,
                    start_time: Some(start),
                    available_duration_secs: Some(dur),
                    interval_secs: None,
                    target_count: 0,
                    optional: false,
                    end_time: None,
                };
                let (new_id, new_short_id) = crate::sql::create_task(pool, &task_obj).await?;
                task_obj.id = Some(new_id);
                task_obj.short_id = Some(new_short_id);

                if !opts.quiet() {
                    println!(
                        "Created task #{}: {}",
                        task_obj.short_id.unwrap_or_default(),
                        task_obj.name
                    );
                    if opts.verbose() {
                        crate::display::print_rows(&crate::display::task_rows(&task_obj));
                    }
                }
            } else {
                handle_scheduled_task_creation(
                    pool,
                    config,
                    opts,
                    name,
                    start_epoch,
                    duration_secs,
                    body,
                    open_editor,
                )
                .await?;
            }
        }
    }

    Ok(())
}

/// `feeling -` (bare) — tasks-edit entry point. Stub for now: interactive
/// task editing is future work (see TODO.md).
async fn handle_tasks_edit() -> Result<()> {
    anyhow::bail!("Task editing is not yet implemented");
}

/// Interactive oneshot creation flow (`feeling ! name ..`): cliclack
/// priority and target count prompts, then the body editor. Returns the
/// resolved `(priority, target_count, body)` for `handle_task` to insert.
fn handle_oneshot_task_creation(config: &Config) -> Result<(i32, i32, String)> {
    if !atty::is(atty::Stream::Stdin) {
        anyhow::bail!("Oneshot task creation requires an interactive terminal");
    }

    crate::display::task_intro("Create oneshot task")?;

    let priority = crate::prompts::prompt_priority(config.tasks.default_priority)?;
    let target_count = crate::prompts::prompt_target_count()?;
    let body = open_editor_for_body(config.editor.hint)?;

    Ok((priority, target_count, body))
}

async fn handle_recurring_task_creation(
    pool: &SqlitePool,
    config: &Config,
    opts: &CliOpts,
    prefill: Option<String>,
    body: String,
    open_editor: bool,
) -> Result<()> {
    use crate::date::parse_duration_secs;

    if !atty::is(atty::Stream::Stdin) {
        anyhow::bail!("Recurring task creation requires an interactive terminal");
    }

    crate::display::task_intro("Create recurring task")?;

    // 1. Task name (required, unique, no tabs) — re-prompt on duplicates
    // instead of aborting the whole flow. A pre-fill from `feeling ! @
    // <description>` skips the prompt entirely; on a duplicate the prompt
    // re-opens with the pre-fill as the default input so the user can
    // change it. The name is trimmed before use. The pre-filled value is
    // logged so the log file records what skipped the prompt.
    if let Some(p) = &prefill {
        cliclack::log::info(format!("Name: {p}"))?;
    }
    let name = prompt_unique_name(pool, prefill.as_deref()).await?;

    // 2. Priority (1..=999 per validation; blank falls back to default).
    let priority = crate::prompts::prompt_priority(config.tasks.default_recurring_priority)?;

    // 3. Start time (blank = the current moment, `date::now()`). This is the
    // recurrence anchor: interval boundaries are computed from it
    // (`task::current_interval_start`), and the placeholder shows the
    // formatted default so the current anchor is visible before editing.
    let start_time = crate::prompts::prompt_start_time(None, config.date.dialect)?;

    // 4. Interval (required, valid duration)
    let interval_str = crate::prompts::prompt_interval(None)?;
    let interval_secs = parse_duration_secs(&interval_str)?;

    // 5. Available duration (blank = always available; capped at the
    // interval — availability beyond it means always available).
    let avail_str =
        crate::prompts::prompt_available_duration(&interval_str, None, Some(interval_secs))?;

    let available_duration_secs = if avail_str.is_empty() {
        None
    } else {
        let dur = parse_duration_secs(&avail_str)?;
        if dur >= interval_secs {
            None
        } else {
            Some(dur)
        }
    };

    // 6. Target count (blank = 0, task can be completed once)
    let target_count = crate::prompts::prompt_target_count()?;

    // 7. End time (blank = never ends). `prompt_end` accepts a duration
    // (relative to now) or an absolute date/time and returns the epoch.
    let end_time = crate::prompts::prompt_end(None, config.date.dialect)?;

    // 8. Optional
    let is_optional = crate::prompts::prompt_optional(false)?;

    // 9. Body: command-line body (`.. text`) or the editor when `..` with
    // an empty body was given.
    let body = if open_editor {
        open_editor_for_body(config.editor.hint)?
    } else {
        body
    };

    // Insert into database. start_time marks the recurrence start (used as the
    // anchor for interval boundaries when applying completion deltas). Both the
    // stable row id and the user-facing short id are assigned by the database
    // layer (see sql.rs).
    let mut task_obj = TaskObject {
        id: None,
        short_id: None,
        name,
        body,
        priority,
        start_time: Some(start_time),
        available_duration_secs,
        interval_secs: Some(interval_secs),
        target_count,
        optional: is_optional,
        end_time,
    };
    let (new_id, new_short_id) = crate::sql::create_task(pool, &task_obj).await?;
    task_obj.id = Some(new_id);
    task_obj.short_id = Some(new_short_id);

    if !opts.quiet() {
        println!(
            "Created task #{}: {}",
            task_obj.short_id.unwrap_or_default(),
            task_obj.name
        );
        if opts.verbose() {
            crate::display::print_rows(&crate::display::task_rows(&task_obj));
        }
    }

    Ok(())
}

/// Interactive scheduled creation flow (`! @<time> [:description] [%<duration>]`
/// with anything missing from the command line). Mirrors the recurring flow:
/// required name (unique, re-prompt on duplicates) and start time, then the
/// available duration (blank → 1 hour), then priority. Scheduled tasks always
/// have target_count 0, so there is no target prompt. Values that came from
/// the command line skip their prompt.
async fn handle_scheduled_task_creation(
    pool: &SqlitePool,
    config: &Config,
    opts: &CliOpts,
    name: Option<String>,
    start: Option<i64>,
    duration: Option<i64>,
    body: String,
    open_editor: bool,
) -> Result<()> {
    use crate::date::parse_duration_secs;

    if !atty::is(atty::Stream::Stdin) {
        anyhow::bail!("Scheduled task creation requires an interactive terminal");
    }

    crate::display::task_intro("Create scheduled task")?;

    // 1. Task name (required, unique, no tabs). A name from the command
    // line skips the prompt entirely; on a duplicate the prompt re-opens
    // with the given name as the default input so the user can change it.
    if let Some(n) = &name {
        cliclack::log::info(format!("Name: {n}"))?;
    }
    let name = prompt_unique_name(pool, name.as_deref()).await?;

    // 2. Start time (required). A start time from the command line skips
    // the prompt; blank in the prompt means "now".
    let start = match start {
        Some(s) => {
            cliclack::log::info(format!("Start: {}", crate::date::format_datetime(s)))?;
            s
        }
        None => crate::prompts::prompt_start_time(None, config.date.dialect)?,
    };

    // 3. Available duration (required for scheduled tasks). A duration from
    // the command line (parsed to seconds in the caller) skips the prompt;
    // blank means the 1-hour default.
    let duration_secs = match duration {
        Some(d) => {
            cliclack::log::info(format!("Duration: {}", format_duration(d)))?;
            d
        }
        None => {
            let raw = crate::prompts::prompt_available_duration("1 hour", None, None)?;
            if raw.trim().is_empty() {
                3600
            } else {
                parse_duration_secs(&raw)?
            }
        }
    };

    // 4. Priority (blank falls back to the scheduled default).
    let priority = crate::prompts::prompt_priority(config.tasks.default_scheduled_priority)?;

    // 5. Body: command-line body (`.. text`) or the editor when `..` with
    // an empty body was given.
    let body = if open_editor {
        open_editor_for_body(config.editor.hint)?
    } else {
        body
    };

    let mut task_obj = TaskObject {
        id: None,
        short_id: None,
        name,
        body,
        priority,
        start_time: Some(start),
        available_duration_secs: Some(duration_secs),
        interval_secs: None,
        target_count: 0,
        optional: false,
        end_time: None,
    };
    let (new_id, new_short_id) = crate::sql::create_task(pool, &task_obj).await?;
    task_obj.id = Some(new_id);
    task_obj.short_id = Some(new_short_id);

    if !opts.quiet() {
        println!(
            "Created task #{}: {}",
            task_obj.short_id.unwrap_or_default(),
            task_obj.name
        );
        if opts.verbose() {
            crate::display::print_rows(&crate::display::task_rows(&task_obj));
        }
    }

    Ok(())
}

async fn handle_update(
    pool: &SqlitePool,
    opts: &CliOpts,
    target: UpdateTarget,
    count: Option<i64>,
) -> Result<()> {
    match target {
        UpdateTarget::OneShot(short_id) => {
            // `feeling - <id> [count]`: the id is the user-facing short id
            // (see sql.rs). Completed tasks have no short id and are not
            // addressable here — use the word query form instead.
            let info = crate::sql::fetch_oneshot_task_for_update(pool, short_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Oneshot task with id {} not found", short_id))?;
            update_oneshot(pool, opts, &info, count).await?
        }
        UpdateTarget::Query { words } => {
            // `feeling - <words…> [count]`: update the *unique* oneshot task
            // whose name contains the words in their order. Zero matches and
            // multiple matches both fail — the caller must disambiguate.
            let matches = crate::sql::fetch_oneshot_matching_words(pool, &words).await?;
            let joined = words.join(" ");
            match matches.len() {
                0 => anyhow::bail!(
                    "No task matches \"{}\" — the words must appear in a task name, in order",
                    joined
                ),
                1 => update_oneshot(pool, opts, &matches[0], count).await?,
                n => {
                    let names = matches
                        .iter()
                        .map(|m| match m.short_id {
                            Some(sid) => format!("'{}' (id {})", m.name, sid),
                            None => format!("'{}' (completed)", m.name),
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    anyhow::bail!(
                        "{} tasks match \"{}\": {}. Use more words or the task id",
                        n,
                        joined,
                        names
                    )
                }
            }
        }
    }

    Ok(())
}

/// Apply a completion increment to a oneshot task. `info.id` is the stable
/// row id; `info.short_id` is the user-facing id as it was before the update.
/// `update_task` syncs the short id to the completion state, so a done →
/// not-done transition reassigns the smallest free id — the not-done message
/// re-reads it so it reflects the post-update id.
async fn update_oneshot(
    pool: &SqlitePool,
    opts: &CliOpts,
    info: &TaskUpdateInfo,
    count: Option<i64>,
) -> Result<()> {
    let increment = count.unwrap_or(1) as i32;
    let new_completions = crate::sql::update_task(pool, info.id, increment).await?;
    let is_done = crate::task::is_task_done(info.target_count, Some(new_completions));

    if !opts.quiet() {
        if is_done {
            println!(
                "Task '{}' completed! (completions: {})",
                info.name, new_completions
            );
        } else {
            let short_id = crate::sql::fetch_task_short_id(pool, info.id).await?;
            println!(
                "Task '{}' (id {}) updated: {}/{} completions",
                info.name,
                short_id.unwrap_or_default(),
                new_completions,
                info.target_count
            );
        }
    }

    Ok(())
}

/// `:embed` — read one text line at a time from stdin, print the embedding
/// vector for each line as space-separated floats.
///
/// Diagnostic tool: uses raw text (no `feeling ` prefix) so users can probe
/// arbitrary strings independent of the runtime mood encoding.
pub fn handle_embed<R: BufRead, W: Write>(reader: &mut R, out: &mut W) -> Result<()> {
    let embedder = crate::embed::global_embedder();

    for line in reader.lines() {
        let line = line.context("Failed to read stdin")?;
        if line.trim().is_empty() {
            continue;
        }
        let vector = embedder.embed(&line, "")?;
        writeln!(out, "{}", crate::embed::format_vector(&vector))?;
    }
    Ok(())
}

/// `:color <feeling>` — diagnostic: embed the mood string with the
/// configured `moods.prefix_string`, run it through the full three-step mood-color
/// pipeline, and print intermediate values at each stage plus the final
/// Oklab / sRGB colour (with a terminal swatch of the final colour).
pub fn handle_color<W: Write>(
    mood: &str,
    config: &Config,
    opts: &CliOpts,
    out: &mut W,
) -> Result<()> {
    let embedder = crate::embed::global_embedder();
    let mood = mood.trim();
    let axes = config.moods.color_axes.as_ref().unwrap();

    // Verbose: dump the full axes settings up front; the per-value lines that
    // used to follow are gone — the dump carries them.
    if opts.verbose() {
        dbg!(&config.moods.axes);
    }

    // Embed the mood with the same prefix as the production pipeline.
    let embedding = embedder
        .embed(mood, &config.moods.axes.prefix_string)
        .context("Failed to embed mood")?;

    // The diagnostic always runs the full pipeline (no cached score).
    let weights = axes.regression_weights(&embedding, embedder, mood, None);
    let final_oklab = axes.weights_to_color(weights.as_ref());
    let rgb = final_oklab.to_srgb();

    let raw_emb = embedder.embed(mood, "").unwrap_or_default();
    let saliency = weights.as_ref().map(|w| w.saliency).unwrap_or(1.0);
    let s_eff = axes.effective_saliency(saliency);

    // Shift vector = prefixed embedding relative to the neutral base — the
    // vector the NNLS regression projects onto (see `regression_weights`).
    let shift: Vec<f32> = embedding
        .iter()
        .zip(&axes.base_vector)
        .map(|(e, b)| e - b)
        .collect();
    let cos_raw_shift = crate::embed::cosine_similarity(&raw_emb, &shift);

    // --- output ---
    writeln!(out, "mood              : {mood}")?;
    writeln!(
        out,
        "embedding         : {} floats (first 8: {:?}...)",
        embedding.len(),
        &embedding[..8.min(embedding.len())]
    )?;
    writeln!(
        out,
        "cos sim(raw,shift): {}",
        match cos_raw_shift {
            Some(c) => format!("{c:.4}"),
            None => "(undefined)".to_string(),
        }
    )?;
    writeln!(out, "saliency score    : {saliency:.4}",)?;
    writeln!(out, "effective saliency: {s_eff:.4}")?;

    // Regression weights: raw NNLS weights and the rescaled (power-weighted,
    // normalized) weights used for the Oklab blend, per contributing mood.
    match &weights {
        Some(reg) => {
            let raw = reg
                .raw
                .iter()
                .map(|(i, w)| format!("{}: {w:.4}", axes.basis_moods[*i].mood))
                .collect::<Vec<_>>()
                .join(", ");
            let rescaled = reg
                .rescaled
                .iter()
                .zip(&reg.raw)
                .map(|(w, (i, _))| format!("{}: {w:.4}", axes.basis_moods[*i].mood))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(out, "regression weights (NNLS)     : {raw}")?;
            writeln!(out, "regression weights (rescaled) : {rescaled}")?;
        }
        None => {
            writeln!(out, "regression weights (NNLS)     : (none)")?;
        }
    }
    writeln!(out)?;

    writeln!(
        out,
        "final Oklab: (L={l:.4}, a={a:.4}, b={b:.4})",
        l = final_oklab.l,
        a = final_oklab.a,
        b = final_oklab.b,
    )?;
    writeln!(
        out,
        "final sRGB : #{r:02X}{g:02X}{b:02X}",
        r = rgb.r,
        g = rgb.g,
        b = rgb.b,
    )?;
    // Real swatch rendered directly (works when stdout is a tty); the hex
    // line above is the capture-safe record. Debug builds additionally
    // print the copy-paste printf command for non-tty captures.
    writeln!(
        out,
        "swatch     : {}",
        "        ".on(crossterm::style::Color::Rgb {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        }),
    )?;
    #[cfg(debug_assertions)]
    writeln!(
        out,
        "to visualise (copy-paste): printf \"\\x1b[48;2;{r};{g};{b}m  \\x1b[0m\"",
        r = rgb.r,
        g = rgb.g,
        b = rgb.b,
    )?;

    Ok(())
}

// -------------------------- HELPERS -------------------------

/// Resolve a unique, non-empty task name for creation. A name from the
/// command line skips the prompt entirely; on a duplicate the prompt
/// re-opens with the given name as the default input so the user can
/// change it.
async fn prompt_unique_name(pool: &sqlx::SqlitePool, given: Option<&str>) -> Result<String> {
    let given = given.map(str::trim).filter(|s| !s.is_empty());
    if let Some(name) = given {
        if !crate::sql::recurring_task_name_exists(pool, name).await? {
            return Ok(name.to_string());
        }
    }
    loop {
        let candidate = crate::prompts::prompt_name(given)?;
        if crate::sql::recurring_task_name_exists(pool, &candidate).await? {
            cliclack::log::error(format!("A task with name '{candidate}' already exists"))?;
            continue;
        }
        return Ok(candidate);
    }
}

#[cfg(test)]
mod tests {
    use super::interval_slot;
    use crate::date;

    /// The slot always contains the entry and has the requested length.
    #[test]
    fn interval_slot_contains_entry() {
        let t = date::today_start() + 12 * 3600;
        for interval in [1800i64, 86400, 604800] {
            let (start, end) = interval_slot(t, interval);
            assert!(t >= start && t < end, "{t} not in [{start}, {end})");
            assert_eq!(end - start, interval);
        }
    }

    /// Uniform tiling: adjacent slots touch (no gaps/overlaps). This is the
    /// property that keeps sub-day trackers (e.g. 30 min) working.
    #[test]
    fn interval_slot_tiles_uniformly() {
        let t = date::today_start() + 12 * 3600;
        for interval in [1800i64, 86400] {
            let a = interval_slot(t, interval);
            let b = interval_slot(t + interval, interval);
            assert_eq!(a.1, b.0, "slots must be adjacent for {interval}s");
        }
    }

    /// Sub-day intervals: entries in the same 30-min bucket share a slot,
    /// crossing a boundary doesn't.
    #[test]
    fn interval_slot_sub_day() {
        let t = date::today_start() + 10 * 3600; // 10:00 local
        let bucket = interval_slot(t, 1800);
        assert_eq!(interval_slot(t + 600, 1800), bucket); // 10:10 — same bucket
        assert_ne!(interval_slot(t + 1801, 1800), bucket); // 10:30:01 — next bucket
    }
}
