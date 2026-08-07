

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

# linking tasks and moods
add a table linking tasks to (mood) entries. Read help.txt, currently we allow [-tracker v].. mood [-tracker], want to also allow [-id] in place of tracker where numeric means it refers to a short id of a task. This is not a completion, simply adds an entry in the link table (short id resolved to actual task id). In make_preview, if any linked moods, display a field moods:, below '  - {mood badge} {mood text}', use mood_color_cached with a mutexed global hashmap.

- change mood_color_cached to be sync (no backfill)
- change :prune to :db, with subcommands :db prune, :backfill
note that embedding loading should be lazy with a startup queue, render badge method checks color cache instead of coloring at injection.

# grid
show grid by last completion time
changeable tracker types: what to do?
db :doctor -- clear all invalid entries, feeling :config should auto-call
 How coloring happens on dots without min/max again?
:query for a variety of items, categories, filters, output null seperated items

Add a TrackerKind::Null:
	in cli parsing where we expect trackers, null tracker doesn't consume a next token.

- change interval for trackers to be calendar based.
- null entry: min/max from interval start/end. direction is always forward. colors can be adjusted so min/max reversal is unnecessary. No payload.
- specifying min/max reverts it to count based: payload is count. new entries increment. Accept: applies delta instead of set.

If no interval is specified, acts as a tag ig. Grid view, leave as todo.


# Lowpri
config value, confirm before accepting scheduled
interactive todo creation requires opening editor, a bit odd but not sure how to signal to enable easily.
time-sliced grid views.
CliError instead of anyhow error, so that we can return Handled without logging the error
support attaching additional mood/journal entries to Moods, display timestamped in preview (carry extra id column)
subcommands:
:trackers [text|number|int|float]  list trackers
support per-tracker colors config for binning
more rigorous parse_from flow, i.e. validate_name before handle_command
Syncing: todo_completions is easy to reconcile, task/mood edits we could just track last edit time and require (field level) confirmation for different fields, with Y being on newer if data diverged.