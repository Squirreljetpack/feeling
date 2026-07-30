# Task view matrix

What each view mode fetches, as a function of the `include_scheduled` and
`include_completed` flags. All task rows come from `sql::fetch_tasks_for_view`
(shared by the TUI tasks app and the CLI views); the today view uses
`views::fetch_today_entries`.

## Predicates

| shorthand | meaning |
| --- | --- |
| `O` | oneshot task: `interval_secs IS NULL AND available_duration_secs IS NULL` |
| `R` | recurring task: `interval_secs IS NOT NULL` |
| `S` | scheduled task: `interval_secs IS NULL AND available_duration_secs IS NOT NULL` |
| `ongoing(S)` | `start_time + available_duration_secs >= now` (window not yet elapsed) |
| `elapsed(S)` | `start_time + available_duration_secs < now` (window fully over) |
| `noentry` | no `todo_completions` row for the task (`completions IS NULL`) |
| `done(O)` | oneshot has a completion entry with `count >= target_count` |
| `done(R)` | recurring completed within its current interval (`count >= target_count`) |

Scheduled task state (derived, not stored): `ongoing(S) AND noentry` is
**ongoing**; `noentry AND elapsed(S)` is **auto-completed**; a completion entry
of `0` is **failed**; an entry `>= 1` is **completed** (early or on time).
A scheduled task keeps at most one completion row (entry upsert).

## `!` — OneShotTasks

| | include_completed = false | include_completed = true |
| --- | --- | --- |
| include_scheduled = **false** | incomplete `O` (`noentry OR count < target_count`) | all `O`, done or not |
| include_scheduled = **true** | incomplete `O` **+** `ongoing(S) AND noentry` | all `O` **+** `ongoing(S)` (entries or not) |

Order: `priority DESC, start_time ASC`.

## `@` — RecurringTasks

| | include_completed = false | include_completed = true |
| --- | --- | --- |
| include_scheduled = **false** | active `R` within availability window, `noentry OR count < target_count` in current interval | active `R` within availability window, incl. those done this interval |
| include_scheduled = **true** | same `R` set **+** `ongoing(S) AND noentry` | same `R` set **+** `ongoing(S)` (entries or not) |

The recurring completion sum is scoped to the current interval; scheduled rows
fall to the unscoped branch so `noentry` works for them. `R` rows are
post-filtered by `recurring_available(now)`.
Order: `priority DESC, start_time ASC`.

## `@done` — DoneTasks

`include_scheduled = true` **replaces** the view content with scheduled-only
rows (no oneshot/recurring rows).

| | include_completed = false | include_completed = true |
| --- | --- | --- |
| include_scheduled = **false** | done `O` only | done `O` + done `R` (current interval) |
| include_scheduled = **true** | `elapsed(S) AND noentry` (auto-completed) | `S` **with** a completion entry — completed (entry ≥ 1) or failed (entry 0) |

With `include_scheduled = true` the view shows resolved scheduled tasks only:
without `include_completed` the auto-completed ones (window elapsed, no
entry); with it, exactly the scheduled tasks that carry a completion entry
(completed early/on time or marked failed) — no window bound.
Order: `COALESCE(MAX(completion.time), start_time) DESC`; scheduled-only rows
order by `start_time DESC`.

## `@due` — DueTasks

| | include_completed = false | include_completed = true |
| --- | --- | --- |
| include_scheduled = **false** | `O` with `start_time <= today_end`, `noentry OR count < target_count` | all `O` with `start_time <= today_end` |
| include_scheduled = **true** | `O` **and** `S` with `start_time <= today_end` — same query, `noentry` (entries dropped by HAVING) | all `O` and `S` with `start_time <= today_end`, entries or not |

With `include_scheduled = true` the scheduled rows share the oneshot query
verbatim: `interval_secs IS NULL` already matches `S`, so enabling the flag
only drops the `available_duration_secs IS NULL` guard. The HAVING clause
(`completions IS NULL OR completions < target_count`, with `target_count` 0
for scheduled rows) keeps only entry-less rows unless `include_completed`
drops it. `@due` is where scheduled tasks stay discoverable (never orphaned),
including today's own scheduled tasks.
Order: `start_time ASC, priority DESC`.

## Today view (`feeling today`, TodayApp)

Not flag-driven — no `include_scheduled` / `include_completed` toggles (the
toggle actions are not handled in TodayApp). Always fetches, within the horizon:

1. today's feelings
2. today's custom tracker entries
3. `O` with `start_time <= horizon_end` (floor = `today_start`, or unbounded
   when `today_view.include_overdue` is set)
4. active `R` (availability-filtered)
5. today's todo completions
6. **scheduled tasks overlapping the horizon**:
   `start_time < horizon_end AND start_time + available_duration_secs > today_start`
   (window overlap; floor fixed at `today_start` for all horizons, upper bound
   scales: `today_end` / `day_end(today + 1d)` / `day_end(week_sunday)`). All
   states shown — ongoing, auto-completed, completed, failed — with detail
   labels `scheduled` / `done` / `overdue`.

Rows sorted chronologically.

## Who drives the flags

- **CLI views** (`feeling @`, `feeling @done`, `feeling @due`, …): both flags
  are hardcoded `false` — scheduled tasks are invisible in CLI lists; they only
  surface in `feeling today` (always) and via the interactive creation flow.
- **TUI tasks app**: `include_completed` starts from the command (always
  `false` from the CLI), toggled with `ctrl+d` (`Action::ToggleCompleted`);
  `include_scheduled` starts from `command.include_scheduled || config.tasks_view.include_scheduled`
  (config default `false`), toggled with `ctrl+a` (`Action::ToggleScheduled`).
  Both trigger a refetch.
