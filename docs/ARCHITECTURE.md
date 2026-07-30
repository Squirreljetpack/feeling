# feeling — Architecture

A CLI + TUI journaling and task-tracking tool. Mood entries (optionally journal
bodies), custom numeric trackers, oneshot and recurring tasks, all stored in a
single SQLite database. Terminal output is tab-separated and parseable; the same
views run as a fullscreen ratatui TUI when stdout is a TTY.

---

this project is in progress and we don't need to worry about migrations or
breaking changes

removed features are treated as if they never existed: no tests, no mentions
in this document, no changelog rows

when running tests, use --test-threads=4

## 1. Crate layout

The crate is a **library + thin binary** so that integration tests can import it
as `feeling::`.

```
src/
  lib.rs        re-exports every module
  main.rs       thin binary: init flow + dispatch (the only TTY/TUI decision point)
  paths.rs      XDG-style paths (config dir, state dir, db, log)
  config.rs     serde Config (config.toml) incl. color schemes and custom trackers
  logger.rs     cba `bog`/env_logger setup + log file
  db.rs         SQLite pool, CREATE TABLE schema, indexes, PRAGMA foreign_keys
  date/         all chrono/humantime usage lives here
    mod.rs      Epoch type + now/today/week/month/year helpers
    parse.rs    datetime string parsing via chrono-english → epoch
    parse_duration.rs  duration string parsing via humantime → seconds
    format.rs   duration/time/datetime formatting
  clap.rs       manual CLI parser → Command enum (no clap crate)
  handlers.rs   command dispatch + all write paths
  prompts.rs    all interactive prompts (priority, target count, interval,
                end time, optional, name, clear confirm)
  sql.rs        all SQL queries and data-access types (the only module
                that contains sqlx query calls)
  display.rs    plain stdout output (tab-aligned field rows, no cliclack,
                no interactive branching)
  task.rs       shared completion logic (deltas, interval boundaries, done-ness)
  views.rs      non-TTY view output (tasks, today, trackers) + fetch queries
  action.rs     unified Action enum emitted by event loop and consumed by TUI apps
  binds.rs      crokey key combinations -> Action bindings (default_binds)
  message.rs    RenderEvent (Action, Resize) and ControlEvent (Pause, Resume) channels
  event_loop.rs async crossterm input stream parser & action emitter
  embed.rs      all-MiniLM-L6-v2 ONNX sentence embeddings via Burn (burn-onnx)
  color.rs      Oklab mood-color projection from embeddings
  editor.rs     `..` body editor (VISUAL/EDITOR)
  tui.rs        thin matchmaker-style fullscreen terminal wrapper
  render/       ratatui rendering
    mod.rs      Render trait & shared TUI lifecycle runner
    system.rs   TUI external editor suspension helper (pause/resume event loop)
    tasks.rs    task-list TUI (`!`, `@`, `@done`, `@due`) via TasksApp
    today.rs    today TUI (Today/Tomorrow/Week horizons) via TodayApp
    preview.rs  task & today entry preview rendering helpers
    utils.rs    priority colors, mode labels, string truncation
```

### main.rs flow

```
bog::init_bogger
config  = load_type_or_default(default_config_path(), toml)     // cba
         if config.task_color.colors.is_empty() -> exit(1)      // see §8
init_logger
cmd     = parse_args()                                          // feeling::clap
pool    = db::init_database(database_path())
out     = io::stdout()
tui     = atty::is(Stdout)                                      // ← the single TUI decision point
handle_command(cmd, &pool, &config, &mut out, tui)
```

`handle_command` takes an explicit `tui: bool` — the TUI never auto-launches from
inside the library, so integration tests always pass `false` and exercise the
plain view output.

### User-facing logging — `cba::ebog!`/`wbog!`/`ibog!` (print to stdout)

All errors, warnings, and notifications the user should see in the terminal are
emitted through the cba bogger macros (printed to stdout by `bog::init_bogger`
in §1). Use `ebog!`, `wbog!`, `ibog!` (and `nbog!`/`dbog!`/`mbog!`/`cbog!` as
appropriate) with the typed form `MACRO!("tag"; "message")` when the error
category is worth surfacing:

```rust
cba::ebog!("config";   "task_color.colors must not be empty in config.toml");
cba::wbog!("embed";    "Embedding unavailable: {err:#}");
cba::ibog!("embed";    "Downloading embedding model '{}' (first use; ~22MB)...", MODEL_REPO);
cba::ibog!("test";     "skipping test_score_utility: model not downloaded");
```

