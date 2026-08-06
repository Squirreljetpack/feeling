# VIEWS.md — ShowVariant view system

The `ShowVariant` enum controls which subset of tasks a view displays. `All`
shows everything relevant to the view; `A` is the oneshot-only subset; `B` is
the non-oneshot subset (recurring + scheduled), showing more of these where
applicable.

## Predicates

`now` = current Unix timestamp; `t` = a task row; `entry` = a completion
record in `todo_completions`. Shorthand used in the matrices below:

| shorthand | meaning |
| --- | --- |
| `O` / `R` / `S` | oneshot / recurring / scheduled (from `interval_secs` + `available_duration_secs`) |
| `done(t)` | `O`: `completions >= target` (target 0: any entry) · `R`: reached target in current interval · `S`: entry ≥ 1 or auto-completed |
| `ongoing(S)` | no entry, window still open |
| `failed(S)` | has an entry with count 0 |
| `auto_completed(S)` | no entry, window elapsed |
| `expired(R)` | `end_time` set, `now > end_time`, not done |
| `has_entry(t)` | any completion row exists |
| `availability_passed(t)` | window end `<= now` — recurring: current-interval-anchored; scheduled: absolute (`start + duration`) |

Completion sums are scoped to the current interval for recurring tasks; the
`@done` history view uses an unscoped sum instead. Availability-window checks
must first exclude `expired(t)` tasks — an expired task has no current
interval.

## CLI syntax

| Command | Effect |
| --- | --- |
| `feeling !` | Interactive oneshot creation (name prompted) |
| `feeling @` | Pending view — `ShowVariant::All` |
| `feeling @:o` | Pending view — `ShowVariant::A` (oneshots only) |
| `feeling @:O` | Pending view — `ShowVariant::B` (recurring not availability-filtered + scheduled) |
| `feeling @done` | Completed tasks — `ShowVariant::All` |
| `feeling @done:o` | Completed oneshots only — `ShowVariant::A` |
| `feeling @done:O` | Completed recurring history + completed scheduled — `ShowVariant::B` |
| `feeling @due` | TodayView, `ShowVariant::B`, `TodayHorizon::Today` |
| `feeling @due:t` | Tomorrow view, `ShowVariant::B`, `TodayHorizon::Tomorrow` |
| `feeling @due:w` | Week view, `ShowVariant::B`, `TodayHorizon::Week` |
| `feeling @<date>` | Anchored TodayView, `ShowVariant::All`, `TodayHorizon::Today` |

The variant suffix is `o` (A) or `O` (B) — there is no `a` suffix, so
`@:a` / `@done:a` are invalid. Starting in `ShowVariant::A` is only possible
via the `:o` suffix.

## View matrix

### `@` — Pending view

| Variant | Behavior |
| --- | --- |
| `All` | `not done(O)` + `R` (interval-scoped, availability-filtered, not expired) + `ongoing(S)` |
| `A` | `not done(O)` only |
| `B` | `not done(R)` (any not expired, not just availability-filtered) + `! availability_passed(S)` |

all of these also include + any task (all/oneshot_only/not_oneshot_only) with a completion entry within the last `persist_pending_seconds`

Non-complete scheduled tasks in `All` are exactly `ongoing(S)` — failed,
auto-completed and completed `S` are excluded.

### `@done` — Completed tasks

| Variant | Behavior |
| --- | --- |
| `All` | `done(O)` + `S` `has_entry` + `done(R)` in current interval |
| `A` | `done(O)` only |
| `B` | (ALL `R`) + `S` `has_entry` or `auto_completed` |

`@done:b` shows more scheduled tasks than `All` — it adds auto-completed `S`
and every recurring task (never-completed rows included).
Order: done time, newest first — the last completion entry; entry-less
rows fall back per kind: auto-completed `S` to
`start_time + available_duration_secs`, zero-entry `R` history rows to
`start_time` (their `available_duration_secs` is the availability window,
not a completion moment).

### `@due` / `@<date>` — TodayView

Note: the two spellings use different defaults — `@due` starts at
`ShowVariant::B` with the day horizon, `@<date>` at `ShowVariant::All`.

