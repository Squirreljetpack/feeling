# feeling

`feeling` is a CLI tool for tracking moods, journaling, custom metrics, and managing oneshot and recurring tasks.

## Features

- Mood & Journal Logging: Log moods and journal entries, browse them together with due tasks like an interactive daily note.
- Grid view: Display your mood history as dots colored by mapping your recorded moods along configurable semantic axes throughout various timespans and watch as interesting patterns emerge.
- Task Management: Support for oneshot and recurring tasks with flexible priority, interval, availability duration, and target completions.
- Custom Trackers: Track custom metrics with configurable intervals, ranges and colors.
- Interactive TUI & Views: View today's summary

## Usage

<!-- HELP_START -->
```
feeling — mood, journal, and task tracker

Usage:
  feeling <mood> [.. [body]]                          log a mood (with an optional body)
  feeling [-tracker <value>]...                       add one or more custom tracker records
    [mood] [-tracker <value>]... [.. [body]]            (optionally, linked to a new mood entry)

  feeling ! [-<parent_id>]                            create a oneshot task (interactive)
  feeling ! [-<parent_id>] <name> [@<time>]           create a oneshot task
    [.. [body]]                                         (optionally, linked to a parent)
  feeling ! @ [name]                                  create a recurring task (interactive)
  feeling ! @<time> [:name]                           create a scheduled task
    [%duration] [.. [body]]                             (interactive unless all 3 fields are specified)

  feeling - <query words> [count]                     update completion of the unique task
                                                        whose name contains <query words> in their order
  feeling - <id> [count]                              update task completion by id

Views:
  feeling [@date]                                     today view
  feeling @due[:t|:w]                                 due view
                                                        (today / tomorrow / this week)
  feeling @[:o|:O]                                    pending tasks
                                                        (all / oneshot / recurring+scheduled)
  feeling @done[:o|:O]                                completed tasks

Trackers and grids:
  feeling :[week|month|year] [ids]                    dot-sequence tracker grid
                                                        ids: <tracker> or @<recurring-name>
                                                        period defaults to "week"
  feeling :embed                                      embed stdin lines (one vector/line)
  feeling :score "start" "end"                        score stdin vectors onto an axis
  feeling :color <feeling>                            projected mood color diagnostic

Other:
  feeling :config | :c                                open the config in $VISUAL / $EDITOR
  feeling :moods                                      open the moods config file
  feeling :clear [@date]                              clear all mood entries from a day
  feeling :prune                                      delete completed and expired tasks
  feeling -q | -v <command>                           quiet / verbose; flags go first
  feeling --help | -h                                 show this help
```
<!-- HELP_END -->

## Installation

##### Homebrew

```sh
brew install Squirreljetpack/tap/feeling
```

##### AUR

Unavailable

##### npm

```sh
npm install -g @squirreljetpack/feeling
```

## Configuration

Run `feeling :config` to open the configuration file in your `$VISUAL` or `$EDITOR`. If no configuration exists, default settings will be written to `~/.config/feeling/config.toml`.

## See also

Inspired by <https://github.com/qiz-li/feeling>