**Do not** use `log::warn!` / `log::error!` / `log::info!` / `eprintln!` for
user-facing output — `init_logger` in this crate pipes the standard env_logger
to the **log file only** (`Target::Pipe`), so those calls are invisible in the
terminal. Only the bogger reaches the terminal. `log::debug!` is fine (and
stays as-is — those are for the log file / internal debugging, not the
user).

The `__ebog` extension on `Result` from `cba::bog::BogOkExt` is the standard
handler-level exit-on-error helper (e.g. `parse_args().__ebog()`).

---

## 2. Paths & config

**paths.rs** — `BINARY_FULL = "feeling"`. `FEELING_CONFIG_DIR` env var overrides
the config dir when it exists. Config path: `~/.config/feeling/dev.toml` in debug
builds, `config.toml` in release (cba `expr_as_path_fn`). State dir:
`dirs::state_dir()/feeling` (fallback `~/.local/state/feeling`). DB at
`state_dir/feeling.db`, log at `state_dir/feeling.log`.

**config.rs** — `Config` (serde, all fields `#[serde(default)]`):

```toml
[mood_color]               # Oklab axes defined by endpoint mood+color pairs
axes = [
  [{ mood = "happy",      color = "#15F76F" }, { mood = "sad",         color = "#18389B" }],
  [{ mood = "drained",    color = "#495057" }, { mood = "charged",     color = "#FF3D00" }],
  [{ mood = "passive",    color = "#9370DB" }, { mood = "purposeful",  color = "#FFC107" }],
]

[task_color]
colors = ["DarkRed", "DarkYellow", "DarkGreen"]   # crossterm Color, deserialized w/ serde feature

[custom.sleep]        # registered custom trackers (unknown types are rejected)
interval = "1 day"    # humantime duration via cba as_option
kind = "float"        # payload type: text | number | float (default: text)
max = 10
min = 0               # min/max apply to number and float; ignored for text

[tasks]
default_priority = 5  # valid range: 1..=999 (cliclack validation enforces)

[grid]
week_rolling = false               # true = full Mon..Sun week (7 dots); false = week_start..today
month_rolling = true               # true = rolling last 4 weeks ending today (subrepo compat); false = month_start..today
year_rolling = true                # true = calendar year (Jan 1..today); false = subrepo rolling 52-week window aligned to a full week start (no leading blanks)
week_start = "monday"              # day each week starts on (Mon .. Sun); sets both week grid and rolling month alignment
[tasks_view]          # reserved, no fields yet
[today_view]
include_overdue = false   # show tasks due before today in the today view
```

`MoodColorConfig` accepts a `Vec<ColorAxis>` (instead of three fixed named
axes) with each axis carrying both endpoint mood strings and endpoint
`Color` values (hex `#RRGGBB`, `rgb_(r,g,b)`, or named crossterm colors via
the crossterm serde feature). It also carries a `color_axes:
Option<ColorAxes>` field with `serde(skip)` — the fully built projection
axes (per-axis Oklab endpoint colors, direction vectors, and rescaling
bounds) populated by `MoodColorConfig::init_with(embedder)` so subsequent
color projections skip re-embedding.

Custom tracker payloads: `text` stores a string (`-accomplishment "fixed 2 bugs"`),
`number` an integer (min/max apply), `float` a decimal (min/max apply). Values
are parsed against the declared kind at write time with a clear error on
mismatch (e.g. a float tracker given a non-numeric argument).

---

## 3. Database schema

`db.rs` runs `CREATE TABLE IF NOT EXISTS` + indexes (`run_migrations`). **There
are no ALTER migrations** — the project is treated as fresh; schema changes mean
deleting the dev DB. `PRAGMA foreign_keys = ON` in both `init_database` and
`test_pool`.

**PRAGMA journal_mode**: `WAL` in release builds; `DELETE` in debug builds
(to avoid leaving `-wal`/`-shm` sidecar files in the state dir during
dev work). Setting `DELETE` explicitly also converts a pre-existing
WAL-mode db file, so the mode is deterministic regardless of the
database's prior state.

```
feeling:  id, mood TEXT NOT NULL, body TEXT NOT NULL DEFAULT '', time INTEGER (unixepoch), embedding BLOB
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
```

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

