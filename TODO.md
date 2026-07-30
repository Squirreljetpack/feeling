Better looking preview
Customizable tui borders
Support editing (not adding) tasks in TUI's, adding moods in Today view
subcommands:
:trackers [text|number|int|float]  list trackers
support per-tracker colors config for binning

### todo view
alt-h/? to set previewpane to display bidns
columns: configurable, default: date, name.

### Perf

Easily modify axis to color by on cli for cool exploration of different mood values. Probably, something like "s1" "s2" [mood_axis_color_index]

bring back ort/fastembed under a feature flag as burn turned out to be slow to start

embed cache is essentially useless since we expect most items to have embeddings. We want to cache the final colors keyed to mood string.

