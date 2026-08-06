# feeling — Architecture

A CLI + TUI journaling and task-tracking tool. Mood entries (optionally journal
bodies), custom numeric trackers, oneshot/recurring/scheduled tasks, all stored
in a single SQLite database. Terminal output is tab-separated and parseable; the
same views run as a fullscreen ratatui TUI when stdout is a TTY.

---

this project is in progress and we don't need to worry about migrations or
breaking changes

removed features are treated as if they never existed: no tests, no mentions
in this document, no changelog rows

when running tests, use --test-threads=4

## 1. Crate layout

The crate is a **library + thin binary** so that integration tests can import it
as `feeling::`.

```text
src/
  lib.rs        re-exports every module
  main.rs       thin binary: init flow + dispatch (the only TTY/TUI decision point)
  paths.rs      XDG-style paths (config dir, state dir, db, log)
  config.rs     serde Config (config.toml) incl. mood pairs, task colors, custom trackers
  config/       config-side types (types.rs: MoodEndpoint, TrackerType, ColorBins)
  logger.rs     cba `bog`/env_logger setup (env_logger piped to the log file only)
  db.rs         SQLite pool, CREATE TABLE schema, indexes, PRAGMA foreign_keys
  date/         all chrono/humantime usage lives here
    mod.rs      Epoch type + now/today/week/month/year boundary helpers
    parse.rs    datetime string parsing via chrono-english → epoch (+ parse_date)
    parse_duration.rs  duration string parsing via humantime → seconds
    format.rs   duration/time/date/datetime/DM-Y formatting
  clap.rs       manual CLI parser → Command enum + CliOpts flag counts (no clap crate)
  handlers.rs   command dispatch + all write paths (entry, task flows, prune, clear, :color)
  prompts.rs    all interactive cliclack prompts (priority, target count, interval,
                end time, optional, name, start time, clear confirm)
  sql.rs        all SQL queries and data-access types (the only module
                that contains sqlx query calls)
  display.rs    shared output helpers (task_rows/print_rows confirmations,
                format_today_simple, format_tasks_simple)
  task.rs       shared task logic (completion deltas, interval floors, done-ness,
                the unified Enter-action used by both TUIs)
  views.rs      non-TTY view output (tasks, today, trackers) + fetch queries
  action.rs     unified Action enum emitted by event loop and consumed by TUI apps
  binds.rs      crokey key combinations -> Action bindings (default_binds)
  message.rs    RenderEvent (Action, Resize) and ControlEvent (Pause, Resume) channels
  event_loop.rs async crossterm input stream parser & action emitter
  embed.rs      nomic-embed-text-v1.5 ONNX embeddings + saliency adaptor via ort (ONNX Runtime)
  color.rs      Oklab mood-color projection from embeddings (NNLS + saliency)
  color_conversion.rs  rgb ↔ Oklab conversion
  editor.rs     `..` body editor (VISUAL/EDITOR)
  tui.rs        thin matchmaker-style fullscreen terminal wrapper
  render/       ratatui rendering
    mod.rs      Render trait & shared TUI lifecycle runner
    system.rs   TUI external editor suspension helper (pause/resume event loop)
    tasks.rs    task-list TUI (`@[:o|:O]`, `@done[:o|:O]`) via TasksApp
    today.rs    today TUI (Today/Tomorrow/Week horizons) via TodayApp
    preview.rs  task & today entry preview rendering helpers
    utils.rs    priority colors, mode labels, string truncation
```text

### main.rs flow

```text
bog::init_bogger
cmd     = parse_args()                                          // feeling::clap
init_logger([q, 1+v], log_path())                               // q/v from cli.opts
config  = load_type_or_default(default_config_path(), toml)     // cba
         config.init()                                          // tracker-name validation, palette fallback
pool    = db::init_database(database_path())
out     = io::stdout()
tui     = atty::is(Stdout)                                      // ← the single TUI decision point
handle_command(cmd, &pool, &config, &cli.opts, &mut out, tui)
```text

`handle_command` takes an explicit `tui: bool` — the TUI never auto-launches from
inside the library, so integration tests always pass `false` and exercise the
plain view output. The leading `-q`/`-v` flags (counted into `CliOpts { qv:
[u8; 2] }`, `quiet()`/`verbose()`/`verbose_level()`) drive both the logger and
handler-level output gating (task confirmations, grid titles, the `:color`
axes dump).

### User-facing logging — `cba::ebog!`/`wbog!`/`ibog!` (print to stdout)

All errors, warnings, and notifications the user should see in the terminal are
emitted through the cba bogger macros (printed to stdout by `bog::init_bogger`
in §1). Use `ebog!`, `wbog!`, `ibog!` (and `nbog!`/`dbog!`/`mbog!`/`cbog!` as
appropriate) with the typed form `MACRO!("tag"; "message")` when the error
category is worth surfacing.

**Do not** use `log::warn!` / `log::error!` / `log::info!` for user-facing
output — `init_logger` pipes env_logger to the **log file only**
(`Target::Pipe`), so those calls are invisible in the terminal. Only the
bogger reaches the terminal. `log::debug!` is fine (and stays as-is — those
are for the log file / internal debugging). Notable exceptions: interactive
prompts and TUIs write to stderr (cliclack), and `:color` writes its
diagnostic to the caller's `out`.

