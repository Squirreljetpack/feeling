# NOTES — working notes for the TODO.md implementation

Progress log, decisions, and uncertainties. Updated as stages complete.

## Design decisions & interpretation risks

### Null tracker time-of-day coloring (Stage 2) — AMBIGUOUS SPEC

TODO.md: "min/max represent epoch seconds from interval start/end (endpoints
are span end)... if [r, g, b] then 1pm is red, before 23:00 is blue. The
cycle back point ... is the midpoint of (config.min.min(config.max),
config.max.max(config.max))."

The worked example is internally inconsistent (for min=23:00, max=02:00 the
"1pm is red" example and the midpoint formula cannot both hold — tried every
reading). Implemented interpretation, the only one satisfying both example
sentences *and* the midpoint formula:

- `max` is measured from the **span end**: effective `max' = span - max`
  ("24:00 - 2:00" = 22:00 in the example).
- Red zone = circular `[min, max')` (23:00 → 22:00 in the example, wrapping
  the interval boundary); blue zone = `[max', min)`.
- Cycle-back point = `(min + max') / 2`; the blue zone is split at it:
  `[mid, min)` → blue (last palette color), everything else (`[max', mid)`
  plus the wrap segment) → red (first palette color). Palette middle colors
  are unused by this mode.
- Consequences for a sleep tracker (min=23:00, max=2h): 1pm → red, 22:45 →
  blue, 22:15 → red (oddity of the formula), 00:30 → red.

If the user intended something else, it's one isolated function to change
(`null_tracker_color`).

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

## Misc observations

- Baseline: `cargo check` clean except the pre-existing `IoStream::Stdout`
  dead-code warning; 246 tests pass (per old PLAN.md; re-verified at stage 0).
- `CARGO_HOME` is `/home/dev/.dev.cargo`; had to chown it (was root-owned).
- jiff 0.2.35 is already in Cargo.lock (transitive); promote to a direct dep.
