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




bring back ort/fastembed under a feature flag as burn turned out to be slow to start

embed cache is essentially useless since we expect most items to have embeddings. We want to cache the final colors keyed to mood string.

# Gradations
Mood color presets that can be selected on cli i.e. -[1-9]
describe how to achieve a linear blend on an axis

### Notetaking
store body files in folder, named by mood/time-mood_summary-disambiguating_number.md or task/time-name_slug
	mood_summary: use our model to do some kind of summarization somehow or allow configuring a set of categories.
recommend using fs with rg to search through all notes.