---

## 2. Paths & config

**paths.rs** — `BINARY_FULL = "feeling"`. `FEELING_CONFIG_DIR` env var overrides
the config dir when it exists. Config path: `~/.config/feeling/dev.toml` in debug
builds, `config.toml` in release (cba `expr_as_path_fn`). State dir:
`dirs::state_dir()/feeling` (fallback `~/.local/state/feeling`). DB at
`state_dir/feeling.db`, log at `state_dir/feeling.log`.

**config.rs** — `Config` (serde, every section `deny_unknown_fields` with
per-field defaults).

`Config::init` (called from main after load) drops tracker names that collide
with CLI syntax — `:`-prefix, `-`/whitespace inside, or names made purely of
the flag letters `q`/`v` — and falls back to the default palette when
`tasks.colors` has fewer than 3 entries.

---

## 3. Database schema

`db.rs` runs `CREATE TABLE IF NOT EXISTS` + indexes (`run_migrations`). **There
are no ALTER migrations** — the project is treated as fresh; schema changes mean
deleting the dev DB (and test DBs are always created in memory). `PRAGMA
foreign_keys = ON` in both `init_database` and `test_pool`.

**PRAGMA journal_mode**: `WAL` in release builds; `DELETE` in debug builds
(to avoid leaving `-wal`/`-shm` sidecar files in the state dir during
dev work). Setting `DELETE` explicitly also converts a pre-existing
WAL-mode db file, so the mode is deterministic regardless of the
database's prior state.

```text
feeling:  id, mood TEXT NOT NULL, body TEXT NOT NULL DEFAULT '', time INTEGER (unixepoch),
          embedding BLOB, score REAL          -- cached saliency for the mood text; computed at
                                              -- entry creation, backfilled for legacy rows
custom:   id, type TEXT, score BLOB NOT NULL CHECK (typeof(score) IN ('integer','text','real')),
          time, feeling INTEGER → feeling(id)   -- nullable: tracker entries without a linked feeling are allowed
          -- score holds a text | integer | real payload per the tracker's declared kind;
          -- BLOB decltype (no affinity) preserves the storage class exactly
todos:    id (AUTOINCREMENT), name TEXT NOT NULL, body, priority INTEGER DEFAULT 5,
          short_id INTEGER UNIQUE,   -- user-facing id; first-free-gap, NULL once a oneshot
                                     -- task is completed (see "Short-id allocation" below)
          name_embedding BLOB,       -- reserved for a name-derived embedding; never populated
          start_time, available_duration_secs, interval_secs, target_count INTEGER DEFAULT 0,
          optional INTEGER DEFAULT 0, end_time
todo_completions: id, todo_id → todos(id) ON DELETE CASCADE,
          -- row ids are stable (never reassigned), so no ON UPDATE CASCADE.
          time INTEGER, count INTEGER NOT NULL DEFAULT 1
embedding_cache: text TEXT PRIMARY KEY, embedding BLOB
```text

Timestamps are **Unix epoch seconds (INTEGER)** everywhere, via the `date` module.

### Completion semantics

