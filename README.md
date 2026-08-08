# im

`im` is a CLI tool for tracking moods, journaling, custom metrics, and managing oneshot and recurring tasks.

## Features

- Mood & Journal Logging: Log moods and journal entries, browse them together with due tasks like an interactive daily note.
- Grid view: Display your mood history as dots colored by mapping your recorded moods along configurable semantic axes throughout various timespans and watch as interesting patterns emerge.
- Task Management: Support for oneshot and recurring tasks with flexible priority, interval, availability duration, and target completions.
- Custom Trackers: Track custom metrics with configurable intervals, ranges and colors.
- Interactive TUI & Views: View today's summary

## Usage

<!-- HELP_START -->
```
im — mood, journal, and task tracker

Usage:
  im <mood> [.. [body]]                          log a mood (with an optional body)
  im [-tracker <value>]...                       add one or more custom tracker records
    [mood] [-tracker <value>]...                        (optionally linked to a new mood entry)

  im ! [-<parent_id>] [.. body]                  create a oneshot task (interactive)
  im ! <name> [@<time>]                          create a oneshot task
  im ! @ [name]                                  create a recurring task (interactive)
  im ! @<time> [:name] [%duration]               create a scheduled task 
                                                        (interactive if partial)

  All the previous subcommands support a trailing [.. [body]].
  If only .. is specified, `$EDITOR` will open for writing the body of the entry.

  Oneshot tasks can be optionally linked to a parent (i.e. subtasks)
  by writing the parent's id prefixed with `-` in the first argument.
    A bare - allows you to pick the parent interactively.

  im - <query words> [count]                     update completion of the unique task
                                                        whose name contains <query words>
                                                        in their order
  im - <id> [count]                              update task completion by id


Views:
  im [@date]                                     today view
  im @due[:t|:w]                                 due view
                                                        (today / tomorrow / this week)
  im @[:o|:O]                                    pending tasks
                                                        (all / oneshot / recurring+scheduled)
  im @done[:o|:O]                                completed tasks


Trackers and grids:
  im :[week|month|year] [ids]                    dot-sequence tracker grid
                                                        ids: <tracker> or @<recurring-name>
                                                        period defaults to "week"

Other:
  im :config | :c                                open the config in $VISUAL / $EDITOR
  im :moods                                      open the moods config file
  im :embed                                      embed stdin lines (one vector/line)
  im :color <mood>                            projected mood color diagnostic
  im :clear [@date]                              clear all mood entries from a day
  im :db prune                                   delete completed and expired tasks
  im :db backfill                                compute and persist missing mood embeddings
  im :db doctor                                  check tracker entries vs kinds; prune mismatches

Flags:
  im -q | -v <command>                           quiet / verbose; flags go first
  im --help | -h                                 show this help
```
<!-- HELP_END -->

## Installation

##### Homebrew

```sh
brew install Squirreljetpack/tap/im
```

##### AUR

Unavailable

##### npm

```sh
npm install -g @squirreljetpack/im
```

## Configuration

Run `im :config` to open the configuration file in your `$VISUAL` or `$EDITOR`.

The default locations are in order:

- `~/.config/matchmaker/config.toml` (If the folder exists already).
- `{PLATFORM_SPECIFIC_CONFIG_DIRECTORY}/matchmaker` (Generally the same as above when on linux)

## FAQ

### What is the difference between `im` and `im-dynamic`?

- **`im`** (default): Statically links ONNX Runtime (`ort`) at build time. It is a self-contained binary with no external library dependencies.
- **`im-dynamic`**: Dynamically loads the ONNX Runtime shared library (`libonnxruntime`) at runtime. Use this variant if you prefer linking against a system-installed or custom-built ONNX Runtime.

### How do I specify the library path for `im-dynamic`?

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

- https://github.com/qiz-li/im
- https://docs.rs/jiff/latest/jiff/
