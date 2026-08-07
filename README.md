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
    [mood] [-tracker <value>]...                        (optionally linked to a new mood entry)

  feeling ! [-<parent_id>] [.. body]                  create a oneshot task (interactive)
  feeling ! <name> [@<time>]                          create a oneshot task
  feeling ! @ [name]                                  create a recurring task (interactive)
  feeling ! @<time> [:name] [%duration]               create a scheduled task (interactive
                                                        unless all fields are filled)

  All creation subcommands above support a trailing body [.. [body]].
  If the body is empty, and creation is interactive or .. is given,
  the `$EDITOR` will open for writing the body.

  Oneshot tasks can be optionally linked to a parent (i.e. subtasks)
  by writing the parent's id prefixed with `-` in the first argument.
    A bare - allows you to pick the parent interactively.

  feeling - <query words> [count]                     update completion of the unique task
                                                        whose name contains <query words>
                                                        in their order
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

Other:
  feeling :config | :c                                open the config in $VISUAL / $EDITOR
  feeling :moods                                      open the moods config file
  feeling :embed                                      embed stdin lines (one vector/line)
  feeling :color <feeling>                            projected mood color diagnostic
  feeling :clear [@date]                              clear all mood entries from a day
  feeling :db prune                                   delete completed and expired tasks
  feeling :db backfill                                compute and persist missing mood embeddings
  feeling :db doctor                                  check tracker entries vs kinds; prune mismatches

Flags:
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

Run `feeling :config` to open the configuration file in your `$VISUAL` or `$EDITOR`.

The default locations are in order:

- `~/.config/matchmaker/config.toml` (If the folder exists already).
- `{PLATFORM_SPECIFIC_CONFIG_DIRECTORY}/matchmaker` (Generally the same as above when on linux)

## FAQ

### What is the difference between `feeling` and `feeling-dynamic`?

- **`feeling`** (default): Statically links ONNX Runtime (`ort`) at build time. It is a self-contained binary with no external library dependencies.
- **`feeling-dynamic`**: Dynamically loads the ONNX Runtime shared library (`libonnxruntime`) at runtime. Use this variant if you prefer linking against a system-installed or custom-built ONNX Runtime.

### How do I specify the library path for `feeling-dynamic`?

Set the `ORT_DYLIB_PATH` environment variable to point to your `libonnxruntime` shared library:

```sh
# macOS
export ORT_DYLIB_PATH=/opt/homebrew/lib/libonnxruntime.dylib

# Linux
export ORT_DYLIB_PATH=/usr/local/lib/libonnxruntime.so

# Windows (PowerShell)
$env:ORT_DYLIB_PATH = "C:\path\to\onnxruntime.dll"
```

## See also

- https://github.com/qiz-li/feeling
- https://docs.rs/jiff/latest/jiff/
