Better looking preview
Customizable tui borders
Support editing (not adding) tasks in TUI's, adding moods in Today view
subcommands:
:trackers [text|number|int|float]  list trackers
support per-tracker colors config for binning

### todo view
alt-h/? to set previewpane to display bidns
columns: configurable, default: date, name.

interactive flow should log pre-filled fields
update scheduled task creation syntax to match help (description start is marked by word beginning with :, duration start by word beginning with @)
interactive create task flows: use cliclack intro
Created task #1 instead of created oneshot task
oneshot task with target > 0: display type as 'threshold'.
don't show target if no target count.
type value in lowercase
field:    val instead of just field    val.
Don't show if quiet (need to thread CliOpts { quiet: bool, verbose: bool }) into handlers.

support the feeling "@<date>" syntax for launching Today view on arbitrary dates
(use parse_date which just deferes to parse_datetime for now).

:color: display mood config if verbose, don't show its values otherwise. 

scheduled view should toggle none -> 1 -> 0 -> none

Delete needs to work on all types in today view, and be bound to Ctrl-h on mac.
	journal entry in modal should say Delete journal entry? instead of using mood which is ''

In grid view, change the titles preceding the bodies
Tracker 'idea' (Week): , change to just idea (Week)
Tracker for recurring tasks display as @name (Week)
Tracker for mood just says Moods
These titles only display if in verbose mode, otherwise just newline (skip the first of these newlines so we don't have 2 initial newlines when not verbose)
If verbose, display the date as [format_datetime_short], with text in Darkgray

format_datetime_short new fn, -> just defer to format_datetime for now

badge for journal entries (empty mood) change from centered dot to just empty.
config.today_view.journal_badge: Option<char> (use this to get the journal badge, if not given, use empty string (no badge)).
ensure invalid timestamps (@x) fail task creation


### Drift correction
Issues: DST, variable month/year durations, time zone changes, leap seconds
@tomorrow -> maps to tomorrow same time instead of day end

### Perf
bring back ort/fastembed under a feature flag as burn turned out to be slow to start
embed cache is essentially useless since we expect most items to have embeddings. We want to cache the final colors keyed to mood string.


our approach is good at using tokens to match to weighted topics, but no good at nuance such as contrastive logic i.e. stuffed triggers hungry more than full.

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

:query for a variety of items, categories, filters, output null seperated items


# 