# TODO.md implementation plan

Implements, in dependency order: jiff migration (foundation), Span-based
recurring-task intervals, tracker intervals + `TrackerKind::Null`, task↔mood
links + preview moods, and the `:db` command. Every stage ends with a
checkpoint commit (`cargo fmt -- --check`, `cargo check`, `cargo test`,
`git diff --check`).

## Stage 0 — jiff foundation (date module rewrite)

- Add `jiff` to `Cargo.toml` (0.2.35 already in the lockfile as a transitive
  dep).
- Rewrite `src/date/mod.rs` helpers (`now`, `today_start/end`,
  `week_start`/`week_monday`, `rolling_*_start`, `aligned_year_start`,
  `month_start/end`, `year_start/end`, `day_start/end`) on `jiff::Zoned`
  with the local system time zone. Public API stays `Epoch = i64`.
- `src/date/parse.rs`: keep `chrono-english` for natural-language parsing;
  convert its `DateTime<Utc>` result to a `jiff::Timestamp` via the
  SystemTime bridge given in TODO.md.
- `src/date/format.rs`: jiff strtime formatting (same output shapes:
  `%H:%M`, `%d-%m-%y`, `%Y-%m-%d %H:%M`, `M-D HH:MM`, `DD HH:MM`).
- `src/date/parse_duration.rs`: keep `parse_duration_secs` (fixed durations
  for `available_duration_secs`); add `parse_span` (calendar-aware: "1 day",
  "1 month", "1 year" → `jiff::Span`) and `format_span`.
- New `src/date/span.rs`: `DbSpan = i64`, `span_to_db` / `db_to_span`
  (verbatim from TODO.md), `zoned_from_unix_secs`, `current_interval_start_zoned`
  (verbatim), plus `interval_index(anchor, t, span)` and
  `interval_start(anchor, t, span)` helpers for slot/window math.
- `src/config/views.rs`: `grid.week_start` `chrono::Weekday` → `jiff::Weekday`.
- Update the call sites that leak chrono types (tracker.rs year heatmap).
- Checkpoint: "stage 0: jiff date foundation".

## Stage 1 — recurring-task intervals become jiff Spans

- `interval_secs` columns keep their name but now store a packed `DbSpan`
  (no migration infra; precedent: the `score` column comment says a DB
  without it is deleted by the user).
- `db/models.rs`: document `interval_secs: Option<DbSpan>`; add
  `TaskRow::interval_span()` / `TaskObject::interval_span()` helpers.
- `task/scheduling.rs`: `current_interval_start(start, span, now)` via
  `current_interval_start_zoned`; `recurring_window_end`,
  `availability_passed`, `interval_start`, `pending_sort_time` on jiff.
- Remove seconds arithmetic from SQL completion scoping:
  - `db/views.rs`: `fetch_task_by_id`-style CASE scoping → fetch rows +
    completions, compute interval-scoped sums/last_time in Rust;
    rewrite `fetch_tasks_for_view` (6 variants) with WHERE filters in SQL
    and completion filtering/scoping in Rust;
    `fetch_tasks_completed_on` same; `recurring_windows_in_period` uses
    jiff window math (`anchor + span*k`, truncation at `end_time`).
  - `task_tree.rs::load`: same de-scoping.
  - `db/tasks.rs`: `update_task` / `sync_short_id` boundaries via the new
    `current_interval_start`.
- `tracker.rs::display_recurring_tracker`: slot index via `interval_index`.
- `ui/preview.rs`: `next:` field via `interval_start + span`.
- `output/tasks.rs`: `interval` cell via `format_span`, `next_available`
  via jiff.
- `commands/task.rs` + `prompts.rs`: interval prompt/parse → `parse_span`;
  store `span_to_db(span)`; availability cap compares `dur_secs` against
  `span.total(Unit::Second)`.
- Checkpoint: "stage 1: span intervals for recurring tasks".

## Stage 2 — tracker intervals `(Epoch, Span)` + calendar slots + Null kind

- `config/trackers.rs`:
  - `TrackerKind::Null` variant (serde rename `null`).
  - `TrackerSetting.interval: Option<(Epoch, Span)>` with a custom
    deserializer for TOML `["2026-03-01 00:00", "1 day"]` (date module
    parse methods) and matching serializer.