- `todos` has **no** `completions` column. `todo_completions` is the event log
  (one row per update, `count` = that update's value). Totals are derived as
  `SUM(count)`.
- **Done-ness** (`task::is_task_done`): `target_count <= 0` → done iff any
  completion exists with `count > 0` (the `Some(0)` case is *not* done — zero
  completions is the not-done state regardless of `target_count`); `target_count > 0` → done iff
  `completions >= target_count`.
- **Deltas** (`task::apply_completion_delta`): positive delta inserts a row with
  `count = delta`. Negative deltas are consumed **at write time** from the most
  recent entries backwards (whole entries removed, last one reduced) so no
  negative rows are ever stored and totals floor at 0. Pure model
  `apply_delta_to_counts` is unit-tested.
- **Interval awareness**: recurring tasks define their recurrence start as
  `start_time` (CLI creation stores `date::now()`). The current interval boundary
  is `start_time + floor((now - start_time)/interval) * interval`
  (`task::current_interval_start`, `div_euclid`, clamped when `now <= start_time`).
  Negative deltas on recurring tasks never touch entries recorded before that
  boundary; the returned total is interval-scoped for recurring tasks.
- **Scheduled tasks** keep at most one completion row: value `1` = completed
  (early, or auto-completed when the window elapses), `0` = failed (marked as
  missed). `set_scheduled_completion` replaces the row in a transaction.

### Short-id allocation

`todos.id` is a plain `AUTOINCREMENT` primary key — **stable, never
reassigned**. The user-facing id is the separate `short_id` column:

- New tasks get the **smallest free positive** short id (>= 1), the first gap
  among `short_id IS NOT NULL` rows (`sql::allocate_short_id`). The row id and
  short id are both assigned by the database layer (`create_task` asserts the
  caller passes neither).
- A oneshot task whose `is_task_done` transition is `false → true` has its
  `short_id` cleared (`NULL`), freeing the id for reuse. Transitions in the
  reverse direction (negative deltas / resets that remove `todo_completions`
  rows) reassign the smallest free short id (`sql::sync_short_id`, called by
  `update_task` and `reset_task_completions`).
- **Recurring tasks keep their short id** across intervals — their "done"
  state is interval-scoped and transient, so clearing/reassigning per interval
  would churn ids. Display code hides the id of any task that is done.
- The allocator query is a pure read; the `short_id` column is `UNIQUE` (NULLs
  are distinct), so a concurrent double-allocation fails loudly at INSERT time
  rather than silently sharing an id. In practice the CLI is single-threaded
  per invocation.
- Completed oneshot tasks are no longer addressable by id (`- <id>` queries
  `short_id`); untoggle them via the word query form or the TUI @done reset.

---

## 4. Date & duration parsing (date/)

All date and duration string parsing is encapsulated in the `date/` sub-module so callers work exclusively with `Epoch` (i64 Unix epoch seconds) or duration seconds (`i64`) without touching `chrono` or `humantime` types directly.

- **Datetime parsing (`date/parse.rs`)**: `parse_datetime(s: &str, dialect: DateDialect) -> Result<Epoch>` uses `chrono-english` (`chrono_english::parse_date_string`) with `chrono::Local::now()` as the anchor. The dialect (`DateDialect::Uk` day-first, default, or `DateDialect::Us` month-first) comes from the `[date] dialect` config setting; it only matters for ambiguous slash forms like `3/5/2024`. It handles both natural language expressions (e.g. `"yesterday"`, `"tomorrow 9am"`, `"3 days ago"`) and fixed format strings (e.g. `"2024-03-15"`, `"2024-03-15 14:30:00"`), returning epoch seconds directly. `parse_date(s, dialect)` additionally aligns to the start of that day — it backs the `feeling @<date>` today view.
- **Duration parsing (`date/parse_duration.rs`)**: `parse_duration_secs(s: &str) -> Result<i64>` uses `humantime` (`humantime::parse_duration`) to parse human-readable durations (e.g. `"1 day"`, `"2 hours"`, `"1d"`, `"2h"`), returning total seconds as an `i64`.
- **Formatting (`date/format.rs`)**: `format_time` (HH:MM), `format_date` (ISO), `format_date_time` (ISO + HH:MM), `format_datetime_short` (= date_time), `format_date_dmy` (DD-MM-YY, the today TUI's anchored-day label), `format_duration` (humantime).
- **Boundary helpers (`date/mod.rs`)**: `now`, `today_start`/`today_end`, `day_start`/`day_end` (arbitrary day), `week_start(weekday)` (grids), `month_start`/`month_end`, `year_start`/`year_end`, rolling variants (`rolling_month_start`, `aligned_year_start`).

---

## 5. CLI parsing (clap.rs)

Manual parser, no clap crate. `parse_args()` (from `env::args`) and
`parse_from(Vec<String>)` (unit-testable). A leading flag run (`-q`/`-v`,
singly or combined like `-qv`) is stripped into `CliOpts { qv: [u8; 2] }`
counts — order is not tracked (`-vq` ≡ `-qv`). `-h`/`--help` in the initial
position short-circuits to `Command::Help`; after the first non-flag token
everything is command text (so `feeling ok -q` treats `-q` as entry text).
`parse_from` dispatches on `args[0]`:

| Input | Command |
| --- | --- |
| (no args) | `Today { date: None }` — today view (`feeling` with no subcommand) |
| `@<date>` | `Today { date: Some(date) }` — today view anchored to that day; the **handler** parses it with `config.date.dialect` (the parser has no config, so nothing is validated here) |
| `--help` / `-h` | `Help` — bundled `assets/help.txt` printed via `include_str!` |
| plain words (`happy`, `good ...`) | `Entry { feeling, customs, .. }` — mood entry; custom trackers as `-type score` |
| `..` (bare, at the end) | opens the body editor (Entry/Task with `open_editor`) |
| `!` (bare) | `Task { OneShot, name: None, open_editor: true }` — interactive oneshot creation (name prompted via `prompt_name`) |
| `! description [@date] [..]` | `Task { OneShot, .. }` — `@YYYY-MM-DD` is the **due** time (stored in `end_time`; `start_time` records creation); a second `@`-word is rejected |
| `! @` / `! @ description` | `Task { Recurring, prefill }` — interactive recurring creation; the description pre-fills the name prompt (`@`-words inside it stay free text) |
| `! @<time> [:name] [%<duration>] [..]` | `Task { Scheduled, .. }` — scheduled creation; immediate when all three fields came from the CLI, else interactive with pre-fills. The space discriminator is load-bearing: `! @ 10pm` (bare `@`) is recurring, `! @10pm` is scheduled |
| `@[:o\|:O]` | `View { PendingTasks, show }` — pending tasks (all / oneshots only / recurring+scheduled, not availability-filtered); recently-completed tasks stay within `persist_pending_seconds` (D9) |
| `@done[:o\|:O]` | `View { DoneTasks, show }` — completed tasks (all / oneshots only / recurring history + scheduled incl. auto-completed) |
| `@due[:t\|:w]` | `Today { date: None, show: B, horizon: Today\|Tomorrow\|Week }` — today view, tasks only |
| `@<date>` | `Today { date: Some, show: All, horizon: Today }` — anchored today view |
| `- id [count]` | `Update { OneShot(id), count }` — `id` is the user-facing short id; `count` may be negative |
| `- words… [count]` | `Update { Query(words), count }` — the unique oneshot task whose name contains the words in order (subsequence match) |
| `-` alone | `TasksEdit` — stub: `handle_tasks_edit` bails "not yet implemented" |
| `:` / `:week\|month\|year` / `:` + ids | `Tracker { period, items }` — dot-sequence tracker views (`:g` bails "not yet implemented") |
| `:embed` | `Embed` — stdin lines → one 768-dim vector per line |
| `:score "start" "end"` | `Score` — stub (`todo!()`) |
| `:config` | `Config` — opens the live config in `$VISUAL`/`$EDITOR` |
| `:prune` | `Prune` — prunes expired tasks, clears the embedding cache |
| `:color <mood>` | `Color` — full mood-color pipeline diagnostic |
| `:clear [@date]` | `Clear` — deletes that day's mood entries (interactive confirm) |

Custom tracker names cannot begin with `@` (reserved for recurring ids).
Tabs in mood/name fields are an error (output is tab-separated); whitespace in
recurring task names is allowed (the `@` prefix disambiguates).

---

## 6. Handlers (handlers.rs)

`handle_command<W: Write>(cmd, pool, config, opts: &CliOpts, out, tui)` matches
the `Command` enum. `opts` gates confirmations and verbose output throughout.

- **Entry** → `handle_entry`: runs in a transaction; inserts `feeling` (+ optional
  `custom` rows with `feeling_id`). Each `-type value` is parsed against the
  tracker's declared kind (text/number/float) with a clear error on mismatch;
  min/max apply to number/float, unknown tracker types are rejected. Insertion
  strategy is kind × interval: `text`/`float` trackers **with an interval** keep
  one entry per interval slot — re-logging the same tracker inside the same slot
  replaces the previous entry; `number` trackers and interval-less trackers are
  plain inserts that accumulate (the views sum per-slot scores). When the mood is
  non-empty it is embedded **before** the transaction opens, and its emotional
  saliency is computed (`color::predict_saliency`) and stored in `feeling.score`
  — so later color passes skip the saliency ONNX run for fresh rows. A wholly
  empty entry is aborted ("Nothing to log"). Confirmation output
  (`display::display_entry`) is quiet-gated.
- **Task** → `handle_task`:
  - oneshot: name required (no tabs); `@<time>` parsed with the config dialect
    (an unparseable date fails creation); interactive priority/target prompts
    only when the body editor was requested.
  - recurring: interactive flow (`feeling ! @ <description>`) — cliclack
    prompts for name (unique, pre-filled), priority, interval, available
    duration, target count, end time, optional, and the body editor. The flow
    bails when stdin is not interactive. Pre-filled values are logged at info
    level (`prefill` tag) and skip their prompts.
  - scheduled: `! @<time> [:name] [%<duration>]` — the start time is parsed
    with the config dialect before any prompt; creation is immediate when the
    name, start and duration all came from the CLI (priority = the scheduled
    default), otherwise the interactive flow pre-fills what was given
    (available duration blanks to 1 hour). Scheduled tasks always have
    `target_count 0`.
  - The confirmation (`Created task #N: name`) is printed by the call sites:
    header only by default, `-v` adds the field rows (`task_rows`), `-q`
    silences it entirely.
- **Update** → `handle_update`: applies the delta via `task::apply_completion_delta`
  (interval-aware for recurring) and prints the new total (quiet-gated). The
  `- <id>` form looks up the task by user-facing short id (see §3); completed
  tasks have no short id and are not addressable here (use the word query form
  instead).
- **Today** → resolves `@<date>` via `parse_date` with `config.date.dialect`
  (day-aligned), then TUI (`TodayApp::new(pool, config, day_epoch, show,
  horizon).run()`) when `tui`, else `views::handle_today(...)`.
- **View** → TUI (`TasksApp::new(pool, mode, config, show,
  persist_pending_seconds).run()`) when `tui`, else
  `views::handle_view(pool, mode, config, show, out)`.
- **Tracker** → `views::handle_tracker` (no TUI path yet).
- **Prune** → prunes expired/completed tasks and clears the **entire**
  `embedding_cache` (it is a cache — rows are lazily re-embedded).
- **Clear** → `:clear [@date]` deletes that day's mood entries (and linked
  customs), with an interactive confirm showing the computed date.
- **Color** → `:color <mood>` runs the whole pipeline once and prints every
  intermediate value; `-v` additionally dumps `config.moods.axes` up front.
- **Embed** → `handle_embed`: stdin lines → one embedding vector per line.
- **Score** → stub (`todo!()`).

`main.rs` supplies `out = io::stdout()`; every view/today/tracker function takes
`&mut W: Write`. TUI branches run the ratatui apps; everything else writes
plain text to `out`.

---

## 7. Views (views.rs) — non-TTY output

All non-TTY output is newline-separated rows, tab-separated columns.

### Task lists — `format_tasks_simple`

`id \t interval \t next_available \t pri \t name \t status`, sorted priority
desc (then due nearness). `id` is the user-facing short id; completed
oneshot tasks show an empty id column (their short id was cleared on
completion). `interval` is the recurring task's interval
(`date::format_duration`); `next_available` is the next time a recurring task
becomes available — the start of the next interval window
(`date::format_date_time`). Oneshot tasks render a single space in both
the interval and next_available columns. The status column is the
**completion badge** (§8): `"●"` alone when complete (no `DONE` word), `"● m/n"`
in progress, `"◯"` at 0%.

TaskRow queries use the **interval-scoped GROUP BY pattern** — the JOIN
condition scopes recurring completions to the current interval
(`tc.time >= start_time + floor((now - start_time)/interval) * interval`),
oneshot tasks keep all-time sums, and `GROUP BY t.id` is deterministic (all
columns functionally depend on the PK). The same fragment is inlined in
views.rs, render/tasks.rs and render/today.rs (no shared const — queries are
intentionally duplicated). Single-row fetches use a correlated `(SELECT
SUM(count) ...)` subquery with the same boundary condition.

Per-mode filters (`fetch_tasks_for_view(mode, show, persist_pending_seconds)`):
`@` All = not-done oneshots ∪ recurring (interval-scoped, not expired,
availability-checked in Rust) ∪ ongoing scheduled (window open, no entry) ∪
recently-completed (any kind, last `persist_pending_seconds`); `@:o` =
not-done oneshots only (+ recently-completed oneshots); `@:O` = not-done
recurring (any not expired, no availability check) ∪ scheduled with an open
window (+ recently-completed sched/recur); `@done` = done oneshots ∪
scheduled with any entry ∪ recurring done in the current interval; `@done:o`
= done oneshots only; `@done:O` = ALL recurring (one row per task, no
completions filter — history, expired and never-completed rows) ∪ scheduled
with any entry or auto-completed (no entry, window elapsed). All view
fetches carry an ORDER BY, but it doesn't actually matter: every consumer
re-sorts in Rust with the shared view keys — `views::task_done_time` for
`@done` (last completion entry; entry-less rows fall back per kind to
`start_time + duration` for auto-completed scheduled, `start_time` for
zero-entry recurring history), `views::task_entry_time` for pending lists
and the today view (done rows by last completion entry, scheduled →
`start_time`, recurring → current-interval window end while still open,
else the start of the *next* interval, oneshot
→ due time), and `fetch_today_entries` always ends in `today_sort`. The SQL
order only survives where the Rust keys tie (the sorts are stable).

### Today view — `fetch_today_entries` / `format_today_simple`

`ts \t marker \t label \t detail` rows aggregated from feelings, custom
entries, due oneshot tasks, scheduled tasks whose window overlaps the horizon,
and active recurring tasks (recurring availability filter:
`(now - start_time) mod interval < available_duration`). **Rows are tasks
only — completion events are not rendered**; a done task carries `✓` on its
own row instead. Badges: `●` feeling + Oklab mood projection; `◆` numeric
custom + `bin_score_color`; `·` text custom; oneshot tasks use `○`
(in-progress) / `✓` (done); recurring tasks use `↻` (in-progress) / `✓`
(done); scheduled tasks render `✓` for done/auto-completed states and `◷`
for ongoing and failed states; journal
entries (empty mood) use `config.today_view.journal_badge` when set (no badge
otherwise). Each entry carries its marker glyph (`TodayEntry.badge:
Option<char>`) and a dynamic dot color (`TodayEntry.color`, type
`ratatui::Color`) computed at fetch time. The TUI and the text rows consume
the same `TodayEntry`s, so both renderers share the exact badge/color logic.

Horizons: Today / Tomorrow / Week — **Week is always the next 7 days** from
the anchored day. `feeling @<date>` anchors the view to any parseable day
(day-aligned, config dialect; the TUI title shows Today / Yesterday /
DD-MM-YY). The oneshot query has two orthogonal bounds on the due time
(`COALESCE(end_time, start_time)`, where `end_time` is the `@<time>`
deadline and the fallback keeps legacy rows and undated tasks working): an
upper bound at the horizon end and — unless
`config.today_view.include_overdue` — a lower bound keeping only tasks due
from today onward. Empty day → `Nothing logged today.`

In the text output, `format_today_simple` wraps the marker glyph in the
entry's ANSI color, skipping color when `color == Reset` (e.g. 0% tasks) so
parseable tab-separated rows don't carry no-op escape sequences.

### Trackers — `: [week|month|year] [ids]`

Dot-sequence history grids, colored via config. Grid ranges follow
`config.grid` (rolling vs calendar week/month/year variants); year grids use
the weekday-rows heatmap. `handle_tracker` prints one section per item:

- **Section titles** are verbose-only: bare `Moods` / `idea` / `@name` at
  `-v`, `({period:?})` suffix at `-vv`+, and nothing at default verbosity —
  sections are then separated by a blank line (skipped before the first item).
  The "No entries" / "No completions" / "not found" messages are
  unconditional.
- **Mood tracker** (`display_mood_tracker`): per-day dot colored by the Oklab
  mood projection (§11), preferring stored `feeling.embedding` blobs over
  on-the-fly embedding. Days without an entry render `◯`; days with an entry
  render `●`. Queries filter `mood != ''` so journal-only entries are excluded.
- **Custom tracker** (`display_custom_tracker`): per-interval dots binned with
  `bin_score_color` (score vs `min`/`max`, inverted ranges handled; no
  blending) for `number`/`float` kinds; `text` trackers list each entry as a
  dark-gray `> text` line — at `-v` each line appends the entry's own
  timestamp in Darkgray (`> walked the dog [2024-03-15 14:22]`).
- **Recurring task tracker** (`display_recurring_tracker`): per-interval dots
  from the per-interval completion sum via the shared `completion_badge` (§8)
  — a zero-sum interval renders `◯`, sum ≥ target renders the last bin color,
  in between bins over `colors[..len-1]`. Interval-less recurring tasks render
  one dot per completion event.

---

## 8. Completion badge (the only completion-status rendering)

`views::completion_badge(config, count, target_count) -> (char, CtColor)` +
`completion_badge_text(...)` produce the **single** completion-status display
used everywhere — CLI task lists, the TUI table status cell, the preview pane,
and the recurring tracker:

| state | render |
| --- | --- |
| no entries / interval sum 0 (0%) | `◯` (U+25EF, **uncolored** — `Color::Reset`, so no ANSI codes are emitted; code skips `.with()` for Reset) |
| 100% (`count >= target`, or any `count > 0` when `target_count <= 0`) | `●` in `colors.last()` — badge text `●` (no `DONE` word) |
| in between (`0 < count < target`) | `●` binned over `colors[..len-1]` (last bin is reserved for 100%) — text `● n/m` |

`target_count = 0` never shows an `n/m` fraction (just `◯` or `●`).
Binning is **discrete only — no blending**. The continuous Oklab mood projection
is the deliberate exception. `config.tasks.colors` is **guaranteed nonempty**
under all runtime paths — `Config::init` falls back to the default
dark-red / dark-yellow / dark-green palette when fewer than 3 colors are
configured — so all binning/indexing code can assume index 0 and index
`len-1` exist.

---

## 9. TUI (tui.rs + render/ + event loop)

**tui.rs** is a thin matchmaker-style wrapper: always fullscreen, `IoStream`
abstraction, `enter`/`exit`, `resize`, `Drop` cleanup. `enter_execute`/
`return_execute` suspend and restore terminal state when launching external
subprocesses (such as `$VISUAL`/`$EDITOR`).

### Event loop & action channel architecture

The TUI uses an asynchronous, channel-driven event loop decoupling input reading
from frame rendering:

- **`Action` enum (`action.rs`)**: unified action set emitted by input parsing
  (`Up`, `Down`, `Left`, `Right`, `Accept`, `Edit`, `Delete(bool)`, `CycleMode`,
  `ToggleSort`, `CycleShow`, `Refresh`, `Quit`, `Ack`, `Input(char)`). Both
  `TasksApp` and `TodayApp` match on every variant and ignore those that don't
  apply to their context (`CycleShow` cycles the ShowVariant in both apps).
- **Key bindings (`binds.rs`)**: `default_binds()` uses `crokey::KeyCombination`
  to map key combinations to `Action`s (`q`/`esc` quit, `j`/`k`/`h`/`l` nav,
  `tab` cycle mode/horizon, `ctrl-s` sort, `ctrl-d` show-variant cycle, `enter`
  accept, `delete`/`backspace` delete, `ctrl-e` edit, `ctrl-r` refresh).
  Unbound keys fall through to `Action::Input(char)` for modal text fields.
- **Event loop (`event_loop.rs`)**: runs in a dedicated tokio task reading
  crossterm's `EventStream`. Key presses map to `Action`s and emit `RenderEvent`s
  (`Action` or `Resize`) over an unbounded `mpsc` channel. Redraws only happen when an
  event arrives (no tick loop).
- **Process suspension & control events (`message.rs`, `render/system.rs`)**:
  When an external editor is launched (`Ctrl+e`), `edit_with_editor` sends
  `ControlEvent::Pause` to the event loop and awaits `Action::Ack`. The event
  loop drops its `EventStream` so keystrokes are not stolen from the editor.
  `tui.enter_execute()` yields the terminal. On exit, `tui.return_execute()`
  restores terminal state, sends `ControlEvent::Resume`, awaits `Action::Ack`,
  and resumes input reading.

### `Render` trait & app lifecycle (`render/mod.rs`)

`pub trait Render` provides the shared TUI runner loop for both `TasksApp` and `TodayApp`:
`render(&self, f: &mut Frame)` draws state; `handle_action(...)` handles state
updates and modal routing; `should_quit()` is the exit condition; `run()` is
the default lifecycle — enter fullscreen, spawn the event loop, draw on every
`RenderEvent`, dispatch actions, exit cleanly.

### Enter (`Action::Accept`) — the shared task toggle

Both TUIs route Enter through the same pure decision fn
(`task::enter_action`), executed by `task::apply_enter_action`:

| task kind | state | Enter → |
| --- | --- | --- |
| scheduled | no entry / done / failed | toggles directly, never a modal: `none → 1 (done) → 0 (failed) → none (clear, before the window end) / 1 (after)` |
| once-only / `target_count ≤ 1` | not done / done | toggles: complete (+1) ↔ reset immediately (no modal) — except the tasks TUI's @done view, where the reset asks first |
| `target_count > 1` | not complete | `CompleteModal` — numeric completion delta prompt |
| `target_count > 1` | complete | "Reset progress?" confirm modal, **default Yes** (recurring tasks reset only the current interval — earlier history survives) |

### Item Editing (`Action::Edit`)

`Ctrl+e` / `e` on the selected item: tasks and feeling entries open the
external editor pre-filled with the body (event loop suspended via
`ControlEvent::Pause`); text custom trackers open the editor on their payload;
`number`/`float` customs use the in-TUI `EditTrackerModal` (validated against
the tracker kind on Enter).

### TUI Applications

- **`TasksApp` (`render/tasks.rs`)** — task-list App (`@[:o|:O]`, `@done[:o|:O]`):
  fields include `pool`, `tasks`, `selected`, `mode`, `show` (`ShowVariant`),
  `persist_pending_seconds`, `config`, `sort_by_due`, `modal`. Modals:
  `CompleteModal`, `DeleteConfirm` (default No; recurring tasks add an indented
  italic "This task will stop recurring!" line), `ResetConfirm` (default Yes),
  `AvailabilityConfirm` (D10 — Enter on a recurring task whose availability
  window has passed; default Yes). `@done` sorts by `views::task_done_time`
  (last completion entry; entry-less rows fall back per kind —
  auto-completed scheduled → `start + duration`, zero-entry recurring →
  `start_time`) newest first in date mode; equal-priority ties fall back to
  the same key. Expired `@done:O`
  history rows log `task {id} is expired` and ignore actions.
  Table renders `short_id / pri / name` with badge and `m/n` sub-line;
  completed tasks show no id. Preview pane (`render/preview.rs`) shows detailed
  task fields.
- **`TodayApp` (`render/today.rs`)** — Today App: fields include `pool`,
  `config`, `entries`, `selected`, `horizon`, `day_epoch`, `day_label`,
  `sort_by_priority`, `modal`, `selected_task`, `color_cache`. Cycles horizons
  (Today → Tomorrow → Week) via `Tab`; the title shows the anchored day
  (`Today` / `Yesterday` / `DD-MM-YY`); handles mood/task body edits, custom
  tracker edits, and deletion — `Delete` works on feeling / custom / task
  entries with a confirm modal ("Delete journal entry?" for nameless rows).

---

## 10. Embeddings (embed.rs)

- Model: **nomic-embed-text-v1.5** int8 QDQ ONNX, vendored at
  `assets/model/embed.onnx` (~131 MB, tracked with git-lfs) together with the
  saliency adaptor (`saliency_adaptor.onnx`) and `tokenizer.json`. All three
  are `include_bytes!`-d into the binary at build time — runtime inference is
  fully offline, and loading cannot be declined.
- **Runtime**: ONNX Runtime via the `ort` 2.0 crate. Sessions are `&mut`-heavy,
  so each model sits behind a `Mutex`; the global
  `EMBEDDER: OnceLock<Embedder>` (`global_embedder()`) panics on load failure
  — the model is a build/runtime invariant.
- **Embedding shape**: 768-dim, **mean pooling** over the attention mask
  (nomic's intended pooling — padding tokens excluded), **L2-normalized**.
  Inputs are **dynamic-shape**: the real token count is fed to the graph (no
  fixed-size padding), and the tokenizer truncates to 2048 tokens (a
  realistic upper bound for a journal entry).
- **Saliency adaptor**: a tiny MLP mapping an embedding to an emotional-
  saliency score in [0, 1] (`predict_saliency`). It runs on the **un-prefixed**
  raw mood text embedding.
- **SQLite embedding cache** (`embedding_cache`): keyed by
  `prefix + text`; `get_or_embed_cached` embeds and stores on miss. The axes
  build, the today view, and the mood tracker all go through it; `:prune`
  clears it entirely (a cache — rows are lazily re-embedded).
- **Build-time model management (`build.rs`)**: `EMBED_MODEL` env var
  (default `nomic`) must match `assets/model/.embed_model_stamp`; on mismatch,
  or when the vendored file is below the size floor, the model is regenerated
  via `pixi run --manifest-path model/pixi.toml python model/quantize_qdq.py`
  (falling back to a bare `python3`). `build.rs` also generates
  `default_pairs()` from the bundled config's `[[moods.pairs]]` and bundles
  `assets/help.txt`.
- Vector helpers: `format_vector`, `embedding_to_blob`/`blob_to_embedding`,
  `normalize`, `dot`, `cosine_similarity`.

---

## 11. Color (color.rs) — Oklab mood projection via NNLS basis-ray regression

Config `[moods]` provides `ColorAxesSettings` (flattened; §2) plus a
`Vec<MoodEndpoint>` of basis moods, each with a target color. The pipeline:

**Build (`ColorAxes::build_async(pool, embedder, settings, pairs)`)** —
embeds the neutral base string (`base_string`, no prefix) and each pair mood
(prefix-anchored, SQLite-cached), computes the basis ray for each pair as the
L2-normalized difference from the base embedding, stores each pair's Oklab
target, and precomputes the **Gram matrix** (AᵀA) of basis-ray dot products.
The result is cached on `MoodConfig.color_axes` by the idempotent
`MoodConfig::init_with` (the only caller).

**Regression (`regression_weights(embedding, embedder, mood_text, saliency:
Option<f32>) -> Option<MoodWeights>`)** —

1. shift vector = embedding − base (L2 length ≥ ε, else `None`);
2. `at_b = Aᵀ · shift` (normalized), solved with a Lawson-Hanson NNLS
   implementation (`nnls_core`) over the precomputed Gram matrix;
3. weights filtered by `min_contribution` (share of the total), sorted
   descending, truncated to `top_k` — empty → `None` (neutral fallback);
4. power-rescaled weights (`w^steepness`, normalized) for the blend;
5. saliency: the caller-supplied override (the row's cached `feeling.score`)
   skips the ONNX saliency pass; otherwise `color::predict_saliency` runs the
   adaptor on the un-prefixed raw text (fallback 1.0).

**Blend (`weights_to_color(Option<&MoodWeights>) -> Oklab`)** — pure given a
regression result: weighted blend of the contributing basis Oklab colors,
saliency-gated (`Seff = 1 + P·(S − 1)` with P =
`effective_saliency_gate`):

```text
L = L_neutral + Seff·(L_blended − L_neutral)
a = Seff·a_blended,  b = Seff·b_blended
```

`None` maps to the neutral baseline (`(L=baseline_oklab_l, a=0, b=0)`).

**Render path (`mood_color_cached`)** — resolves a feeling row within a render
run: per-mood `HashMap<String, Oklab>` cache so repeated moods run the
pipeline once; prefers the stored embedding BLOB (re-embeds + backfills
`feeling.embedding` for legacy rows); passes `feeling.score` as the saliency
override and backfills the score when absent. Journal-only rows (empty mood)
return `None` and are never scored. `handle_color` (`:color <mood>`) runs the
pipeline once and prints every intermediate value plus a terminal swatch.

Color↔Oklab boundary conversions live in `color_conversion.rs`:
`oklab_to_crossterm` converts via the oklab crate's `to_srgb() -> Rgb<u8>`
to a crossterm `Color::Rgb`; `rgb_to_oklab` deserializes any crossterm
`Color` (named, `Rgb`, or `AnsiValue` mapped to approximate sRGB) into Oklab.

---

## 12. Editor (editor.rs)

`open_editor_for_body()`: reads `VISUAL` then `EDITOR` — **errors if neither is
set** (no silent vi fallback). Writes a `# additional notes below` header into a
`tempfile::NamedTempFile`, spawns the editor, strips the header line, and returns
the body (possibly empty, in which case entry creation proceeds only if mood is
non-empty; otherwise the entry is aborted). Integration tests set `EDITOR=true`
(exit 0, no modifications) to exercise the empty-body paths without spawning a
real editor.

---

## 13. Testing strategy

- **Unit tests** live in each module: clap parsing (`parse_from` on arg vecs,
  flag counts, scheduled-marker state machine), `apply_delta_to_counts`,
  `current_interval_start`, `enter_action` over the full task grid,
  `grid_title`, `day_label_for`, date parsing/formatting, config serde
  round-trips (including `[moods]` flatten + `deny_unknown_fields`).
- **Integration tests** (`tests/integration.rs`) exercise the **full
  CLI parse → handler → DB path**: they call `parse_from` + `handle_command`
  with a `Vec<u8>` writer and `&CliOpts::default()` (≈90 call sites) and assert
  on captured stdout content (tabular rows, markers, badges), not just exit
  success. Configs register custom trackers (`sleep`, `water`) so `handle_entry`
  accepts `-sleep 8`-style args.
- Recurring tasks are seeded via raw SQL INSERTs because the `! @` cliclack
  prompts bail on non-interactive stdin; completion updates still go through
  `handle_command`.
- Embedding-dependent paths (today view colors, `feeling.score` backfill) run
  for real: the model is bundled into the test binary, so there is no download
  gate and no CI download.
- `tui: false` is always passed, keeping TUI code out of tests.
- `db::test_pool()` gives each test an in-memory SQLite DB with the full schema
  (fresh `CREATE TABLE` per test — no migrations to worry about).
- **Short-id allocation tests** verify that completed oneshot tasks lose their
  short id (`NULL`), that ids are recycled via first-free-gap allocation, and
  that the user-facing id column is correctly cleared/reassigned on completion
  transitions.

---

## 14. Deferred / intentionally out of scope

- **`:score`** — stub (`todo!()`).
- **`:g` grid view** — parser bails "Grid view (:g) is not yet implemented".
- **CLI flags for view variants** — intentionally absent: the `@[:o|:O]` /
  `@done[:o|:O]` suffixes (`ShowVariant`) and `ctrl+d` in the TUIs cover the
  former `include_completed` / `include_scheduled` toggles; the
  `INCLUDE_COMPLETED` / `INCLUDE_SCHEDULED` env vars (previously applied in
  main.rs via `apply_envs`) were removed with them. No flags will be added.
- **No DB migrations** — schema changes are CREATE TABLE edits only; the dev DB
  (`~/.local/state/feeling/feeling.db`) must be deleted manually when the schema
  changes.
- Trackers have no TUI path; entry creation is CLI-only (no TUI input forms).
