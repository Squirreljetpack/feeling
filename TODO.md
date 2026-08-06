

### tui
Matchmaker:
alt-h/? to set previewpane to display binds
columns: configurable, default: date, name.
Customizable tui borders
Support editing (not adding) tasks in TUI's, adding moods in Today view

### Drift correction
Issues: DST, variable month/year durations, time zone changes, leap seconds
@tomorrow -> maps to tomorrow same time instead of day end

### Perf
bring burn under a feature flag
tested nli: no good but what's better than embeddings?
	- even with high accuracy, averaged and ordinary moods get muddied... solutions? top-2? what's top?

# Gradations
Mood color presets that can be selected on cli i.e. -[1-9]
i would prefer syntax like feeling :colors 1, then show all anchors for confirmation, but we have nowhere good to store it: sqlite seems kind of strange.
Out of scope for now. i'm not too adverse just writing to a theme_index file in state directory.
probably the full mood config struct should be a vec actually if we do this.
example: a preset for a linear blend along an interesting axis.

whats causing the startup delay?


### Notetaking
store body files in folder, named by mood/time-mood_summary-disambiguating_number.md or task/time-name_slug
	mood_summary: use our model to do some kind of summarization somehow or allow configuring a set of categories.
recommend using fs with rg to search through all notes.


# Lowpri
config value, confirm before accepting scheduled
interactive todo creation requires opening editor, a bit odd but not sure how to signal to enable easily.
:query for a variety of items, categories, filters, output null seperated items
time-sliced grid views.
CliError instead of anyhow error, so that we can return Handled without logging the error
support attaching additional mood/journal entries to Moods, display timestamped in preview (carry extra id column)
subcommands:
:trackers [text|number|int|float]  list trackers
support per-tracker colors config for binning