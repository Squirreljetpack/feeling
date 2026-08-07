# NOTES — working notes for the TODO.md implementation

Progress log, decisions, and uncertainties. Updated as stages complete.

## Design decisions & interpretation risks

### Null tracker time-of-day coloring (Stage 2) — CORRECTED BY USER (2026-08-07)

My first implementation read the TODO example "1pm is red" literally and
derived an inconsistent spec (max measured from the span end, blue zone
`[mid, min)`). The user clarified:

> I meant **1am** is red. Within the range 23:00-02:00 circular, we do
> binning. Outside that range, if we pick first or last depends on which one
> we are closer to, circular.

Final spec (implemented in `badge::null_tracker_color`):

- `min`/`max` are **seconds from the interval start** (times of day), not
  from the span end.
- The color range is `[min, max]` circular, traversed **forward** from min
  (wrapping the interval boundary when `max < min`).
- Inside the range: **binning** by position — `min` → last palette color,
  `max` → first color (continuous with the outside rule; the later the
  entry, the redder).
- Outside the range: **first/last by circular proximity** to the nearer
  endpoint — closer to `min` → last color ("before 23:00 is blue"), closer
  to `max` → first color. This is exactly the TODO's cycle-back midpoint
  split of the outside zone (both the `(a+b)/2` and the "right side"
  formulas reduce to the circular midpoint).
- Sleep example (min=23:00, max=02:00): 01:00 → in range (binned yellow),
  22:45 → blue, 03:00 → red (closer to 02:00), 13:00 → blue (closer to
  23:00; "before 23:00 is blue" — this is why the example said 1am, not
  1pm), 12:00 → red (closer to 02:00).

### Valueless `-<name>` parsing (Stage 2) — parser is config-free

`parse_from` has no access to the config, so the parser cannot know a
tracker is `Null`. A trailing `-<name>` (last token) now parses as a
valueless tracker `(name, "")`; the *handler* rejects empty values for
Text/Number/Float trackers. Parse-time "Tracker 'x' requires a value"
errors moved to handler time; parser tests asserting the old parse-time
error were updated. Consequence: `-sleep good` for a Null tracker consumes
`good` as the value — write `good -sleep` instead (mood first, tracker last).

### `interval_secs` column keeps its name, now stores packed DbSpan (Stage 1)

No migration infra exists (`CREATE TABLE IF NOT EXISTS` only); the project
precedent (score column) is "an existing DB without the column is deleted by
the user". Renaming/adding a column would silently NULL out every recurring
task's interval in existing DBs — worse. The column value is now
`span_to_db(span)`; all readers unpack via `db_to_span`.

### Null count-mode insert uses score 1 (Stage 2)

TODO says "use 0 for score" and "increment ... by 1" only for *existing*
entries. For a fresh interval a score of 1 was chosen so the count equals
the number of logs in the interval (0 would undercount by one).

### `:db backfill` definition (Stage 4)

`mood_color_cached` used to backfill embeddings/scores inline; with the
sync/no-backfill change, `:db backfill` persists missing scores (and
missing embeddings) for feeling rows. Journal-only rows (empty mood) are
skipped, matching the old inline behavior.

## Progress log

### Stage 0 — jiff foundation ✅ (commit `stage 0: jiff date foundation`)

- `src/date/span.rs` added: `DbSpan` pack/unpack (TODO verbatim), `zoned_from_unix_secs`,
  `current_interval_start_zoned`, `interval_index`, `interval_start_unix_secs`,
  `span_rough_seconds`.
- `src/date/mod.rs` / `format.rs` / `parse.rs` rewritten on jiff (Epoch i64 API
  unchanged; chrono-english bridge per TODO). `parse_duration.rs` adds
  `parse_span`/`format_span`.
- `config.grid.week_start` → `config::Weekday` wrapper (jiff Weekday has no
  serde); case-insensitive deserialize, serializes as "Monday".
- `src/ort_compat.rs` added: `__isoc23_strtol*` shims — see "Environment" below.

### Environment (important for running tests on this machine)

The prebuilt ONNX Runtime (`ort` download-binaries) needs glibc ≥ 2.38 and
libstdc++ from GCC ≥ 13; this devcontainer is Debian 12 (glibc 2.36, GCC 12).
Two workarounds are in place:

1. `src/ort_compat.rs` provides `__isoc23_strtol`/`__isoc23_strtoll`/
   `__isoc23_strtoull` (delegating to the C99 libc fns; behaviorally
   equivalent for ONNX's JSON scanning). Harmless on newer glibc.
2. Linking needs the newer libstdc++ from `model/.pixi` (GCC 13): every
   `cargo build`/`cargo test` in this session runs with
   `RUSTFLAGS="-C link-arg=-L…/model/.pixi/envs/default/lib -C
   link-arg=-Wl,-rpath,…/model/.pixi/envs/default/lib"`. This is
   machine-specific and NOT committed (`.cargo/config.toml` is tracked).

Full suite with default features: 176 lib + 88 integration = 264 pass.
`cargo test --features load-dynamic` also links but breaks all embedding
paths at runtime (no .so), so default features + RUSTFLAGS is the standard
checkpoint command for this session.

### jiff API notes (for future stages)

- `jiff::Span` has **no `PartialEq<Span>`** — only `SpanFieldwise`
  comparisons (`assert_eq!(a.fieldwise(), b.fieldwise())`). Never `==` spans.
- `Span::new()` returns `Span`; `.years()` … `.seconds()` take `Into<i64>`
  and validate ranges (years ±19998, panic on overflow).
- `Span::total(Unit::Second)` **errors** for spans with days/weeks (and
  months) without a relative reference — hence `span_rough_seconds` (nominal
  365.25d years / 30.44d months), used by `current_interval_start_zoned` and
  `interval_index` instead of the TODO's `total().unwrap_or(86400.0)`
  (which would overflow `checked_mul` for month/year spans over long
  periods). Algorithm is otherwise verbatim.
- TODO's `db_to_span` reads months/weeks/minutes/seconds as `i8`: values
  above 127/63 wrap negative. Kept verbatim; encodable maxima are
  months ≤ 127, weeks ≤ 127, minutes ≤ 63, seconds ≤ 63.
- `Timestamp::as_second()` is the epoch-seconds accessor; `Zoned - Zoned`
  yields a seconds-only `Span` (`total(Unit::Second)` OK there).

### Stage 2 ✅ (commit `stage 2: tracker intervals + null tracker kind`)

- `TrackerSetting.interval` is now `Option<TrackerInterval>` = (anchor
  epoch, calendar Span), TOML `["2020-01-01 00:00", "1 day"]`; old string
  form rejected; bundled dev.toml/config.toml updated. `date::deserialize`
  module deleted (was only used by the old interval format).
- `TrackerKind::Null` added; valueless trailing `-<name>` parses (numeric
  names still error — reserved for task links in stage 3); handler rejects
  empty values for text/number/float.
- Null write semantics: count mode (a bound missing) increments the slot
  entry's score (insert 1); both-bounds mode inserts 0 and re-log moves
  the entry's time without touching the score. No interval → error.
- Replace slots are calendar-based (`interval_slot_unix_secs`); grid
  slotting via `interval_index` from the tracker anchor.
- Null coloring: `badge::null_tracker_color` implements the NOTES
  interpretation (red = `[min, mid)` circular, blue = `[mid, min)`, solid
  first/last palette colors); single-bound/no-interval falls back to
  numeric score binning; grid for interval-less Null is skipped with
  `bog::error`.
- Today view: Null label = tracker name only; `TodayEntry` carries
  `tracker_interval` + `tracker_last`; `build_today_preview` shows
  `next:`/`last:` for interval trackers like recurring tasks.
- `interval_index` generalized to t before the anchor (negative indices).
- Tests: null semantics integration test, TrackerInterval serde roundtrip,
  null_tracker_color unit tests. 179 lib + 89 integration pass.

### Stage 3 ✅ (commit `stage 3: task-mood links + preview moods + sync mood color cache`)

- `task_moods` link table (cascades both ways); `-<short id>` tokens in
  entry commands record links — resolved to row ids at write time, require
  a feeling entry ("Nothing to log" / explicit error otherwise).
- `ColorAxes::mood_color_cached` is now **sync and backfill-free**: rows
  without a stored embedding are embedded on the fly, rows without a score
  fall back to predicting inline, no DB writes. `:db backfill` (stage 4)
  persists those.
- Process-wide `GLOBAL_MOOD_COLOR_CACHE` (Mutex<HashMap>) for the sync
  task preview; `build_preview` gained `linked_moods` + `axes` params and
  renders `moods:` with `  - ● mood` lines. The tasks/today apps fetch the
  selected task's linked moods on selection change (async, cached per app).
- The old render-time score/embedding backfill is gone; the integration
  test that asserted it now asserts the opposite.

### Stage 4 ✅ (commit `stage 4: :db command (prune + backfill)`)

- `Command::Prune` → `Command::Db { sub: DbSubcommand }` (`Prune` |
  `Backfill`); `:db prune` = old `:prune`; `:db backfill` persists missing
  feeling embeddings + saliency scores (journal rows skipped); bare `:db`
  and unknown subcommands error with usage.

### Stage 5 ✅ (docs + final validation)

- docs/ARCHITECTURE.md rewritten for jiff/calendar intervals (DbSpan),
  tracker interval config format, Null tracker semantics, task_moods,
  `:db` commands, sync mood cache; README help block re-synced from
  help.txt; CHANGELOG entry added.
- Live smoke test (scratch XDG state): task link, Null trackers (count +
  time-marker), today view, `:db backfill`, `:db` usage error all work.

## Misc observations

- Baseline: `cargo check` clean except the pre-existing `IoStream::Stdout`
  dead-code warning; 246 tests pass (per old PLAN.md; re-verified at stage 0).
- `CARGO_HOME` is `/home/dev/.dev.cargo`; had to chown it (was root-owned).
- jiff 0.2.35 is already in Cargo.lock (transitive); promote to a direct dep.