- **Datetime parsing (`date/parse.rs`)**: `parse_datetime(s: &str, dialect: DateDialect) -> Result<Epoch>` uses `chrono-english` (`chrono_english::parse_date_string`) with `chrono::Local::now()` as the anchor. The dialect (`DateDialect::Uk` day-first, default, or `DateDialect::Us` month-first) comes from the `[date] dialect` config setting; it only matters for ambiguous slash forms like `3/5/2024`. It handles both natural language expressions (e.g. `"yesterday"`, `"tomorrow 9am"`, `"3 days ago"`) and fixed format strings (e.g. `"2024-03-15"`, `"2024-03-15 14:30:00"`), returning epoch seconds directly.
- **Duration parsing (`date/parse_duration.rs`)**: `parse_duration_secs(s: &str) -> Result<i64>` uses `humantime` (`humantime::parse_duration`) to parse human-readable durations (e.g. `"1 day"`, `"2 hours"`, `"1d"`, `"2h"`), returning total seconds as an `i64`.

---

## 5. CLI parsing (clap.rs)

Manual parser, no clap crate. `parse_args()` (from `env::args`) and
`parse_from(Vec<String>)` (unit-testable). Dispatches on `args[0]`:

| Input | Command |
| --- | --- |
| (no args) | `Today` — today view (`feeling` with no subcommand) |
| `--help` / `-h` | `Help` — bundled `assets/help.txt` printed via `include_str!`; recognized only in the **initial position** by `parse_cli` (never `parse_from`) |
| plain words (`happy`, `good ...`) | `Entry { feeling, customs, .. }` — mood entry; custom trackers as `-type score` |
| `..` (only as **last** arg) | opens the body editor (Entry/Task with `open_editor`) |
| `!` | `View { mode: OneShotTasks, include_completed: false }` |
| `! description [@date] [..]` | `Task { OneShot, .. }` — `@YYYY-MM-DD` due date |
| `! @` | interactive recurring creation (cliclack; bails when stdin is not a TTY) |
| `@` / `@done` / `@due` / `@scheduled` | `View { mode, include_completed: false }` (`@scheduled` is a stub, deferred) |
| `- id [count]` | `Update { OneShot(id), count }` — `id` is the user-facing short id; `count` may be negative. Completed tasks have no short id and are not addressable (see §3) |
| `- words… [count]` | `Update { Query(words), count }` — the unique oneshot task whose name contains the words in order (subsequence match); a trailing numeric arg is the count |
| `:` / `:week\|month\|year` / `:` + ids | `Tracker { period, ids }` — dot-sequence tracker views (`:g` bails "not yet implemented") |
| `:embed` | `Embed` — stdin lines → one 384-dim vector per line |
| `:score "start" "end"` | `Score` — stdin vectors → axis score |
| `:config` | `Config` — opens the live `Config::default()`-seeded config in `$VISUAL`/`$EDITOR` |
| `-` alone | `TasksEdit` — stub: `handle_tasks_edit` bails "not yet implemented" |

`View.include_completed` controls whether completed recurring tasks appear in
`@`/`@done`. It **always parses to `false`** today (no CLI flag yet); the field
exists so a future flag can turn it on. Both the TUI and CLI paths honor the same
flag, so they never diverge.

Custom tracker names cannot begin with `@` (reserved for recurring ids).
Tabs in mood/name fields are an error (output is tab-separated); whitespace in
recurring task names is allowed (the `@` prefix disambiguates).

---

## 6. Handlers (handlers.rs)

`handle_command<W: Write>(cmd, pool, config, out, tui)` matches the `Command`
enum:

- **Entry** → `handle_entry`: runs in a transaction; inserts `feeling` (+ optional
  `custom` rows with `feeling_id`). Each `-type value` is parsed against the
  tracker's declared kind (text/number/float) with a clear error on mismatch
  (e.g. a float tracker given a non-numeric argument); min/max apply to
  number/float, unknown tracker types are rejected. Insertion strategy is
  kind × interval: `text`/`float` trackers **with an interval** keep one entry
  per interval slot — re-logging the same tracker inside the same slot
  (slot = `time / interval_secs`, matched by `type`) replaces the previous
  entry; `number` trackers and interval-less trackers are plain inserts that
  accumulate (the views sum per-slot scores). Mood/body are plain `String`
  (empty = absent); a wholly empty entry is aborted ("Nothing to log"). When
  mood is non-empty, the mood is embedded **before** the transaction opens
  (`embed::global_embedder()` — the model is bundled into the binary, so loading
  cannot be declined; a per-text embedding failure stores no embedding rather
  than losing the entry).
