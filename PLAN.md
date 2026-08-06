# Feeling refactor plan

## Decisions

- Use the full feature-oriented layout, with staged migrations and compile/test checkpoints.
- Breaking internal/public module paths are acceptable; no compatibility façade is required.
- Keep `src/types.rs` as the shared cross-layer contract module.
- `ViewVariant` is the shared name and lives in `src/types.rs` with the other cross-layer option/input types.
- Remove the separate `TaskType` enum and `TaskKind::Threshold`; one `TaskKind` covers oneshot, recurring, and scheduled tasks. A target count remains completion behavior, not a task kind.
- Keep the preview `today: bool` parameter for now.
- Extract shared modal payload structs, but keep Today/Tasks modal enums and state machines separate.
- Put shared task timestamp helpers in neutral task sorting code.
- Do not investigate or implement the future matchmaker UI migration in this refactor.

## Target boundaries

- `cli/`: parser and CLI grammar; shared command/input types remain in `types.rs`.
- `commands/`: command orchestration and write flows, split from parsing and output.
- `today/`: today-item model, query assembly, and today sorting.
- `tracker/`: tracker grid calculations and plain-output rendering.
- `task/`: task domain behavior and shared task sorting.
- `task_view/`: task-list-specific `ViewMode` behavior only.
- `output/`: non-TUI formatting and writing.
- `db/`: database bootstrap plus feature-oriented query modules.
- `ui/`: shared terminal lifecycle, events, actions, preview, modal payloads, and separate Today/Tasks apps.

## Migration order

1. [x] Shared type cleanup: finish `ViewVariant`, move `ViewMode`/`TodayHorizon` out of CLI/views, and update references.
2. [x] Extract `views.rs`: today model/query/sort, tracker grid, and CLI view orchestration.
3. [x] Extract `display.rs` into non-TUI output modules.
4. [x] Split `handlers.rs` into command modules.
5. [x] Split `clap.rs` and move parser modules; keep shared input/option types in `types.rs`.
6. [x] Split `sql.rs` into `db/` models and feature query modules.
7. [x] Reorganize TUI support into `ui/`, preserving separate Today/Tasks state machines, shared preview, shared modal payloads, and shared helpers.
8. [x] Complete config, task, color, embedding, percentage, and color-conversion module cleanup.
9. [x] Update `docs/ARCHITECTURE.md` and remove stale module references.
10. [x] Run final workspace diagnostics and review the resulting diff.

Final validation: `cargo fmt -- --check`, `cargo check`, `cargo test` (246 passed),
`git diff --check`, and pi-lens diagnostics. Pi-lens reports five pre-existing
non-blocking warnings in `embedding.rs` and `prompts.rs`; Cargo reports the
pre-existing dead-code warning for `IoStream::Stdout`.

## Release caveats and follow-ups

- This is an intentionally breaking module-path refactor. Downstream callers
  must migrate from paths such as `feeling::clap`, `feeling::sql`,
  `feeling::views`, and `feeling::render` to the new façades.
- `Command::Score` is still a `todo!()` path in `commands/mod.rs`; the parser
  accepts `:score`, but executing it still panics. Implement it or remove the
  command before presenting it as supported.
- `commands/update.rs` casts the user-supplied `i64` completion count to `i32`.
  Validate the range before calling the database layer to avoid wraparound.
- `db/tasks.rs::update_task` still performs completion mutation and short-ID
  synchronization without one transaction. Concurrent CLI processes could
  race; transactional mutation should be a follow-up.
- Several color-producing APIs assume `Config::moods.color_axes` was initialized
  and use `unwrap()`. Top-level command dispatch establishes that invariant, but
  direct library callers can still panic instead of receiving an error.
- Date/horizon logic still uses fixed 86,400-second arithmetic in places such as
  `types::TodayHorizon` and today day-label handling. Calendar-day arithmetic
  should be added for DST-safe behavior.
- Duration parsing/formatting still uses unchecked `u64`/`i64` conversions for
  extreme or negative values. Add range validation if those inputs need robust
  error handling.
- The removed `TaskKind::Threshold` variant does not remove target-count
  behavior. Plain output intentionally continues to label target-count oneshot
  tasks as `threshold`; this is a presentation label, not an enum kind.
- The future matchmaker UI migration remains deliberately deferred. The current
  `ui/` façade keeps the existing `Render` lifecycle and separate Today/Tasks
  applications until that migration is undertaken.

## Checkpoint policy

After each migration phase:

- `cargo fmt -- --check`
- `cargo check`
- `cargo test`
- `git diff --check`
- targeted diagnostics for edited files

Do not overwrite unrelated pre-existing working-tree changes.
