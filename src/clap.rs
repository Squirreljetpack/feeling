use anyhow::Context;
use std::env::args;

use crate::types::{Entry, Task};
use crate::views::TodayHorizon;

/// Characters reserved as leading flags: `-q` (quiet) and `-v` (verbose),
/// accepted in the *initial* position of any command line as single chars
/// or combined (`feeling -q ok`, `feeling -qv !`, …). Config init drops
/// trackers whose names consist solely of these letters (e.g. `q`, `v`,
/// `qv`) so a `-tracker` token is never ambiguous with a flag.
pub const FLAG_CHARACTERS: &str = "qv";

/// Counts of the leading `-q` / `-v` flag characters. `qv[0]` = number of
/// `q` chars, `qv[1]` = number of `v` chars (combined tokens like `-qv`
/// count once each). Order is not tracked — the logger and handlers only
/// care about presence/counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CliOpts {
    pub qv: [u8; 2],
}

impl CliOpts {
    pub fn quiet(&self) -> bool {
        self.qv[0] > 0
    }
    pub fn verbose(&self) -> bool {
        self.qv[1] > 0
    }
    /// `-vv`-gated output (e.g. the WP7 grid period suffix).
    pub fn verbose_level(&self) -> u8 {
        self.qv[1]
    }
}