- `config/mod.rs::init`: interval validity check against the span.
- `commands/entry.rs`:
  - `-<name>` as the final token parses as a valueless tracker
    `(name, "")` (config-free parser; the handler errors for non-Null
    kinds — parse-time "requires a value" errors move to the handler).
  - Calendar replace slot: `[current_interval_start(anchor, now, span), next)`.
  - Null handling: both min+max → insert score 0 / re-log updates the
    old entry's date only; else count semantics (increment existing
    entry's score by 1, insert with 1).
- `db/entries.rs`: `create_entry` gets an update-instead-of-insert path for
  Null interval trackers.
- `today.rs` / `badge.rs` / `tracker.rs`: Null coloring — time-of-day color
  when interval + both bounds (see NOTES.md for the spec interpretation),
  numeric-style single-bound coloring, `Color::Reset` fallback; grid view
  for Null without an interval is skipped with `bog::error`.
- `today.rs` `TodayEntry` gains the tracker interval + unscoped last entry
  time; `ui/preview.rs::build_today_preview` shows `next:` / `last:` for
  interval trackers like recurring tasks.
- Checkpoint: "stage 2: tracker intervals + null tracker kind".

## Stage 3 — task↔mood links, preview moods, sync mood color cache

- `db/mod.rs`: new `task_moods` link table (task id, feeling id, FKs
  cascade) + index.
- `types::Entry.task_links: Vec<i64>` (task short ids); parser accepts
  `-<numeric>` in entry commands as a link; `commands/entry.rs` resolves
  short ids to row ids and inserts link rows with the feeling entry
  (errors when the entry creates no feeling row).
- `color/mod.rs`: `mood_color_cached` becomes **sync** and backfill-free
  (embedding from blob or embed-on-the-fly, score from row or predict —
  no DB writes); add a process-wide `Mutex<HashMap<String, Oklab>>`
  global cache for preview use; update `today.rs` caller.
- `db/entries.rs`: `fetch_linked_moods(task_id)` → `Vec<FeelingRow>`.
- `ui/tasks.rs` + `ui/today.rs`: on selection change fetch the selected
  task's linked moods; `ui/preview.rs::build_preview` gains a
  `linked_moods` parameter and renders `moods:` + `- ● {mood}` lines
  (badge colored via the sync cache + embedder).
- Checkpoint: "stage 3: task-mood links + preview moods".

## Stage 4 — `:db` command (prune + backfill)

- `cli/mod.rs`: `Command::Prune` → `Command::Db { sub: DbSubcommand }`
  (`Prune` | `Backfill`); `cli/parse/special.rs` parses `:db prune` /
  `:db backfill` (bare `:db` → usage error).
- `commands/maintenance.rs`: `:db prune` = current prune behavior;
  `:db backfill` = compute + persist missing feeling embeddings/scores
  (the behavior `mood_color_cached` used to do inline).
- Update `assets/help.txt`, parser tests, integration tests
  (`:prune` → `:db prune`), README, docs/ARCHITECTURE.md.
- Checkpoint: "stage 4: :db command".

## Stage 5 — docs + final validation

- `docs/ARCHITECTURE.md`: interval representation (DbSpan), tracker
  interval config, Null tracker semantics, `:db` commands, preview moods.
- Full suite: fmt, check, test, `git diff --check`, lens diagnostics.
- Final commit.

## Known interpretation risks (see NOTES.md)

1. Null time-of-day coloring: user clarified 2026-08-07 — "1am is red";
   range `[min, max]` circular with binning inside, first/last by circular
   proximity outside (closer to min → last/blue, closer to max → first/red).
   Implemented in `badge::null_tracker_color`.
2. Valueless `-<name>` parses when followed by another dash token or EOL
   (config-free parser); chained valueless trackers work
   (`good -sleep -xyz -withvalue abc -null3`); `-sleep good` still consumes
   `good` as the value.
3. `interval_secs` column name retained (now packed DbSpan); existing DBs
   must be deleted (project precedent).