- **Task** → `handle_task`: oneshot insert (due date `@YYYY-MM-DD` parsed to
  `start_time`); recurring creation via cliclack interactive prompts
  (interval, start time, available duration, target count, end time, optional).
  The prompts bail with an error when stdin is not interactive. Recurring
  creation stores `start_time = date::now()` as the recurrence start so
  interval boundaries are computable.
- **Update** → `handle_update`: `target_count` handling and per-target messaging;
  applies the delta via `task::apply_completion_delta` (interval-aware for
  recurring). The `- <id>` form looks up the task by user-facing short id
  (see §3); completed tasks have no short id and are not addressable here
  (use the word query form instead). The message reflects the returned
  (interval-scoped) total.
- **TasksEdit** → `handle_tasks_edit`: stub that bails "Task editing is not
  yet implemented" (interactive task editing is future work, see TODO.md).
- **Today** → TUI (`TodayApp::new(pool, config).run()`) when `tui`, else
  `views::handle_today`.
- **View** → TUI (`TasksApp::new(pool, mode, config, include_completed).run()`)
  when `tui`, else `views::handle_view(pool, mode, config, include_completed, out)`.
- **Tracker** → `views::handle_tracker` (no TUI path yet).
- **Embed/Score** → `handle_embed`/`handle_score`: read stdin line by line
  (blank lines skipped), write to `out`.

`main.rs` supplies `out = io::stdout()`; every view/today/tracker function takes
`&mut W: Write`. Entry/task/update **confirmation** messages stay `println!`
(interactive confirmations, not view output).

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
the interval and next_available columns. `pri` is the numeric priority (1..=999).
The status column is the **completion badge** (§8). For target-counted tasks
the badge text is just `"●"` (no `DONE` word anywhere); in-progress tasks
get `"● m/n"` on the same line. (The TUI table in `render/tasks.rs` has no interval/next columns — the
recurring interval/availability live in the TUI preview, and status moves
into the `name` cell.)

Fetching (`fetch_tasks_for_view(pool, mode, include_completed)`) — the
**interval-scoped GROUP BY pattern** used by every TaskRow query:

```sql
SELECT t.*, SUM(tc.count) AS completions [, MAX(tc.time) AS last_time]
FROM todos t
LEFT JOIN todo_completions tc ON tc.todo_id = t.id
    AND (t.interval_secs IS NULL OR t.start_time IS NULL
         OR tc.time >= t.start_time + ((? - t.start_time) / t.interval_secs) * t.interval_secs)
GROUP BY t.id
HAVING ...
```

- The JOIN condition **scopes recurring completions to the current interval**
  (boundary = `start_time + floor((now - start_time)/interval) * interval`;
  SQLite integer division truncates, which equals floor when `now >= start_time`).
  Oneshot tasks (`interval_secs IS NULL`) keep all-time sums. A NULL `start_time`
  falls back to all-time — matching `apply_completion_delta`'s no-boundary rule.
  This makes both `SUM(tc.count)` and `MAX(tc.time)` naturally interval-scoped.
- `GROUP BY t.id` with `t.*` is deterministic (all columns functionally depend on
  the PK); SQLite resolves aliases in `HAVING`/`ORDER BY`.
- The `?` is bound to `date::now()` **before** the WHERE binds (JOIN ON precedes
  WHERE). The same fragment is inlined in views.rs, render/tasks.rs and
  render/today.rs (no shared const — queries are intentionally duplicated).

Per-mode behavior (all governed by `include_completed`, always `false` today):

| mode | filter |
| --- | --- |
| `@` (RecurringTasks) | `interval_secs IS NOT NULL`, `end_time > now`, `HAVING completions < target_count` (not done in the **current** interval), then a Rust availability-window check on `available_duration_secs` |
| `@done` (DoneTasks) | `include_completed=false`: **oneshot only** (`interval_secs IS NULL`) with `HAVING completions >= target_count`, ordered by `COALESCE(MAX(tc.time), t.start_time) DESC`. `true`: done oneshot + recurring tasks done in their current interval and inside their window (`start_time <= now < end_time`) |
| `@due` (DueTasks) | oneshot only, `start_time <= today_end` |
| `!` (OneShotTasks) | oneshot, not done |

