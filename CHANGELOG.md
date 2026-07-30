## [Unreleased]

### Fixed

- Mood colors: with a high `blend_steepness` (e.g. 10) the raw weight sum
  `Σ|t_i - 0.5|^p` underflows below the absolute `1e-6` "all neutral"
  threshold even when an axis is strongly polarized, so the blend wrongly
  fell back to an equal mix of every axis (muddy grey). The all-neutral
  check is now relative to the largest delta, keeping the dominant axis in
  charge at any steepness.

### Changed

- Embeddings: sentence vectors are now **CLS pooled** — the hidden state of
  the first token (`model_output[:, 0]`) — instead of mean pooling. That is
  bge-small-en-v1.5's intended usage (BAAI model card; Cloudflare's serving
  notes also recommend `cls` pooling for better accuracy), and CLS vectors are
  not compatible with mean-pooled ones. Axis directions are re-derived from
  the new embeddings at startup, so every mood color shifts.

- Mood colors: the `blending` strategy enum (`centroid` / `sigmoid`) is gone;
  the blend curve is now a single `moods.blend_steepness` knob (default 2.0,
  clamped to `>= 1.0`). `w_i = |t_i - 0.5|^p`: `1.0` reproduces the old
  linear `centroid` behavior, higher values snap toward the dominant axis
  (large `p` ≈ winner-take-all).
- Mood colors: new `moods.polarization_steepness` knob (default 1.0 =
  identity, clamped to `>= 1.0`). Each axis's blend factor is remapped to
  `t' = t^q / (t^q + (1 - t)^q)` before the per-axis Oklab lerp, so a
  strongly polarized mood renders close to its dominant endpoint color
  instead of drifting toward the desaturated middle of the axis (a linear
  lerp between chromatically-opposite endpoints like magenta ↔ cyan loses
  half its chroma at just `t = 0.3`). `:color` now prints the polarized
  factor per axis.

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
