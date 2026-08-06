# Feeling refactor plan

## Decisions

- Use the full feature-oriented layout, with staged migrations and compile/test checkpoints.
- Breaking internal/public module paths are acceptable; no compatibility façade is required.
- Keep `src/types.rs` as the shared cross-layer contract module.
- `ShowVariant` becomes `ViewVariant` and lives in `src/types.rs` with the other shared option/input types.
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

1. Shared type cleanup: finish `ViewVariant`, move `ViewMode`/`TodayHorizon` out of CLI/views as appropriate, and update references.
2. Extract `views.rs`: today model/query/sort, tracker grid, and CLI view orchestration.
3. Extract `display.rs` into non-TUI output modules.
4. Split `handlers.rs` into command modules.
5. Split `clap.rs` and move `types.rs` input definitions only after shared types are stable.
6. Split `sql.rs` into `db/` models and feature query modules.
7. Reorganize TUI support into `ui/`, preserving separate Today/Tasks state machines, shared preview, shared modal payloads, and shared helpers.
8. Complete config, task, color, embedding, and percentage module cleanup.
9. Update `docs/ARCHITECTURE.md` and remove stale module references.

## Checkpoint policy

After each migration phase:

- `cargo fmt -- --check`
- `cargo check`
- `cargo test`
- `git diff --check`
- targeted diagnostics for edited files

Do not overwrite unrelated pre-existing working-tree changes.