Single-row fetches (e.g. TUI preview refresh) use a correlated `(SELECT SUM(count)
FROM todo_completions ...)` subquery with the same boundary condition instead of
GROUP BY.

### Today view — `format_today_simple`

`ts \t marker \t label \t detail` rows aggregated from feelings, custom entries,
due oneshot tasks, active recurring tasks (recurring availability filter:
`(now - start_time) mod interval < available_duration`), and todo-completion
events. Each entry carries its marker glyph (`TodayEntry.badge`) and a dynamic
dot color (`TodayEntry.color`, type `ratatui::Color`) computed at fetch time in
`fetch_today_entries`. `·` journal-only or text custom + neutral dark gray;
`●` feeling + Oklab mood projection (`color.rs`) when the model is ready,
dark gray otherwise; `◆` numeric custom + `bin_score_color` (§7) binned against
the tracker's `min`/`max`; `○` task + `completion_badge` color (§8) based on
`SUM(todo_completions.count)` from the `TaskRow` query (so today's UI reflects
the live completion count of each task); `✓` completion event + the last
`task_color.colors` (success/finished). The shared `views::TEXT_ENTRY_BADGE`
glyph is a named constant so the text marker can be adjusted later. The oneshot
query has two orthogonal bounds: an upper bound at the horizon end
(`start_time <= horizon_end`) and — unless `config.today_view.include_overdue`
— a lower bound keeping only tasks due from today onward (tasks due strictly
before today are overdue and hidden). Recurring tasks are only included when
their availability window covers `now`. Empty day → `Nothing logged today.`

The TUI and the non-TTY text rows both consume `TodayEntry.color` directly — no
static-glyph coloring fallback — so each row's dot reflects the same
completion/color logic the CLI table applies. (Dynamic values, same `color`
field on every entry, no render-time fallback function.) In the text output,
`format_today_simple` wraps the marker glyph in the entry's ANSI color
(`ratatui::backend::IntoCrossterm` + crossterm `Stylize`), skipping color when
`color == Reset` (e.g. 0% tasks) so parseable tab-separated rows don't carry
no-op escape sequences.

### Trackers — `: [week|month|year] [ids]`

Dot-sequence history grids, colored via config. All grids cover the **full
period** (Mon..Sun week, whole month, whole year) — not just up to today —
and wrap at 7 dots per row with two-space spacing (the last row may be short).
`handle_tracker` threads the CLI's `tui` flag down as `interactive` so grid
paths can prompt.

- **Mood tracker** (`display_mood_tracker`): per-day dot colored by the Oklab
  mood projection (§11), preferring stored `feeling.embedding` blobs over
  on-the-fly embedding. Days without an entry render the empty centered dot
  `◯`; days with an entry render `●` (colored when possible, plain otherwise).
  Calls `embed::global_embedder()` via `Config.mood_color.init_with` first — the
  model is bundled, so loading always succeeds or panics; the grid never falls
  back to plain dots for a missing model. Queries filter `mood != ''` so plain mood entries
  with no body still count; journal-only entries are excluded.
- **Custom tracker** (`display_custom_tracker`): per-interval dots binned with
  `bin_score_color` (score vs `min`/`max`, inverted ranges handled; no
  blending) for `number`/`float` kinds; `text` trackers list each entry as a
  dark-gray `> text` line instead of dots (interval ignored).
- **Recurring task tracker** (`display_recurring_tracker`): per-interval dots
  from the per-interval completion sum via the shared `completion_badge` (§8) —
  a zero-sum interval renders `◯`, sum ≥ target renders the last bin color, in
  between bins over `colors[..len-1]`.

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
is the deliberate exception. The TUI converts the crossterm color via ratatui's
`FromCrossterm` trait (`Color::from_crossterm`, not `From`, in ratatui 0.30).
The today-view timeline markers are fixed glyphs and intentionally unchanged.