/// A parsed command line: the flags given in the initial position (`-q` /
/// `-v`, as counts) plus the command they apply to. The flags drive log
/// verbosity in `main.rs` and quiet/verbose output in the handlers;
/// `cmd` is what `handle_command` dispatches on.
#[derive(Debug, Clone, PartialEq)]
pub struct Cli {
    pub opts: CliOpts,
    pub cmd: Command,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Entry(Entry),
    View {
        mode: ViewMode,
        show: ShowVariant,
    },
    Tracker {
        period: TrackerPeriod,
        items: Vec<TrackerItem>,
    },
    Task(Task),
    Update {
        target: UpdateTarget,
        count: Option<i64>,
    },
    Embed,
    Score {
        start: String,
        end: String,
    },
    /// `feeling` with no args — today view; `feeling @<date>` anchors it to
    /// an arbitrary day (any date string that parses); `feeling @due[:t|:w]`
    /// opens the today view at `ShowVariant::B` with the day/tomorrow/week
    /// horizon. `feeling -` (bare) is TasksEdit.
    Today {
        date: Option<String>,
        show: ShowVariant,
        horizon: TodayHorizon,
    },
    /// `feeling -` (bare) — tasks-edit entry point. The handler is a stub
    /// for now: `handle_tasks_edit` bails "not yet implemented" (interactive
    /// task editing is future work, see TODO.md).
    TasksEdit,
    /// `feeling --help` / `feeling -h` in the initial position (handled in
    /// `parse_cli`, before the command dispatchers — `parse_from` never sees
    /// a help token). Handlers print the contents of `assets/help.txt`.
    Help,
    /// `feeling :config` — handlers open the active config file in
    /// $VISUAL/$EDITOR via [`crate::editor::open_editor_at`]. The bundled
    /// `assets/config.toml` is copied to the path first when missing.
    Config,
    /// `feeling :prune` — handlers delete completed oneshot tasks and
    /// recurring tasks whose `end_time` has passed.
    Prune,
    /// `feeling :color <feeling>` — embed a mood string (with `"feeling "`
    /// prefix) and print the projected Oklab / sRGB color plus intermediate
    /// pipeline values (raw scores, blend factors, per-axis colors).
    /// Diagnostic tool for debugging the mood-color pipeline.
    Color {
        mood: String,
    },
    /// `feeling :clear [@date]` — clear all mood entries from that day.
    Clear {
        date: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum UpdateTarget {
    /// `feeling - <id> [count]` — the oneshot task with that user-facing
    /// short id. Completed tasks have no short id and are not addressable.
    OneShot(i64),
    /// `feeling - <words…> [count]` — the task whose name contains all
    /// `words` in order (whitespace-separated subsequence match). The
    /// handler requires the match to be unique.
    Query { words: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// The pending view (`@`): not-done oneshots + recurring + scheduled.
    PendingTasks,
    /// The completed view (`@done`).
    DoneTasks,
}

/// Which subset of tasks a view displays (see `docs/VIEWS.md`): `All` shows
/// everything relevant to the view, `A` is the oneshot-only subset, `B` is
/// the non-oneshot subset (recurring + scheduled), showing more of these
/// where applicable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShowVariant {
    #[default]
    All,
    A,
    B,
}

impl ShowVariant {
    /// Short suffix label used in TUI titles: `[show: a|b|all]`.
    pub fn label(&self) -> &'static str {
        match self {
            ShowVariant::All => "all",
            ShowVariant::A => "a",
            ShowVariant::B => "b",
        }
    }

    /// Cycle order: All → A → B → All (D5).
    pub fn next(&self) -> Self {
        match self {
            ShowVariant::All => ShowVariant::A,
            ShowVariant::A => ShowVariant::B,
            ShowVariant::B => ShowVariant::All,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerPeriod {
    Week,
    Month,
    Year,
}

/// One item in a `feeling :` display list. `Mood` is a positional marker
/// (a bare `:` token in the args) that renders the mood grid at that spot;
/// `Tracker(name)` renders that tracker's grid (`@name` for recurring).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackerItem {
    Mood,
    Tracker(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskType {
    OneShot,
    Recurring,
    Scheduled,
}

/// Parse the full command line from `env::args` (skipping argv[0]) into a
/// [`Cli`]: leading `-q` / `-v` flags are stripped into `opts`, the rest is
/// parsed as a [`Command`].
pub fn parse_args() -> anyhow::Result<Cli> {
    let raw: Vec<String> = args().skip(1).collect();
    parse_cli(raw)
}

/// Parse flags + command from a pre-collected argument list. Used by tests.
pub fn parse_cli(args: Vec<String>) -> anyhow::Result<Cli> {
    // Flags are only recognized in the initial position: once a non-flag
    // token shows up, everything after it is the command's own arguments
    // (so `feeling ok -q` treats `-q` as entry text, not a flag). A flag
    // token is `-` followed by flag characters only (`-q`, `-v`, `-qv`, …);
    // each character increments the matching count in `opts.qv`.
    let mut opts = CliOpts::default();
    let mut rest: Vec<String> = Vec::new();

    let mut in_flags = true;
    for arg in args {
        if in_flags {
            // `-h` / `--help` are only recognized in the initial position
            // and short-circuit to Help before any dispatching prefix (so a
            // help token is never re-read as a tracker name or command).
            // After a non-flag token, `-h` is entry text like any other
            // `-word`.
            if arg == "-h" || arg == "--help" {
                return Ok(Cli {
                    opts,
                    cmd: Command::Help,
                });
            }
            match arg.strip_prefix('-') {
                Some(s) if !s.is_empty() && s.chars().all(|c| FLAG_CHARACTERS.contains(c)) => {
                    for c in s.chars() {
                        match c {
                            'q' => opts.qv[0] += 1,
                            'v' => opts.qv[1] += 1,
                            _ => unreachable!(), // all() guard above
                        }
                    }
                    continue; // stays in_flags
                }
                _ => in_flags = false,
            }
        }
        rest.push(arg);
    }

    Ok(Cli {
        opts,
        cmd: parse_from(rest)?,
    })
}

/// Parse a command from a pre-collected argument list (flags already
/// stripped). Used by tests and internally by [`parse_cli`].
pub fn parse_from(args: Vec<String>) -> anyhow::Result<Command> {
    // No args → Today view (bare `feeling`). Help is handled one level up in
    // parse_cli (`-h` / `--help`, initial position only) — parse_from treats
    // a `-h`-style token as entry text.
    if args.is_empty() {
        return Ok(Command::Today {
            date: None,
            show: ShowVariant::All,
            horizon: TodayHorizon::Today,
        });
    }

    let first = &args[0];

    // Special commands starting with ':'
    if first.starts_with(':') {
        return parse_special_command(&args);
    }

    // Task commands starting with '!'
    if first.starts_with('!') {
        return parse_task_command(&args[1..]);
    }

    // View commands starting with '@'
    if first.starts_with('@') {
        return parse_view_command(&args);
    }

    // Tasks edit ('-') or update ('- <id> / - <words…>')
    if first == "-" {
        return parse_dash_command(&args[1..]);
    }

    // Otherwise, it's an entry command
    parse_entry_command(&args)
}

fn parse_special_command(args: &[String]) -> anyhow::Result<Command> {
    let first = &args[0];

    if first == ":embed" {
        return Ok(Command::Embed);
    }

    if first == ":score" {
        if args.len() < 3 {
            anyhow::bail!("Usage: feeling :score \"start\" \"end\"");
        }
        return Ok(Command::Score {
            start: args[1].trim_matches('"').to_string(),
            end: args[2].trim_matches('"').to_string(),
        });
    }

    if first == ":config" || first == ":c" {
        if args.len() != 1 {
            anyhow::bail!("Usage: feeling :config");
        }
        return Ok(Command::Config);
    }

    if first == ":prune" {
        if args.len() != 1 {
            anyhow::bail!("Usage: feeling :prune");
        }
        return Ok(Command::Prune);
    }

    if first == ":color" {
        if args.len() < 2 {
            anyhow::bail!("Usage: feeling :color \"<feeling>\"");
        }
        return Ok(Command::Color {
            mood: args[1..].join(" ").trim_matches('"').to_string(),
        });
    }

    if first == ":clear" {
        let date_arg = if args.len() > 1 {
            let joined = args[1..].join(" ");
            let trimmed = joined.trim();
            let stripped = trimmed.strip_prefix('@').unwrap_or(trimmed).trim();
            if stripped.is_empty() {
                None
            } else {
                Some(stripped.to_string())
            }
        } else {
            None
        };
        return Ok(Command::Clear { date: date_arg });
    }

    // Tracker view: dispatches on two syntaxes:
    //   `:`                — default period (Week), then an ordered display
    //                        list of trackers and `:` mood-grid markers
    //   `:week|month|year` — period as a bare suffix on the first token
    // A stray `:<unknown>` token (e.g. `:foo`) is rejected: if the user
    // meant an id filter, they need to type `feeling : foo` with a space.
    if first == ":" || matches!(first.as_str(), ":week" | ":month" | ":year") {
        return parse_tracker_command(args);
    }

    // Grid view stub: :g (unimplemented)
    if first == ":g" {
        anyhow::bail!("Grid view (:g) is not yet implemented");
    }

    anyhow::bail!("Unknown special command: {}", first)
}

fn parse_tracker_command(args: &[String]) -> anyhow::Result<Command> {
    // args[0] is either ":" or one of ":week" / ":month" / ":year". Only the
    // suffix on the first token sets the period; everything after it is an
    // ordered display list where a bare ":" token is a positional mood-grid
    // marker and any other token is a tracker id.
    let first = &args[0];

    let (period, items_from) = if first == ":" {
        // Bare `:` always uses the Week period; args[1..] are the display list.
        (TrackerPeriod::Week, 1)
    } else {
        let period = match first.strip_prefix(":") {
            Some("week") => TrackerPeriod::Week,
            Some("month") => TrackerPeriod::Month,
            Some("year") => TrackerPeriod::Year,
            _ => unreachable!("dispatcher only forwards :, :week, :month, :year"),
        };
        (period, 1)
    };

    let items: Vec<TrackerItem> = args[items_from..]
        .iter()
        .map(|a| {
            if a == ":" {
                TrackerItem::Mood
            } else {
                TrackerItem::Tracker(a.clone())
            }
        })
        .collect();

    // Bare `:` (no items at all) renders just the mood grid, same as `: :`.
    let items = if items.is_empty() {
        vec![TrackerItem::Mood]
    } else {
        items
    };

    Ok(Command::Tracker { period, items })
}

fn parse_task_command(args: &[String]) -> anyhow::Result<Command> {
    // The leading "!" has already been stripped by the caller — `args` holds
    // everything after it.

    // ! alone → interactive oneshot creation (the name is prompted; the
    // editor flow runs for priority/target/body). `!` is no longer a list
    // view — the pending-oneshots list lives at `@:o`.
    if args.is_empty() {
        return Ok(Command::Task(Task {
            task_type: TaskType::OneShot,
            name: None,
            priority: None,
            date: None,
            body: String::new(),
            open_editor: true,
            prefill: None,
            available_duration: None,
        }));
    }

    // `! @ [description] [.. body]` → interactive recurring task creation.
    // An optional description after the bare '@' pre-fills the name prompt,
    // mirroring oneshot creation where the name comes from the command
    // line; a trailing `..` carries the body text (empty body → body
    // editor). The description is trimmed; if it trims to empty it is
    // treated as absent.
    if args[0] == "@" {
        return parse_recurring_task(args);
    }

    // `! @<time> [:description] [%<duration>] [.. [body]]` → scheduled task
    // creation. The first '@' word is the start time (multi-word forms like
    // `@2024-03-20 14:30` survive shell word-splitting); a word beginning
    // with ':' starts the description, a word beginning with '%' starts the
    // duration. Note the space discriminator: `! @ 10pm` is a recurring
    // task named "10pm", while `! @10pm` is a scheduled task.
    if args[0].starts_with('@') {
        return parse_scheduled_task(args);
    }

    // Creating oneshot task: ! <description> [@<time> [more time words]] [..]
    //
    // The first (and only) word starting with '@' marks the start of the
    // time field: everything from that word (leading '@' stripped) until
    // '..' is joined (space-separated) into `date`, so multi-word times
    // like `@2024-03-20 14:30:00` survive shell word-splitting. Words
    // before it form the description. A second '@' word is rejected with an
    // error. After '..' (body state) '@' is literal and never looked for.
    //
    // `..` may appear anywhere in the args (not only as the last token).
    // Everything before the *first* `..` contributes to the name and time;
    // everything after is joined (space-separated) into `body`. The editor
    // opens (with priority/target_count prompts as usual) iff `..` was used
    // AND `body` ends up empty.
    let mut has_dotdot = false;
    let mut date_parts: Vec<String> = Vec::new();
    let mut in_time = false;
    let mut name_parts: Vec<String> = Vec::new();
    let mut body_parts: Vec<String> = Vec::new();

    let mut i = 0; // `args` starts after the "!"
    while i < args.len() {
        let arg = &args[i];
        if arg == ".." {
            has_dotdot = true;
            i += 1;
            continue;
        }
        if has_dotdot {
            body_parts.push(arg.clone());
            i += 1;
        } else if in_time {
            if arg.starts_with('@') {
                cba::ebog!(
                    "@time";
                    "Only one @<time> is allowed per task (found '{}')",
                    arg
                );
                anyhow::bail!("Only one @<time> is allowed per task");
            }
            date_parts.push(arg.clone());
            i += 1;
        } else if let Some(rest) = arg.strip_prefix('@') {
            // Inline time: @YYYY-MM-DD [HH:MM:SS …]
            in_time = true;
            date_parts.push(rest.to_string());
            i += 1;
        } else {
            name_parts.push(arg.clone());
            i += 1;
        }
    }

    let name = if name_parts.is_empty() {
        None
    } else {
        let trimmed = name_parts.join(" ");
        let trimmed = trimmed.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };
    let date = if date_parts.is_empty() {
        None
    } else {
        Some(date_parts.join(" "))
    };
    let body = body_parts.join(" ");
    let open_editor = has_dotdot && body.is_empty();

    Ok(Command::Task(Task {
        task_type: TaskType::OneShot,
        name,
        priority: None,
        date,
        body,
        open_editor,
        prefill: None,
        available_duration: None,
    }))
}

/// Parse `! @ [description] [.. body]` — interactive recurring task
/// creation. `args[0]` is the bare `@`; the description (everything before
/// `..`) pre-fills the name prompt, and text after `..` becomes the body
/// (empty body → body editor).
fn parse_recurring_task(args: &[String]) -> anyhow::Result<Command> {
    // `! @ <description>` — the description is free text that pre-fills the
    // name prompt. @-words inside it (e.g. `! @ buy milk @x`) stay literal:
    // they are part of the description, never parsed as a time (unlike the
    // oneshot/scheduled @-word handling). To keep a literal `@` at the start
    // of a word, use `..` as the escape.
    let mut desc_parts: Vec<String> = Vec::new();
    let mut body_parts: Vec<String> = Vec::new();
    let mut has_dotdot = false;
    for arg in &args[1..] {
        if arg == ".." {
            has_dotdot = true;
        } else if has_dotdot {
            body_parts.push(arg.clone());
        } else {
            desc_parts.push(arg.clone());
        }
    }

    let prefill = {
        let joined = desc_parts.join(" ");
        let trimmed = joined.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };
    let body = body_parts.join(" ");
    let open_editor = has_dotdot && body.is_empty();

    Ok(Command::Task(Task {
        task_type: TaskType::Recurring,
        name: None,
        priority: None,
        date: None,
        body,
        open_editor,
        prefill,
        available_duration: None,
    }))
}

/// `! @<time> [:description] [%<duration>] [.. [body]]` → scheduled task
/// creation. Marker state machine over the words: plain words extend the
/// current field (multi-word times, durations and descriptions survive
/// shell word-splitting); a word beginning with `:` switches to / continues
/// the description, a word beginning with `%` switches to the duration
/// (there is deliberately no escape hatch — a `%`-word is always the
/// duration marker). The duration is validated here so a bad duration
/// fails fast; the start time is validated in the handler (it needs the
/// configured date dialect) before any interactive prompt. `..` switches
/// to body text (empty body → body editor).
fn parse_scheduled_task(args: &[String]) -> anyhow::Result<Command> {
    enum Field {
        Time,
        Description,
        Duration,
    }

    let mut field = Field::Time;
    let mut date_seen = false;
    let mut date_parts: Vec<String> = Vec::new();
    let mut name_parts: Vec<String> = Vec::new();
    let mut duration: Option<String> = None;
    let mut body_parts: Vec<String> = Vec::new();
    let mut has_dotdot = false;

    for word in args {
        if has_dotdot {
            body_parts.push(word.clone());
            continue;
        }
        if word == ".." {
            has_dotdot = true;
            continue;
        }

        match field {
            Field::Time => {
                if !date_seen {
                    // The first word is always the start time; the
                    // dispatcher guarantees it starts with '@'.
                    date_parts.push(word.strip_prefix('@').unwrap_or(word).to_string());
                    date_seen = true;
                } else if let Some(rest) = word.strip_prefix(':') {
                    field = Field::Description;
                    if !rest.is_empty() {
                        name_parts.push(rest.to_string());
                    }
                } else if word.starts_with('%') {
                    field = Field::Duration;
                    duration = Some(word.strip_prefix('%').unwrap_or(word).to_string());
                } else {
                    // Plain words extend the start time (e.g. `@2024-03-20
                    // 14:30`, or `@10pm meeting` — the handler then fails
                    // the datetime parse rather than treating them as a
                    // description). @-words are plain here too: only the
                    // first word carries the scheduled marker.
                    date_parts.push(word.clone());
                }
            }
            Field::Description => {
                if word.starts_with('%') {
                    field = Field::Duration;
                    duration = Some(word.strip_prefix('%').unwrap_or(word).to_string());
                } else if let Some(rest) = word.strip_prefix(':') {
                    if !rest.is_empty() {
                        name_parts.push(rest.to_string());
                    }
                } else {
                    name_parts.push(word.clone());
                }
            }
            Field::Duration => {
                if word.starts_with('%') {
                    anyhow::bail!("Only one %<duration> is allowed per scheduled task");
                }
                if let Some(rest) = word.strip_prefix(':') {
                    // A description may also come after the duration — a
                    // `:`-word switches back to the description field.
                    field = Field::Description;
                    if !rest.is_empty() {
                        name_parts.push(rest.to_string());
                    }
                } else {
                    // Plain words extend a multi-word duration ("%2 hours").
                    let parts = duration.get_or_insert_with(String::new);
                    if !parts.is_empty() {
                        parts.push(' ');
                    }
                    parts.push_str(word);
                }
            }
        }
    }

    // The duration is validated at parse time so a bad value fails fast.
    if let Some(d) = &duration {
        crate::date::parse_duration_secs(d)
            .with_context(|| format!("Invalid scheduled task duration: '{}'", d))?;
    }

    let date = if date_parts.is_empty() {
        None
    } else {
        Some(date_parts.join(" "))
    };
    let name = if name_parts.is_empty() {
        None
    } else {
        let trimmed = name_parts.join(" ");
        let trimmed = trimmed.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };
    let body = body_parts.join(" ");
    let open_editor = has_dotdot && body.is_empty();

    Ok(Command::Task(Task {
        task_type: TaskType::Scheduled,
        name,
        priority: None,
        date,
        body,
        open_editor,
        prefill: None,
        available_duration: duration,
    }))
}

fn parse_view_command(args: &[String]) -> anyhow::Result<Command> {
    // Everything after the `@` token joins into the command text: multi-word
    // datetimes work (`feeling @2024-03-20 14:30`), while a stray token
    // after `@`/`@due`/`@done` is rejected here (or by the handler's date
    // parse for `@ <words>`) — never silently ignored. Only the first word
    // can carry a variant/horizon suffix (`@:o`, `@due:t`); a colon later
    // in the string is part of a time (`@2024-03-20 14:30`).
    let joined = args.join(" ");
    let token = joined.strip_prefix('@').unwrap_or(&joined).trim_start();
    let (word, rest) = match token.split_once(' ') {
        Some((word, rest)) => (word, Some(rest)),
        None => (token, None),
    };
    let (base, suffix) = match word.split_once(':') {
        Some((base, suffix)) => (base, Some(suffix)),
        None => (word, None),
    };
    let reject_extra = |rest: Option<&str>| -> anyhow::Result<()> {
        if let Some(extra) = rest {
            anyhow::bail!(
                "Unexpected argument '{extra}' after '{joined}' — view commands take no further arguments"
            );
        }
        Ok(())
    };

    match base {
        // `@[:o|:O]` → pending view; suffix `` → All, `o` → A, `O` → B.
        // The variant suffix is `o` (A) or `O` (B) — there is no `a`
        // suffix, so `@:a` is invalid.
        "" => {
            reject_extra(rest)?;
            Ok(Command::View {
                mode: ViewMode::PendingTasks,
                show: parse_variant_suffix(suffix)?,
            })
        }
        // `@done[:o|:O]` → completed view with the same suffixes.
        "done" => {
            reject_extra(rest)?;
            Ok(Command::View {
                mode: ViewMode::DoneTasks,
                show: parse_variant_suffix(suffix)?,
            })
        }
        // `@due[:t|:w]` → TodayView at ShowVariant::B, horizon per suffix
        // (`` → Today, `t` → Tomorrow, `w` → Week).
        "due" => {
            reject_extra(rest)?;
            let horizon = match suffix {
                None => TodayHorizon::Today,
                Some("t") => TodayHorizon::Tomorrow,
                Some("w") => TodayHorizon::Week,
                Some(other) => anyhow::bail!("Unknown @due suffix: ':{}' (use :t or :w)", other),
            };
            Ok(Command::Today {
                date: None,
                show: ShowVariant::B,
                horizon,
            })
        }
        // Any other @-word is a today-view date: `feeling @2024-03-20`
        // (plus optional time words, `feeling @2024-03-20 14:30`).
        // Parsing itself happens in the handler with the configured date
        // dialect (the CLI parser has no config), so an unparseable date
        // fails there with a clear error rather than here.
        _ => {
            if suffix.is_some() {
                anyhow::bail!(
                    "Unknown view suffix: '{}' (use :o or :O after @/@done, :t or :w after @due)",
                    joined
                );
            }
            Ok(Command::Today {
                date: Some(token.to_string()),
                show: ShowVariant::All,
                horizon: TodayHorizon::Today,
            })
        }
    }
}

/// The `ShowVariant` for a `@` / `@done` suffix: `` → All, `o` → A,
/// `O` → B; anything else is rejected (there is no `a` suffix — starting
/// in `A` is only possible via `:o`).
fn parse_variant_suffix(suffix: Option<&str>) -> anyhow::Result<ShowVariant> {
    match suffix {
        None => Ok(ShowVariant::All),
        Some("o") => Ok(ShowVariant::A),
        Some("O") => Ok(ShowVariant::B),
        Some(other) => anyhow::bail!(
            "Unknown view suffix: ':{}' (use :o for oneshots or :O for recurring+scheduled)",
            other
        ),
    }
}

fn parse_dash_command(args: &[String]) -> anyhow::Result<Command> {
    // The leading "-" has already been stripped by the caller — `args` holds
    // everything after it:
    //   feeling -                  → TasksEdit (stub; task editing entry point)
    //   feeling - <id> [count]     → Update a oneshot task by id
    //   feeling - <words…> [count] → Update the unique task whose name
    //                                contains the words in their order
    if args.is_empty() {
        return Ok(Command::TasksEdit);
    }

    let first = &args[0];

    // Numeric first arg → oneshot short id (`- <id> [count]` form).
    if let Ok(id) = first.parse::<i64>() {
        if args.len() > 2 {
            anyhow::bail!("Too many arguments. Usage: feeling - <id> [count]");
        }
        return Ok(Command::Update {
            target: UpdateTarget::OneShot(id),
            count: parse_count(args.get(1))?,
        });
    }

    // Otherwise the query form: `- <words…> [count]`, where a trailing
    // numeric word is the count and the rest is the word query.
    let mut words: Vec<String> = args.to_vec();
    let count = if words.len() > 1 && words.last().is_some_and(|w| w.parse::<i64>().is_ok()) {
        Some(
            words
                .pop()
                .expect("len > 1 checked above")
                .parse::<i64>()
                .context("Count must be a number")?,
        )
    } else {
        None
    };
    if words.is_empty() {
        anyhow::bail!(
            "Invalid update target: '{}'. Use a numeric id or the task's words",
            first
        );
    }

    Ok(Command::Update {
        target: UpdateTarget::Query { words },
        count,
    })
}

/// Parse an optional trailing count for the id update form.
fn parse_count(arg: Option<&String>) -> anyhow::Result<Option<i64>> {
    match arg {
        Some(s) => Ok(Some(s.parse::<i64>().context("Count must be a number")?)),
        None => Ok(None),
    }
}

fn parse_entry_command(args: &[String]) -> anyhow::Result<Command> {
    // Trackers are parsed only at the beginning and end of the line — the
    // mood words must be contiguous:
    //   feeling <mood> [-tracker value] [.. [body]]  — tracker(s) after the mood
    //   feeling -tracker value                       — tracker only (no mood)
    //   feeling [-tracker value]… <mood>             — tracker(s) before the mood
    // Once a `-tracker value` pair has been consumed *after* the mood
    // started, the rest of the line must stay tracker-shaped: another
    // `-tracker value` pair, `..`, or end of input. A bare word after that
    // point is an error, e.g. `feeling pretty ok -sleep 8 but not great`
    // (the word after `8` is not another valid tracker pattern, `..`, or
    // the end of the line).
    //
    // `..` may appear anywhere in args (not only at the end). Words before
    // the first `..` are parsed as feeling / custom-tracker values.
    // Words after `..` are joined (space-separated) into `body`. The editor
    // opens iff `..` was used AND `body` is empty.
    let mut has_dotdot = false;
    let mut feeling_parts: Vec<String> = Vec::new();
    let mut customs: Vec<(String, String)> = Vec::new();
    let mut body_parts: Vec<String> = Vec::new();
    // Set once a `-tracker value` pair is seen after the mood started; from
    // then on a bare word is rejected (only tracker pairs / `..` / EOL).
    let mut after_mood_tracker = false;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == ".." {
            has_dotdot = true;
            i += 1;
            continue;
        }
        if has_dotdot {
            body_parts.push(arg.clone());
            i += 1;
            continue;
        }
        match arg.as_str() {
            s if s.starts_with('-') && s != "-" => {
                // Tracker entry: -type value (e.g., -sleep 8, -accomplishment "fixed 2 bugs")
                let tracker_type = s[1..].to_string();
                if i + 1 < args.len() {
                    if !feeling_parts.is_empty() {
                        after_mood_tracker = true;
                    }
                    customs.push((tracker_type, args[i + 1].clone()));
                    i += 2;
                } else {
                    anyhow::bail!("Tracker '{}' requires a value", tracker_type);
                }
            }
            _ if after_mood_tracker => {
                cba::ebog!(
                    "tracker";
                    "Unexpected word '{}' after a tracker value: trackers are parsed only at the \
                     beginning and end of the line — after a mood's tracker pair, only more \
                     '-tracker value' pairs, '..', or the end of the line may follow",
                    arg
                );
                anyhow::bail!(
                    "Unexpected word '{}' after the tracker value: once a tracker follows the \
                     mood, only more '-tracker value' pairs, '..', or the end of the line may \
                     follow",
                    arg
                );
            }
            _ => {
                feeling_parts.push(arg.clone());
                i += 1;
            }
        }
    }

    let feeling = if feeling_parts.is_empty() {
        String::new()
    } else {
        feeling_parts.join(" ")
    };
    let body = body_parts.join(" ");
    let open_editor = has_dotdot && body.is_empty();

    Ok(Command::Entry(Entry {
        feeling,
        customs,
        body,
        open_editor,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_parse_feeling_simple() {
        let cmd = parse_from(args(&["ok"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "ok");
                assert!(entry.customs.is_empty());
                assert!(!entry.open_editor);
            }
            _ => panic!("Expected Entry command"),
        }
    }

    #[test]
    fn test_parse_feeling_with_editor() {
        let cmd = parse_from(args(&["ok", ".."])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "ok");
                assert!(entry.open_editor);
            }
            _ => panic!("Expected Entry command"),
        }
    }

    #[test]
    fn test_parse_feeling_with_customs() {
        let cmd = parse_from(args(&["-sleep", "8", "-water", "5", "good"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "good");
                assert_eq!(entry.customs.len(), 2);
                assert_eq!(entry.customs[0], ("sleep".to_string(), "8".to_string()));
                assert_eq!(entry.customs[1], ("water".to_string(), "5".to_string()));
            }
            _ => panic!("Expected Entry command"),
        }
    }

    #[test]
    fn test_parse_feeling_multiline() {
        let cmd = parse_from(args(&["comfortably", "numb"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "comfortably numb");
            }
            _ => panic!("Expected Entry command"),
        }
    }

    #[test]
    fn test_parse_custom_only() {
        let cmd = parse_from(args(&["-sleep", "10"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "");
                assert_eq!(entry.customs.len(), 1);
                assert_eq!(entry.customs[0], ("sleep".to_string(), "10".to_string()));
            }
            _ => panic!("Expected Entry command"),
        }
    }

    #[test]
    fn test_parse_task_oneshot() {
        let cmd = parse_from(args(&["!", "do", "something"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::OneShot);
                assert_eq!(task.name, Some("do something".to_string()));
                assert_eq!(task.priority, None);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_oneshot_with_date() {
        let cmd = parse_from(args(&["!", "task", "@2024-03-20"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::OneShot);
                assert_eq!(task.name, Some("task".to_string()));
                assert_eq!(task.date, Some("2024-03-20".to_string()));
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_oneshot_datetime_multiple_words() {
        // Everything after the @ word joins the time field, so shell-split
        // datetimes survive: @2024-03-20 14:30:00 → "2024-03-20 14:30:00".
        let cmd = parse_from(args(&["!", "task", "@2024-03-20", "14:30:00"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::OneShot);
                assert_eq!(task.name, Some("task".to_string()));
                assert_eq!(task.date, Some("2024-03-20 14:30:00".to_string()));
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_oneshot_name_is_trimmed() {
        let cmd = parse_from(args(&["!", "  buy milk  "])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::OneShot);
                assert_eq!(task.name, Some("buy milk".to_string()));
                assert_eq!(task.date, None);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_oneshot_empty_description_after_trim_is_none() {
        // A whitespace-only description trims to empty → name None (the
        // handler rejects it with "Task name is required").
        let cmd = parse_from(args(&["!", "   "])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::OneShot);
                assert_eq!(task.name, None);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_oneshot_at_in_body_is_literal() {
        // After `..` (body state) @ words are never treated as times.
        let cmd = parse_from(args(&["!", "task", "..", "@notdate"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::OneShot);
                assert_eq!(task.name, Some("task".to_string()));
                assert_eq!(task.date, None);
                assert_eq!(task.body, "@notdate");
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_oneshot_two_at_times_rejected() {
        // Only one @-word is allowed before `..`; a second is an error.
        assert!(parse_from(args(&["!", "task", "@a", "@b"])).is_err());
        // .. but inside the body state a second @ is fine (literal).
        let cmd = parse_from(args(&["!", "task", "@2024-03-20", "..", "@b"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::OneShot);
                assert_eq!(task.date, Some("2024-03-20".to_string()));
                assert_eq!(task.body, "@b");
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_recurring_create_bare() {
        // ! @ → interactive recurring creation, no pre-filled name.
        let cmd = parse_from(args(&["!", "@"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Recurring);
                assert_eq!(task.name, None);
                assert_eq!(task.prefill, None);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_recurring_create_with_description() {
        // ! @ <description> → recurring creation with the description
        // pre-filling the name prompt (like oneshot creation).
        let cmd = parse_from(args(&["!", "@", "exercise", "more"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Recurring);
                assert_eq!(task.name, None);
                assert_eq!(task.prefill, Some("exercise more".to_string()));
            }
            _ => panic!("Expected Task command"),
        }

        // Whitespace-only description trims to absent.
        let cmd = parse_from(args(&["!", "@", "  "])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.prefill, None);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_scheduled_date_only() {
        // `! @10pm` → scheduled creation with only the start time.
        let cmd = parse_from(args(&["!", "@10pm"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Scheduled);
                assert_eq!(task.name, None);
                assert_eq!(task.date, Some("10pm".to_string()));
                assert_eq!(task.available_duration, None);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_scheduled_date_with_extra_words_is_all_date() {
        // `! @10pm meeting` (no markers) keeps the whole first field as the
        // date — it is validated (and fails) in the handler, never
        // becoming a description.
        let cmd = parse_from(args(&["!", "@10pm", "meeting"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Scheduled);
                assert_eq!(task.name, None);
                assert_eq!(task.date, Some("10pm meeting".to_string()));
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_scheduled_description() {
        // `! @10pm :meeting` → start time + description.
        let cmd = parse_from(args(&["!", "@10pm", ":meeting"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Scheduled);
                assert_eq!(task.name, Some("meeting".to_string()));
                assert_eq!(task.date, Some("10pm".to_string()));
                assert_eq!(task.available_duration, None);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_scheduled_multiword_description() {
        // `! @10pm :a :b` → description words join; a `:`-word mid-
        // description just continues it.
        let cmd = parse_from(args(&["!", "@10pm", ":a", ":b"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Scheduled);
                assert_eq!(task.name, Some("a b".to_string()));
                assert_eq!(task.date, Some("10pm".to_string()));
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_scheduled_duration() {
        // `! @10pm @2 hours` → start time + duration, no description.
        let cmd = parse_from(args(&["!", "@10pm", "%2", "hours"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Scheduled);
                assert_eq!(task.name, None);
                assert_eq!(task.date, Some("10pm".to_string()));
                assert_eq!(task.available_duration, Some("2 hours".to_string()));
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_scheduled_description_and_duration() {
        // `! @10pm :meeting @2 hours` → all three fields.
        let cmd = parse_from(args(&["!", "@10pm", ":meeting", "%2", "hours"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Scheduled);
                assert_eq!(task.name, Some("meeting".to_string()));
                assert_eq!(task.date, Some("10pm".to_string()));
                assert_eq!(task.available_duration, Some("2 hours".to_string()));
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_scheduled_description_after_duration_allowed() {
        // Description may come after the duration too: `! @10pm %2 hours :meeting`.
        let cmd = parse_from(args(&["!", "@10pm", "%2", "hours", ":meeting"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Scheduled);
                assert_eq!(task.name, Some("meeting".to_string()));
                assert_eq!(task.date, Some("10pm".to_string()));
                assert_eq!(task.available_duration, Some("2 hours".to_string()));
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_scheduled_duplicate_duration_rejected() {
        assert!(parse_from(args(&["!", "@10pm", "%2", "hours", "%30", "minutes"])).is_err());
    }

    #[test]
    fn test_parse_task_scheduled_bad_duration_rejected() {
        // A malformed duration fails fast at parse time.
        assert!(parse_from(args(&["!", "@10pm", "%2", "elephants"])).is_err());
    }

    #[test]
    fn test_parse_task_scheduled_body() {
        // `! @10pm :meeting .. take notes` → body after `..`.
        let cmd = parse_from(args(&["!", "@10pm", ":meeting", "..", "take", "notes"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Scheduled);
                assert_eq!(task.name, Some("meeting".to_string()));
                assert_eq!(task.body, "take notes");
                assert!(!task.open_editor);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_scheduled_bare_dotdot_opens_editor() {
        let cmd = parse_from(args(&["!", "@10pm", ":meeting", ".."])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Scheduled);
                assert_eq!(task.body, "");
                assert!(task.open_editor);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_recurring_body() {
        // `! @ exercise .. notes` → recurring creation with the description
        // pre-filling the name and `.. notes` as the body.
        let cmd = parse_from(args(&["!", "@", "exercise", "..", "notes"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Recurring);
                assert_eq!(task.prefill, Some("exercise".to_string()));
                assert_eq!(task.body, "notes");
                assert!(!task.open_editor);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_recurring_bare_dotdot_opens_editor() {
        let cmd = parse_from(args(&["!", "@", "exercise", ".."])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Recurring);
                assert_eq!(task.prefill, Some("exercise".to_string()));
                assert_eq!(task.body, "");
                assert!(task.open_editor);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_at_name_with_extra_args_is_scheduled() {
        // `! @exercise now` → scheduled creation: the leading @ starts the
        // time field, which swallows the rest into the date (validated by
        // the handler, which fails on a bad date before any prompt).
        let cmd = parse_from(args(&["!", "@exercise", "now"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Scheduled);
                assert_eq!(task.name, None);
                assert_eq!(task.date, Some("exercise now".to_string()));
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_task_recurring_create() {
        // Recurring task creation via ! @
        let cmd = parse_from(args(&["!", "@"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::Recurring);
                assert_eq!(task.name, None);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_view_oneshot_list() {
        // Bare `!` is interactive oneshot creation now — name prompted,
        // editor flow on. The pending-oneshots list lives at `@:o`.
        let cmd = parse_from(args(&["!"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::OneShot);
                assert_eq!(task.name, None);
                assert!(task.open_editor);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_view_recurring() {
        let cmd = parse_from(args(&["@"])).unwrap();
        match cmd {
            Command::View { mode, show } => {
                assert_eq!(mode, ViewMode::PendingTasks);
                assert_eq!(show, ShowVariant::All);
            }
            _ => panic!("Expected View command"),
        }
    }

    #[test]
    fn test_parse_view_variant_suffixes() {
        // @:o / @:O → pending view at A / B.
        let cmd = parse_from(args(&["@:o"])).unwrap();
        match cmd {
            Command::View { mode, show } => {
                assert_eq!(mode, ViewMode::PendingTasks);
                assert_eq!(show, ShowVariant::A);
            }
            _ => panic!("Expected View command"),
        }
        let cmd = parse_from(args(&["@:O"])).unwrap();
        match cmd {
            Command::View { mode, show } => {
                assert_eq!(mode, ViewMode::PendingTasks);
                assert_eq!(show, ShowVariant::B);
            }
            _ => panic!("Expected View command"),
        }
        // @done:o / @done:O / @done → done view at A / B / All.
        let cmd = parse_from(args(&["@done:o"])).unwrap();
        match cmd {
            Command::View { mode, show } => {
                assert_eq!(mode, ViewMode::DoneTasks);
                assert_eq!(show, ShowVariant::A);
            }
            _ => panic!("Expected View command"),
        }
        let cmd = parse_from(args(&["@done:O"])).unwrap();
        match cmd {
            Command::View { mode, show } => {
                assert_eq!(mode, ViewMode::DoneTasks);
                assert_eq!(show, ShowVariant::B);
            }
            _ => panic!("Expected View command"),
        }
        let cmd = parse_from(args(&["@done"])).unwrap();
        match cmd {
            Command::View { mode, show } => {
                assert_eq!(mode, ViewMode::DoneTasks);
                assert_eq!(show, ShowVariant::All);
            }
            _ => panic!("Expected View command"),
        }
    }

    #[test]
    fn test_parse_view_rejects_extra_args() {
        // Extra words join into the command text: `@due extra` and
        // `@done extra` fail their suffix checks, while `@ <date> extra`
        // becomes a multi-word date that the handler rejects at parse time
        // (it can no longer be silently ignored).
        assert!(parse_from(args(&["@due", "extra"])).is_err());
        assert!(parse_from(args(&["@due:t", "extra"])).is_err());
        assert!(parse_from(args(&["@done", "extra"])).is_err());
        assert!(parse_from(args(&["@done:O", "extra"])).is_err());
        assert!(parse_from(args(&["@:o", "extra"])).is_err());
        let cmd = parse_from(args(&["@", "extra"])).unwrap();
        assert!(matches!(cmd, Command::Today { date: Some(d), .. } if d == "extra"));
        let cmd = parse_from(args(&["@2024-03-15", "extra"])).unwrap();
        assert!(matches!(cmd, Command::Today { date: Some(d), .. } if d == "2024-03-15 extra"));
    }

    #[test]
    fn test_parse_view_invalid_suffixes() {
        // There is no `a` suffix; unknown suffixes are rejected.
        assert!(parse_from(args(&["@:a"])).is_err());
        assert!(parse_from(args(&["@done:a"])).is_err());
        assert!(parse_from(args(&["@:x"])).is_err());
        assert!(parse_from(args(&["@due:x"])).is_err());
    }

    #[test]
    fn test_parse_view_done() {
        let cmd = parse_from(args(&["@done"])).unwrap();
        match cmd {
            Command::View { mode, show } => {
                assert_eq!(mode, ViewMode::DoneTasks);
                assert_eq!(show, ShowVariant::All);
            }
            _ => panic!("Expected View command"),
        }
    }

    #[test]
    fn test_parse_view_due() {
        // @due → TodayView at ShowVariant::B with the Today horizon.
        let cmd = parse_from(args(&["@due"])).unwrap();
        match cmd {
            Command::Today {
                date,
                show,
                horizon,
            } => {
                assert_eq!(date, None);
                assert_eq!(show, ShowVariant::B);
                assert_eq!(horizon, TodayHorizon::Today);
            }
            _ => panic!("Expected Today command"),
        }
    }

    #[test]
    fn test_parse_view_due_horizons() {
        // @due:t / @due:w → TodayView at B with the Tomorrow / Week horizon.
        let cmd = parse_from(args(&["@due:t"])).unwrap();
        match cmd {
            Command::Today {
                date,
                show,
                horizon,
            } => {
                assert_eq!(date, None);
                assert_eq!(show, ShowVariant::B);
                assert_eq!(horizon, TodayHorizon::Tomorrow);
            }
            _ => panic!("Expected Today command"),
        }
        let cmd = parse_from(args(&["@due:w"])).unwrap();
        match cmd {
            Command::Today {
                date,
                show,
                horizon,
            } => {
                assert_eq!(date, None);
                assert_eq!(show, ShowVariant::B);
                assert_eq!(horizon, TodayHorizon::Week);
            }
            _ => panic!("Expected Today command"),
        }
    }

    #[test]
    fn test_parse_tracker_week() {
        let cmd = parse_from(args(&[":"])).unwrap();
        match cmd {
            Command::Tracker { period, items } => {
                assert_eq!(period, TrackerPeriod::Week);
                // Bare `:` renders just the mood grid.
                assert_eq!(items, vec![TrackerItem::Mood]);
            }
            _ => panic!("Expected Tracker command"),
        }
    }

    #[test]
    fn test_parse_tracker_month() {
        let cmd = parse_from(args(&[":month"])).unwrap();
        match cmd {
            Command::Tracker { period, items } => {
                assert_eq!(period, TrackerPeriod::Month);
                // No display list: mood grid only.
                assert_eq!(items, vec![TrackerItem::Mood]);
            }
            _ => panic!("Expected Tracker command"),
        }
    }

    #[test]
    fn test_parse_tracker_year() {
        let cmd = parse_from(args(&[":year"])).unwrap();
        match cmd {
            Command::Tracker { period, .. } => {
                assert_eq!(period, TrackerPeriod::Year);
            }
            _ => panic!("Expected Tracker command"),
        }
    }

    #[test]
    fn test_parse_tracker_colon_second_arg_is_tracker() {
        // `: month` is a tracker named "month", not a period: only the
        // first-token suffix sets the period.
        let cmd = parse_from(args(&[":", "month"])).unwrap();
        match cmd {
            Command::Tracker { period, items } => {
                assert_eq!(period, TrackerPeriod::Week);
                assert_eq!(items, vec![TrackerItem::Tracker("month".to_string())]);
            }
            _ => panic!("Expected Tracker command"),
        }
    }

    #[test]
    fn test_parse_tracker_with_ids() {
        let cmd = parse_from(args(&[":", "@1", "@2", "sleep"])).unwrap();
        match cmd {
            Command::Tracker { period, items } => {
                assert_eq!(period, TrackerPeriod::Week);
                assert_eq!(
                    items,
                    vec![
                        TrackerItem::Tracker("@1".to_string()),
                        TrackerItem::Tracker("@2".to_string()),
                        TrackerItem::Tracker("sleep".to_string())
                    ]
                );
            }
            _ => panic!("Expected Tracker command"),
        }
    }

    #[test]
    fn test_parse_tracker_period_with_ids() {
        let cmd = parse_from(args(&[":month", "@1", "sleep"])).unwrap();
        match cmd {
            Command::Tracker { period, items } => {
                assert_eq!(period, TrackerPeriod::Month);
                assert_eq!(
                    items,
                    vec![
                        TrackerItem::Tracker("@1".to_string()),
                        TrackerItem::Tracker("sleep".to_string())
                    ]
                );
            }
            _ => panic!("Expected Tracker command"),
        }
    }

    #[test]
    fn test_parse_tracker_first_arg_rejected() {
        // `:foo` with no space is rejected in the dispatcher; the safe
        // entry is `feeling : foo` with a space.
        let cmd = parse_from(args(&[":", "foo"])).unwrap();
        match cmd {
            Command::Tracker { period, items } => {
                assert_eq!(period, TrackerPeriod::Week);
                assert_eq!(items, vec![TrackerItem::Tracker("foo".to_string())]);
            }
            _ => panic!("Expected Tracker command"),
        }
    }

    #[test]
    fn test_parse_tracker_mood_marker_positional() {
        // `: @1 : sleep` renders @1 grid, mood grid, sleep grid, in order.
        let cmd = parse_from(args(&[":", "@1", ":", "sleep"])).unwrap();
        match cmd {
            Command::Tracker { period, items } => {
                assert_eq!(period, TrackerPeriod::Week);
                assert_eq!(
                    items,
                    vec![
                        TrackerItem::Tracker("@1".to_string()),
                        TrackerItem::Mood,
                        TrackerItem::Tracker("sleep".to_string())
                    ]
                );
            }
            _ => panic!("Expected Tracker command"),
        }
    }

    #[test]
    fn test_parse_tracker_colon_colon_is_mood_only() {
        // `: :` is the same as bare `:`: mood grid only.
        let cmd = parse_from(args(&[":", ":"])).unwrap();
        match cmd {
            Command::Tracker { period, items } => {
                assert_eq!(period, TrackerPeriod::Week);
                assert_eq!(items, vec![TrackerItem::Mood]);
            }
            _ => panic!("Expected Tracker command"),
        }
    }

    #[test]
    fn test_parse_tracker_suffix_period_with_mood_marker() {
        // `:week : sleep` → Week period, mood grid then sleep grid.
        let cmd = parse_from(args(&[":week", ":", "sleep"])).unwrap();
        match cmd {
            Command::Tracker { period, items } => {
                assert_eq!(period, TrackerPeriod::Week);
                assert_eq!(
                    items,
                    vec![TrackerItem::Mood, TrackerItem::Tracker("sleep".to_string())]
                );
            }
            _ => panic!("Expected Tracker command"),
        }
    }

    #[test]
    fn test_parse_dash_alone_is_tasks_edit() {
        // `feeling -` (bare) → TasksEdit (stub); `- <id> [count]` and
        // `- <words…> [count]` remain the update forms (tested below).
        let cmd = parse_from(args(&["-"])).unwrap();
        assert_eq!(cmd, Command::TasksEdit);
    }

    #[test]
    fn test_parse_update() {
        let cmd = parse_from(args(&["-", "5"])).unwrap();
        match cmd {
            Command::Update { target, count } => {
                assert_eq!(target, UpdateTarget::OneShot(5));
                assert_eq!(count, None);
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_parse_update_with_count() {
        let cmd = parse_from(args(&["-", "5", "3"])).unwrap();
        match cmd {
            Command::Update { target, count } => {
                assert_eq!(target, UpdateTarget::OneShot(5));
                assert_eq!(count, Some(3));
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_parse_update_at_name_is_query_not_recurring() {
        // The `- @name` recurring form was removed: `- @exercise` is now a
        // word query (which matches nothing, since task names don't carry
        // the '@' prefix).
        let cmd = parse_from(args(&["-", "@exercise"])).unwrap();
        match cmd {
            Command::Update { target, count } => {
                assert_eq!(
                    target,
                    UpdateTarget::Query {
                        words: vec!["@exercise".to_string()]
                    }
                );
                assert_eq!(count, None);
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_parse_update_query_words() {
        // feeling - buy milk
        let cmd = parse_from(args(&["-", "buy", "milk"])).unwrap();
        match cmd {
            Command::Update { target, count } => {
                assert_eq!(
                    target,
                    UpdateTarget::Query {
                        words: vec!["buy".to_string(), "milk".to_string()]
                    }
                );
                assert_eq!(count, None);
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_parse_update_query_words_with_count() {
        // feeling - buy milk 2 — trailing numeric word is the count
        let cmd = parse_from(args(&["-", "buy", "milk", "2"])).unwrap();
        match cmd {
            Command::Update { target, count } => {
                assert_eq!(
                    target,
                    UpdateTarget::Query {
                        words: vec!["buy".to_string(), "milk".to_string()]
                    }
                );
                assert_eq!(count, Some(2));
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_parse_update_query_words_single_word() {
        // A lone non-numeric word is a name query, not an id.
        let cmd = parse_from(args(&["-", "buy"])).unwrap();
        match cmd {
            Command::Update { target, count } => {
                assert_eq!(
                    target,
                    UpdateTarget::Query {
                        words: vec!["buy".to_string()]
                    }
                );
                assert_eq!(count, None);
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_parse_tracker_in_final_position() {
        // feeling <mood> [-tracker value] — trackers after the mood
        let cmd = parse_from(args(&["good", "-sleep", "8"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "good");
                assert_eq!(entry.customs, vec![("sleep".to_string(), "8".to_string())]);
                assert!(!entry.open_editor);
            }
            _ => panic!("Expected Entry command"),
        }

        // … with a trailing `..` body after the tracker pair.
        let cmd = parse_from(args(&["good", "-sleep", "8", "-water", "5", "..", "later"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "good");
                assert_eq!(
                    entry.customs,
                    vec![
                        ("sleep".to_string(), "8".to_string()),
                        ("water".to_string(), "5".to_string())
                    ]
                );
                assert_eq!(entry.body, "later");
                assert!(!entry.open_editor);
            }
            _ => panic!("Expected Entry command"),
        }
    }

    #[test]
    fn test_parse_tracker_beginning_and_end_only() {
        // Trackers are parsed only at the beginning (before any mood word)
        // and at the end (after the mood); mood words must be contiguous.

        // Beginning trackers then mood: feeling -sleep 8 good.
        let cmd = parse_from(args(&["-sleep", "8", "good"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "good");
                assert_eq!(entry.customs, vec![("sleep".to_string(), "8".to_string())]);
            }
            _ => panic!("Expected Entry command"),
        }

        // Beginning + end trackers around the mood: -sleep 8 good -water 5.
        let cmd = parse_from(args(&["-sleep", "8", "good", "-water", "5"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "good");
                assert_eq!(
                    entry.customs,
                    vec![
                        ("sleep".to_string(), "8".to_string()),
                        ("water".to_string(), "5".to_string())
                    ]
                );
            }
            _ => panic!("Expected Entry command"),
        }

        // Multiple beginning trackers then a multi-word mood.
        let cmd = parse_from(args(&["-sleep", "8", "but", "not", "great"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "but not great");
                assert_eq!(entry.customs, vec![("sleep".to_string(), "8".to_string())]);
            }
            _ => panic!("Expected Entry command"),
        }
    }

    #[test]
    fn test_parse_tracker_embedded_in_mood_rejected() {
        // feeling pretty ok -sleep 8 but not great: after the tracker pair
        // the word "but" is not another valid tracker pattern, `..`, or the
        // end of the line → the line is rejected.
        assert!(parse_from(args(&[
            "pretty", "ok", "-sleep", "8", "but", "not", "great"
        ]))
        .is_err());

        // Same rejection after a single mood word.
        assert!(parse_from(args(&["good", "-sleep", "8", "later"])).is_err());

        // … even after a beginning tracker + mood + end tracker pair.
        assert!(parse_from(args(&["-sleep", "8", "good", "-water", "5", "later"])).is_err());

        // But a tracker pair at the very end is fine.
        let cmd = parse_from(args(&["good", "-sleep", "8"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "good");
                assert_eq!(entry.customs, vec![("sleep".to_string(), "8".to_string())]);
            }
            _ => panic!("Expected Entry command"),
        }
    }

    #[test]
    fn test_parse_cli_strips_initial_flags() {
        // -q before the command
        let cli = parse_cli(args(&["-q", "ok"])).unwrap();
        assert_eq!(cli.opts.qv, [1, 0]);
        assert_eq!(
            cli.cmd,
            Command::Entry(Entry {
                feeling: "ok".to_string(),
                customs: vec![],
                body: String::new(),
                open_editor: false,
            })
        );

        // -v before a task view (bare `!` is interactive creation now)
        let cli = parse_cli(args(&["-v", "!"])).unwrap();
        assert_eq!(cli.opts.qv, [0, 1]);
        assert!(matches!(
            cli.cmd,
            Command::Task(Task {
                task_type: TaskType::OneShot,
                ..
            })
        ));

        // both flags, before a tracker view
        let cli = parse_cli(args(&["-q", "-v", ":week"])).unwrap();
        assert_eq!(cli.opts.qv, [1, 1]);
        assert!(matches!(cli.cmd, Command::Tracker { .. }));

        // combined token: -qv sets both
        let cli = parse_cli(args(&["-qv", "ok"])).unwrap();
        assert_eq!(cli.opts.qv, [1, 1]);
        assert!(matches!(cli.cmd, Command::Entry(_)));

        // order is not tracked: -vq is the same counts as -qv
        let cli = parse_cli(args(&["-vq", "-", "ok"])).unwrap();
        assert_eq!(cli.opts.qv, [1, 1]);
        assert!(matches!(cli.cmd, Command::Update { .. }));

        // repeated flags stack up as counts (-vvq → 1 quiet, 2 verbose)
        let cli = parse_cli(args(&["-vvq", "ok"])).unwrap();
        assert_eq!(cli.opts.qv, [1, 2]);
        assert!(matches!(cli.cmd, Command::Entry(_)));

        // flag alone → Today (same as no args)
        let cli = parse_cli(args(&["-q"])).unwrap();
        assert_eq!(cli.opts.qv, [1, 0]);
        assert_eq!(
            cli.cmd,
            Command::Today {
                date: None,
                show: ShowVariant::All,
                horizon: TodayHorizon::Today,
            }
        );

        // no flags
        let cli = parse_cli(args(&["ok"])).unwrap();
        assert_eq!(cli.opts.qv, [0, 0]);
        assert!(matches!(cli.cmd, Command::Entry(_)));
    }

    #[test]
    fn test_parse_cli_flags_initial_position_only() {
        // Once a non-flag token appears, -q is entry text — a tracker named
        // 'q' that requires a value → parse error.
        assert!(parse_cli(args(&["ok", "-q"])).is_err());

        // A combined -qv token is a flag now, not entry text.
        let cli = parse_cli(args(&["-qv", "ok"])).unwrap();
        assert_eq!(cli.opts.qv, [1, 1]);
        assert!(matches!(cli.cmd, Command::Entry(_)));

        // A bare dash is the update/today command, never a flag.
        let cli = parse_cli(args(&["-", "-q"])).unwrap();
        assert_eq!(cli.opts.qv, [0, 0]);
        assert!(matches!(cli.cmd, Command::Update { .. }));

        // Tokens with non-flag characters stop the flag run (-q5 is entry
        // text: a tracker named 'q5' needing a value → parse error alone).
        assert!(parse_cli(args(&["-q5"])).is_err());
    }

    #[test]
    fn test_parse_embed() {
        let cmd = parse_from(args(&[":embed"])).unwrap();
        assert_eq!(cmd, Command::Embed);
    }

    #[test]
    fn test_parse_score() {
        let cmd = parse_from(args(&[":score", "happy", "sad"])).unwrap();
        match cmd {
            Command::Score { start, end } => {
                assert_eq!(start, "happy");
                assert_eq!(end, "sad");
            }
            _ => panic!("Expected Score command"),
        }
    }

    #[test]
    fn test_parse_empty_returns_today() {
        // `feeling` with no args → Today view (All, Today horizon).
        let today = Command::Today {
            date: None,
            show: ShowVariant::All,
            horizon: TodayHorizon::Today,
        };
        let cmd = parse_from(vec![]).unwrap();
        assert_eq!(cmd, today.clone());

        // The same through parse_cli, with or without a leading flag.
        assert_eq!(parse_cli(vec![]).unwrap().cmd, today.clone());
        assert_eq!(parse_cli(args(&["-q"])).unwrap().cmd, today);
    }

    #[test]
    fn test_parse_today_with_date() {
        // `feeling @2024-03-20` → today view anchored to that date
        // (All, Today horizon).
        let cmd = parse_from(args(&["@2024-03-20"])).unwrap();
        assert_eq!(
            cmd,
            Command::Today {
                date: Some("2024-03-20".to_string()),
                show: ShowVariant::All,
                horizon: TodayHorizon::Today,
            }
        );

        // Multi-word datetimes still work through the @ token.
        let cmd = parse_from(args(&["@2024-03-20", "14:30"])).unwrap();
        match cmd {
            Command::Today {
                date,
                show,
                horizon,
            } => {
                // Multi-word datetimes join into the date: the dispatcher
                // passes the full command text through.
                assert_eq!(date, Some("2024-03-20 14:30".to_string()));
                assert_eq!(show, ShowVariant::All);
                assert_eq!(horizon, TodayHorizon::Today);
            }
            _ => panic!("Expected Today command"),
        }

        // Relative dates parse too.
        let cmd = parse_from(args(&["@yesterday"])).unwrap();
        assert!(matches!(cmd, Command::Today { date: Some(_), .. }));

        // Unparseable dates still parse at the CLI level (the handler is
        // the authority, using the config dialect) — assert the date is
        // carried through.
        let cmd = parse_from(args(&["@bogus"])).unwrap();
        assert_eq!(
            cmd,
            Command::Today {
                date: Some("bogus".to_string()),
                show: ShowVariant::All,
                horizon: TodayHorizon::Today,
            }
        );

        // The task views are untouched.
        assert!(matches!(
            parse_from(args(&["@done"])).unwrap(),
            Command::View {
                mode: ViewMode::DoneTasks,
                ..
            }
        ));
    }

    #[test]
    fn test_parse_help_is_cli_level_only() {
        // `-h` / `--help` are handled in parse_cli (initial position only).
        let cli = parse_cli(args(&["-h"])).unwrap();
        assert_eq!(cli.opts.qv, [0, 0]);
        assert_eq!(cli.cmd, Command::Help);

        let cli = parse_cli(args(&["--help"])).unwrap();
        assert_eq!(cli.cmd, Command::Help);

        // Help wins over other initial-position flags.
        let cli = parse_cli(args(&["-q", "-h"])).unwrap();
        assert_eq!(cli.opts.qv, [1, 0]);
        assert_eq!(cli.cmd, Command::Help);
        // After a non-flag token, -h is entry text (a tracker needing a
        // value), not help.
        assert!(parse_cli(args(&["ok", "-h"])).is_err());
    }

    #[test]
    fn test_parse_config() {
        let cmd = parse_from(args(&[":config"])).unwrap();
        assert_eq!(cmd, Command::Config);
    }

    #[test]
    fn test_parse_config_rejects_extra_args() {
        let result = parse_from(args(&[":config", "extra"]));
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_prune() {
        let cmd = parse_from(args(&[":prune"])).unwrap();
        assert_eq!(cmd, Command::Prune);
    }

    #[test]
    fn test_parse_prune_rejects_extra_args() {
        let result = parse_from(args(&[":prune", "extra"]));
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_color() {
        let cmd = parse_from(args(&[":color", "drained"])).unwrap();
        match cmd {
            Command::Color { mood } => assert_eq!(mood, "drained"),
            _ => panic!("Expected Color command"),
        }
    }

    #[test]
    fn test_parse_color_multword() {
        let cmd = parse_from(args(&[":color", "feeling", "drained"])).unwrap();
        match cmd {
            Command::Color { mood } => assert_eq!(mood, "feeling drained"),
            _ => panic!("Expected Color command"),
        }
    }

    #[test]
    fn test_parse_color_rejects_empty() {
        let result = parse_from(args(&[":color"]));
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_tabs_preserved() {
        // Tabs are passed through by parser, rejected by handler
        let cmd = parse_from(args(&["ok\ttab"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "ok\ttab");
            }
            _ => panic!("Expected Entry command"),
        }
    }

    #[test]
    fn test_parse_editor_flag_only_when_dotdot_used_with_empty_body() {
        // `..` at end, no text after → open_editor=true, body empty.
        let cmd = parse_from(args(&["ok", ".."])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "ok");
                assert_eq!(entry.body, "");
                assert!(entry.open_editor);
            }
            _ => panic!("Expected Entry command"),
        }

        // `..` at end with text after → body is the joined text, no editor
        // (text wins over the editor prompt).
        let cmd = parse_from(args(&["ok", "..", "later", "thoughts"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "ok");
                assert_eq!(entry.body, "later thoughts");
                assert!(!entry.open_editor);
            }
            _ => panic!("Expected Entry command"),
        }

        // `..` anywhere in the middle splits: pre-.. is mood, post-.. is body.
        // `["..", "ok"]` puts "ok" into body, leaving mood empty; editor is
        // gated off because body is non-empty.
        let cmd = parse_from(args(&["..", "ok"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "");
                assert_eq!(entry.body, "ok");
                assert!(!entry.open_editor);
            }
            _ => panic!("Expected Entry command"),
        }

        // `..` in the middle with text on both sides — editor gated off, body
        // is the joined post-.. text.
        let cmd = parse_from(args(&["ok", "more", "..", "journal", "entry"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "ok more");
                assert_eq!(entry.body, "journal entry");
                assert!(!entry.open_editor);
            }
            _ => panic!("Expected Entry command"),
        }

        // No `..` at all: no body, no editor.
        let cmd = parse_from(args(&["ok"])).unwrap();
        match cmd {
            Command::Entry(entry) => {
                assert_eq!(entry.feeling, "ok");
                assert_eq!(entry.body, "");
                assert!(!entry.open_editor);
            }
            _ => panic!("Expected Entry command"),
        }
    }

    #[test]
    fn test_parse_task_dotdot_in_middle_splits_name_and_body() {
        let cmd = parse_from(args(&["!", "do", "thing", "..", "body", "text"])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::OneShot);
                assert_eq!(task.name, Some("do thing".to_string()));
                assert_eq!(task.body, "body text");
                assert!(!task.open_editor);
            }
            _ => panic!("Expected Task command"),
        }

        // `..` at end with empty body → editor opens.
        let cmd = parse_from(args(&["!", "do", "thing", ".."])).unwrap();
        match cmd {
            Command::Task(task) => {
                assert_eq!(task.task_type, TaskType::OneShot);
                assert_eq!(task.name, Some("do thing".to_string()));
                assert_eq!(task.body, "");
                assert!(task.open_editor);
            }
            _ => panic!("Expected Task command"),
        }
    }

    #[test]
    fn test_parse_clear() {
        let cmd = parse_from(args(&[":clear"])).unwrap();
        assert_eq!(cmd, Command::Clear { date: None });

        let cmd = parse_from(args(&[":clear", "@2024-03-20"])).unwrap();
        assert_eq!(
            cmd,
            Command::Clear {
                date: Some("2024-03-20".to_string())
            }
        );

        let cmd = parse_from(args(&[":clear", "2024-03-20"])).unwrap();
        assert_eq!(
            cmd,
            Command::Clear {
                date: Some("2024-03-20".to_string())
            }
        );

        let cmd = parse_from(args(&[":clear", "@"])).unwrap();
        assert_eq!(cmd, Command::Clear { date: None });
    }
}