| Variant | Behavior |
| --- | --- |
| `All` | All tasks/trackers/mood sections for the day (oneshots, recurring, scheduled, completed today) |
| `A` | Same but completed tasks filtered out; done rows dropped from regular task lists |
| `B` | Tasks only — no trackers, no mood sections; otherwise the same as `All` (completed tasks and completion-today rows included) |

## Who sets the variant

- **CLI**: `@` / `@done` start at `All`, with the `:o` / `:O` suffixes;
  `@due[:t|:w]` is fixed at `B`; `@<date>` at `All`.
- **TUI**: `ctrl+d` cycles `All → A → B → All`, starting from the command's
  suffix. The tasks app cycles modes with Tab; the today app cycles horizons
  (`Today → Tomorrow → Week`).

## Appendix: formal definitions

All terms below are used throughout this document. `now` = current Unix
timestamp; `t` = a task row; `entry` = a completion record in
`todo_completions`.

```pseudocode
interval_start(t) =
    if t.interval_secs != null and t.start_time != null:
        max(t.start_time, t.start_time + ((now - t.start_time) / t.interval_secs) * t.interval_secs)
    else:
        t.start_time

interval_end(t) =
    if t.interval_secs != null:
        interval_start(t) + t.interval_secs
    else:
        null

is_in_interval(t):
    return interval_start(t) <= now < interval_end(t)

---

done(t):
    // Recurring: reached target in current interval
    // Scheduled: has any completion entry (entry >= 1) or auto-completed
    // Oneshot/Threshold: completions >= target_count (target 0: any entry)

completed(t):
    // Has at least one completion entry (entry count >= 1)
    return completions(t) >= 1

failed(t):
    // Has a completion entry with count 0 (window closed, never done)
    return completions(t) == 0 and has_entry(t)

auto_completed(t):
    // Scheduled task: no entry, but availability window has elapsed
    return has_no_entry(t) and t.interval_secs is null
        and t.available_duration_secs is not null
        and t.start_time + t.available_duration_secs <= now

ongoing(t):
    // Scheduled task: no entry, window still open
    return has_no_entry(t) and t.interval_secs is null
        and t.available_duration_secs is not null
        and t.start_time + t.available_duration_secs > now

expired(t):
    // Recurring task: end_time set and now past end_time,
    // not done in current interval
    return t.end_time is not null and now > t.end_time
        and not done(t)

partial(t):
    // Recurring: has some completions but not yet at target
    return completions(t) > 0 and completions(t) < target_count(t)
        and t.interval_secs is not null

has_entry(t):
    return exists tc in todo_completions where tc.todo_id = t.id

optional(t):
    // Task's optional flag (t.optional != 0)
    return t.optional != 0

has_no_entry(t):
    return not has_entry(t)

window_elapsed(t):
    return t.available_duration_secs is not null
        and t.start_time + t.available_duration_secs <= now

completions(t):
    // Interval-scoped sum for recurring; unscoped sum for done-view
    // (determined by the query variant, not this definition)
    return SUM(tc.count) over completion entries for task t

unscoped_completions(t):
    // Sum over ALL completion entries ever (no interval filter)
    return SUM(tc.count) over all completion entries for task t

availability_passed(t):
    // Window end is anchored to the current interval for recurring tasks
    // (start_time is the chain origin and never advances) and absolute for
    // scheduled tasks.
    return t.available_duration_secs is not null
        and (if t.interval_secs is not null
             then current_interval_start(t, now) + t.available_duration_secs
             else t.start_time + t.available_duration_secs) <= now

---

Note: any function that checks whether `now` is inside a task's availability
window (e.g. `window_elapsed`, `availability_passed`, `recurring_available` in
sql.rs) must first filter out `expired(t)` tasks — an expired task has no
current interval, so the availability-window check does not apply to it.

is_recurring(t):
    return t.interval_secs is not null

is_scheduled(t):
    return t.interval_secs is null
        and t.available_duration_secs is not null

is_oneshot(t):
    return t.interval_secs is null
        and t.available_duration_secs is null

is_overdue(t):
    return t.end_time is not null and now > t.end_time
    and not done(t)

is_due(t):
    return t.end_time is not null and now <= t.end_time
    and not done(t)
```