`task_color.colors` is **guaranteed nonempty** under all runtime paths —
`main.rs` exits with an error if the loaded `Config` has an empty palette,
and the default `Config::default()` populates it (3 entry dark-red / dark-yellow
/ dark-green bins) — so all binning/indexing code can assume index 0 and
index `len-1` exist (`bin_score_color`, `completion_badge`, the completion
event marker today-view color, etc.).

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
  (`Up`, `Down`, `Accept`, `Edit`, `Delete(bool)`, `CycleMode`, `ToggleSort`,
  `Refresh`, `Quit`, `Ack`, `Input(char)`). Both `TasksApp` and `TodayApp` match on
  every variant and ignore those that don't apply to their context.
- **Key bindings (`binds.rs`)**: `default_binds()` uses `crokey::KeyCombination` to
  map key combinations (keys + modifiers) to `Action`s. Unbound keys fall through
  to `Action::Input(char)` for modal text fields.
- **Event loop (`event_loop.rs`)**: runs in a dedicated tokio task reading
  crossterm's `EventStream`. Key presses map to `Action`s and emit `RenderEvent`s
  (`Action` or `Resize`) over an unbounded `mpsc` channel. Redraws only happen when an
  event arrives (no tick loop).
- **Process suspension & control events (`message.rs`, `render/system.rs`)**:
  When an external editor is launched (`Ctrl+e`), `edit_with_editor` sends
  `ControlEvent::Pause` to `EventLoop` and awaits `Action::Ack`. The event loop
  drops its `EventStream` so keystrokes are not stolen from the editor.
  `tui.enter_execute()` yields the terminal. On exit, `tui.return_execute()`
  restores terminal state, sends `ControlEvent::Resume`, awaits `Action::Ack`, and
  resumes input reading.

### `Render` trait & app lifecycle (`render/mod.rs`)

`pub trait Render` provides the shared TUI runner loop for both `TasksApp` and `TodayApp`:

- `render(&self, f: &mut Frame)`: draw current app state.
- `async fn handle_action(...)`: handle action state updates and modal routing.
- `should_quit(&self) -> bool`: exit condition.
- `async fn run(&mut self) -> Result<()>`: default lifecycle method — enters fullscreen,
  spawns `EventLoop`, draws on every `RenderEvent`, dispatches actions, and exits cleanly.

### Item Editing & Modal Action Routing (`Action::Edit` & `Action::Accept`)

The behavior of `Action::Edit` (`Ctrl+e` / `e`) and `Action::Accept` (`Enter`) depends on the selected item type:

| Item Type | Action / Trigger | Behavior |
| --- | --- | --- |
| **Tasks** (oneshot / recurring) | `Action::Edit` | Opens external editor (`$VISUAL`/`$EDITOR`) pre-filled with task `body`. Suspends event loop via `ControlEvent::Pause`, updates `todos.body` on save. |
| **Tasks with `target_count > 0`** | `Action::Accept` (`Enter`) | Opens `CompleteModal` prompt to read numeric completion count delta (`+n` or `-n`). Applies delta to `todo_completions`. (Tasks with `target_count = 0` complete with `+1` directly). |
| **Mood entries** (`feeling`) | `Action::Edit` | Opens external editor pre-filled with entry `body`. Suspends event loop via `ControlEvent::Pause`, updates `feeling.body` on save. |
| **Custom tracker** (`text`) | `Action::Edit` | Opens external editor pre-filled with text payload. Suspends event loop via `ControlEvent::Pause`, updates `custom.score` on save. |
| **Custom tracker** (`number` / `float`) | `Action::Edit` | Opens in-TUI `EditTrackerModal` prompt to edit numeric score in-place. Validates input against kind (`i64`/`f64`) on `Enter`, updates `custom.score`. |
| **Completion logs / other** | `Action::Edit` | No-op (completions are immutable history). |

### TUI Applications

- **`TasksApp` (`render/tasks.rs`) — task-list App** (`!`, `@`, `@done`, `@due`):
  - Fields: `pool`, `tasks`, `selected`, `mode`, `config`, `include_completed`, `should_quit`, `sort_by_due`, `modal`.
  - Keys: `j`/`k` nav, `Tab` mode cycle, `Ctrl+s` sort toggle, `Enter` complete / modal, `ctrl-e` edit body, `Del`/`Backspace` delete, `q`/`Esc` quit.
  - Modals: `CompleteModal` (target_count completion delta), `DeleteConfirm`, `ResetConfirm` (@done progress reset).
  - Table renders `short_id / pri / name` with badge and `m/n` sub-line;
    completed tasks show no id. Preview pane (`render/preview.rs`) shows
    detailed task fields.
