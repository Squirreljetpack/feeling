## [Unreleased]

## [0.1.0] - 2026-08-02

### Added

- Scheduled tasks: `! '@<time>[; description][; @<duration>][.. body]'` creates a
  scheduled task (a one-off task with an availability window). Creation happens
  immediately when the time, name and duration all come from the command line;
  otherwise the interactive flow prompts with the given values pre-filled.
- TUI tasks app: `Ctrl+a` toggles scheduled tasks into `!`/`@`/`@done`/`@due`
  (`config.tasks_view.include_scheduled` sets the startup default); `Ctrl+d`
  toggles completed tasks. Enter on a scheduled task cycles its state
  (ongoing → completed / failed); elapsed windows auto-complete.
- Today view surfaces scheduled tasks whose window overlaps the horizon
  (ongoing / completed / failed states with their own badges).
- Config: `tasks.default_scheduled_priority` (default 10).

### Changed

- `! @ [description] [.. body]` remains interactive recurring creation (the
  description now skips the name prompt; `..` carries the body).
- Removed the `@scheduled` view; the task-view footers in both TUI apps were
  dropped.

## [0.1.0] - 2026-08-01

Initial commit