- **`TodayApp` (`render/today.rs`) — Today App** (`-`):
  - Fields: `pool`, `config`, `entries`, `selected`, `horizon`, `sort_by_priority`, `should_quit`, `modal`, `selected_task`.
  - Cycles horizons (Today → Tomorrow → Week) via `Tab`; handles mood/task body edits, custom tracker edits (`EditTrackerModal` for numeric, external editor for text), and deletion. Text wrapping is enabled in the preview pane (`Wrap { trim: false }`).

---

## 10. Embeddings (embed.rs)

- Model: **`xenova/bge-small-en-v1.5`** ONNX model (384-dim embeddings, 256-token
  truncation). `build.rs` uses `burn_onnx::ModelGen` to compile the ONNX weights
  into native Rust code inside `OUT_DIR/model/bge_small.rs` at build time.
- **Embedder**: Powered by the Burn framework (`burn`, `burn-flex` backend). Inputs `input_ids`/
  `attention_mask`/`token_type_ids` drive `bge_small::Model<Backend>`. Sentence
  embeddings are produced via **CLS pooling** (bge's intended usage — the
  hidden state of the first token, `model_output[:, 0]`, per the BAAI model
  card) + **L2 normalization**.
- Weights & tokenizer: `build.rs` runs `scripts/quantize_bge_qdq.py` to
  produce a static INT8 QDQ graph (~32 MB) in `target/cache` (covered by the
  `/target` gitignore) and compiles it into the binary via `LoadStrategy::Embedded`;
  the tokenizer is `include_bytes!`-d. The dynamic-quantized ONNX variants need
  `DynamicQuantizeLinear`, which burn-onnx 0.21 cannot compile. Runtime inference
  is fully offline.
- **`embed::global_embedder() -> &'static Embedder`**: lazy, one-time load of the
  bundled model behind a `OnceLock`. Loading cannot be deferred or declined, so
  a failure panics rather than degrading silently.
- Vector helpers: `format_vector`, `parse_vector`, `normalize`, `dot`. `handle_embed`
  and `handle_score` provide line-by-line CLI vector embedding and scoring.

---

## 11. Color (color.rs) — Oklab mood projection, directly projected

Config `mood_color.axes` is a `Vec<ColorAxis>` (the old `lightness` /
`green_red` / `blue_yellow` triple is gone). Each `ColorAxis` carries two
`MoodEndpoint { mood, color }` pairs with `color` accepting `#RRGGBB`, named
crossterm colors, or `rgb_(r,g,b)`. The defaults are the three contrast pairs
from TODO.md (green/blue, dark gray/orange, muted purple/yellow).

For each axis the direction vector is `normalize(embed(end) - embed(start))`
in 384-dim MiniLM space. The pipeline projects directly onto each axis
vector — no Gram-matrix decorrelation (matrix inversion distorted semantic
axis meanings and caused sign flips), the soft-max blend handles overlap
instead. Three steps:

1. **Raw projections, rescaled to `[0, 1]` blend factors**. For each axis
   `r_i = d · v_i` (raw dot product), then rescale through the axis's own
   endpoint bounds: `t_i = ((r_i - proj_start_i) / (proj_end_i -
   proj_start_i))` clamped to `[0, 1]`, where `proj_start` / `proj_end` are
   the endpoint embeddings' own dot products against the direction vector.
   This anchors `proj_start → 0` (start color), `proj_end → 1` (end color),
   midpoint → `0.5`. (The TODO also lists a sigmoid variant `(s_i - μ_i) /
   σ_i`; we don't have empirical μ/σ, so the linear form is used.)

2. **Perceptual per-axis interpolation + power-weighted blend**.
   For each axis, the blend factor is first polarized toward its dominant
   endpoint via `t'_i = t_i^q / (t_i^q + (1 - t_i)^q)` (`q` from
   `mood_color.polarization_steepness`, default 1.0 = identity, clamped to
   `>= 1.0`), then the endpoint colors are lerped linearly in Oklab:
   `color_i = lerp_oklab(start_oklab, end_oklab, t'_i)`. Polarization keeps a
   strongly polarized axis close to its own endpoint color — the linear lerp
   between chromatically-opposite endpoints (e.g. magenta ↔ cyan) loses most
   of its chroma at small `t`, which rendered strong moods muddy. The mix
   weights are `w_i = |t_i - 0.5|^p` (computed from the *un*polarized
   factors) normalised to sum 1.0, where the exponent `p` comes
   from `mood_color.blend_steepness` (default 2.0, clamped to `>= 1.0`):
   - `p = 1`: linear blend — every axis contributes in proportion to its
     polarity (this matches the old `centroid` strategy, which is gone).
   - `p = 2` (default): power-weighted — vibrant primary colors with clear
     hybrid blends; each axis's dominant endpoint "wins" more decisively.
   - `p = 4+`: snaps aggressively toward the most-polarized axis; a very
     large `p` is effectively winner-take-all on the axis with the largest
     `|t_i - 0.5|`.
   Because user-defined axes are rarely orthogonal, raw projections bleed
   between them — the `p` exponent acts as a non-linear soft-max that
   naturally suppresses weak, overlapping secondary axes instead of
   cancelling them via decorrelation.
   For `polarization_steepness` (`q`): `q = 1` is the identity (the
   historical linear per-axis lerp), `q = 2`–`4` noticeably saturates
   dominant moods, and `q = 10` renders a 70/30 projection essentially as
   the dominant endpoint color. The neutral midpoint `t = 0.5` is a fixed
   point for every `q`.
   The all-neutral fallback (every `t_i = 0.5`) returns the equal-weight
   mean of all axis midpoints.
   See `sigmoid_blend_oklab` in `src/color.rs` for the exact formula.

`ColorAxes` carries the N axes plus the blend `steepness` and polarization
`q` (cached on `MoodColorConfig.color_axes` by `init_with` so each build
skips re-embedding). `mood_color(axes, embedder, mood)` caches per-mood
embeddings per call; the mood tracker prefers stored blobs
(`blob_to_embedding` + `axes.project`) over on-the-fly embedding.
`average_color` averages a day's mood Oklab coordinates via the pure
`average_oklab(&[Oklab]) -> Option<Oklab>` (component-wise mean, in Oklab
space — never in sRGB, to preserve the perceptual midpoint).

Color↔Oklab boundary conversions live in `color_conversion.rs`:
`oklab_to_crossterm` converts via the oklab crate's `to_srgb() -> Rgb<u8>`
to a crossterm `Color::Rgb`. `rgb_to_oklab` deserializes any crossterm
`Color` (named, `Rgb`, or `AnsiValue` mapped to approximate sRGB) into
Oklab.

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

- **Unit tests** live in each module: clap parsing (`parse_from` on arg vecs),
  `apply_delta_to_counts`, `current_interval_start`, embedding blob round-trips,
  `average_oklab`, color projection with synthetic orthonormal axes (no
  embedder), date parsing.
- **Integration tests** (`tests/integration.rs`) exercise the **full
  CLI parse → handler → DB path**: they call `parse_from` + `handle_command`
  with a `Vec<u8>`/`Cursor` writer and assert on captured stdout content (tabular
  rows, markers, badges), not just exit success. Configs register custom trackers
  (`sleep`, `water`) so `handle_entry` accepts `-sleep 8`-style args.
- Recurring tasks are seeded via raw SQL INSERTs because the `! @` cliclack
  prompts bail on non-interactive stdin; completion updates still go through
  `handle_command`.
- The embedding-model tests (`:embed`, `:score`) always run: the model is bundled
  into the test binary, so there is no download gate and no CI download.
- `tui: false` is always passed, keeping TUI code out of tests.
- `db::test_pool()` gives each test an in-memory SQLite DB with the full schema.
- **Short-id allocation tests** (`test_short_id_allocator_*`): verify that
  completed oneshot tasks lose their short id (`NULL`), that ids are recycled
  via first-free-gap allocation, and that the user-facing id column is
  correctly cleared/reassigned on completion transitions.

---

## 14. Deferred / intentionally out of scope

- **`@scheduled`** — stub, `TODO: ignore for now` (TODO.md).
- **`:g` grid view** — parser bails "Grid view (:g) is not yet implemented".
- **`include_completed`** — always `false`; no CLI flag to set it yet (by design,
  the plumbing is in place).
- **No DB migrations** — schema changes are CREATE TABLE edits only; the dev DB
  (`~/.local/state/feeling/feeling.db`) must be deleted manually when the schema
  changes.
- Trackers have no TUI path; entry creation is CLI-only (no TUI input forms).
