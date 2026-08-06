use anyhow::Context;

use super::super::Command;
use crate::types::{Task, TaskKind};

pub(crate) fn parse_task_command(mut args: &[String]) -> anyhow::Result<Command> {
    // The leading "!" has already been stripped by the caller — `args` holds
    // everything after it.

    let parent = if !args.is_empty() {
        parse_parent_flag(&args[0])?
    } else {
        None
    };

    if parent.is_some() {
        args = &args[1..];
    }

    // ! alone → interactive oneshot creation (the name is prompted; the
    // editor flow runs for priority/target/body). `!` is no longer a list
    // view — the pending-oneshots list lives at `@:o`. An optional
    // `-<parent_id>` (short id) pre-fills the parent prompt.
    if args.is_empty() {
        return Ok(Command::Task(Task {
            task_type: TaskKind::Oneshot,
            name: None,
            priority: None,
            date: None,
            body: String::new(),
            open_editor: true,
            prefill: None,
            available_duration: None,
            parent,
        }));
    }

    // `! @ [name] [.. body]` → interactive recurring task creation.
    // An optional name after the bare '@' pre-fills the name prompt,
    // mirroring oneshot creation where the name comes from the command
    // line; a trailing `..` carries the body text (empty body → body
    // editor). The name is trimmed; if it trims to empty it is
    // treated as absent.
    if args[0] == "@" {
        return parse_recurring_task(args);
    }

    // `! @<time> [:name] [%<duration>] [.. [body]]` → scheduled task
    // creation. The first '@' word is the start time (multi-word forms like
    // `@2024-03-20 14:30` survive shell word-splitting); a word beginning
    // with ':' starts the name, a word beginning with '%' starts the
    // duration. Note the space discriminator: `! @ 10pm` is a recurring
    // task named "10pm", while `! @10pm` is a scheduled task.
    if args[0].starts_with('@') {
        return parse_scheduled_task(args);
    }

    // Creating oneshot task: ! <name> [@<time> [more time words]] [..]
    //
    // The grammar is positional, not stateful: split at the first `..` —
    // everything before it goes to name/time, everything after is body —
    // then within that head split at the first '@'-word into name | time.
    // `..` may appear anywhere in the args: a later `..` inside the body is
    // plain body text. The first time word's leading '@' is stripped and the
    // following words append, so multi-word times like `@2024-03-20
    // 14:30:00` survive shell word-splitting. Words before it form the
    // name; a second '@'-word in the time field is rejected with an
    // error; after `..`, '@' is literal and never looked for. The editor
    // opens (with priority/target_count prompts as usual) iff `..` was used
    // AND `body` ends up empty.
    // `-<parent_id>` parses in the initial position only: once a parent
    // has been consumed (or the first word is not a parent flag), later
    // words starting with '-' are ordinary name/time/body text — e.g.
    // `! -5 buy -milk` is task "buy -milk" under short id 5.
    let dotdot = args.iter().position(|a| a == "..");
    let (head, body_parts) = match dotdot {
        Some(d) => (&args[..d], &args[d + 1..]),
        None => (args, &[][..]),
    };
    let at = head.iter().position(|a| a.starts_with('@'));
    let (name_parts, time_parts) = match at {
        Some(a) => (&head[..a], &head[a..]),
        None => (head, &[][..]),
    };
    for word in time_parts.iter().skip(1) {
        if word.starts_with('@') {
            cba::ebog!(
                "@time";
                "Only one @<time> is allowed per task (found '{}')",
                word
            );
            anyhow::bail!("Only one @<time> is allowed per task");
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
    let date = if time_parts.is_empty() {
        None
    } else {
        // `time_parts[0]` is the word that opened the time field, so it
        // always starts with '@' — strip it, append the rest verbatim, and
        // resolve the whole field to an epoch now (an unparseable date
        // fails here, before anything is created).
        let mut parts = time_parts[0][1..].to_string();
        if time_parts.len() > 1 {
            parts.push(' ');
            parts.push_str(&time_parts[1..].join(" "));
        }
        Some(
            crate::date::parse_datetime(&parts, crate::date::DATE_DIALECT)
                .with_context(|| format!("Invalid task start time: '{}'", parts))?,
        )
    };
    let body = body_parts.join(" ");
    let open_editor = dotdot.is_some() && body.is_empty();

    Ok(Command::Task(Task {
        task_type: TaskKind::Oneshot,
        name,
        priority: None,
        date,
        body,
        open_editor,
        prefill: None,
        available_duration: None,
        parent,
    }))
}

/// Parse a `-<parent_id>` flag (the parent task's short id) from a single
/// argument. Returns `None` for any argument that is not of that shape
/// (e.g. a name starting with a dash), so callers fall through to normal
/// parsing.
fn parse_parent_flag(arg: &str) -> anyhow::Result<Option<i64>> {
    let Some(rest) = arg.strip_prefix('-') else {
        return Ok(None);
    };
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(None);
    }
    let Ok(short_id) = rest.parse::<i64>() else {
        anyhow::bail!("Invalid -<parent_id> '{}': must be a number", arg);
    };
    Ok(Some(short_id))
}

/// Parse `! @ [name] [.. body]` — interactive recurring task
/// creation. `args[0]` is the bare `@`; the name (everything before
/// `..`) pre-fills the name prompt, and text after `..` becomes the body
/// (empty body → body editor).
fn parse_recurring_task(args: &[String]) -> anyhow::Result<Command> {
    // `! @ <name>` — the name is free text that pre-fills the
    // name prompt. @-words inside it (e.g. `! @ buy milk @x`) stay literal:
    // they are part of the name, never parsed as a time (unlike the
    // oneshot/scheduled @-word handling). To keep a literal `@` at the start
    // of a word, use `..` as the escape.
    //
    // Same positional `..` split as the oneshot parser: everything before
    // the first `..` is the name, everything after is body text (a later
    // `..` inside the body is plain body text; bare `..` → body editor).
    let rest = &args[1..];
    let dotdot = rest.iter().position(|a| a == "..");
    let (head, body_parts) = match dotdot {
        Some(d) => (&rest[..d], &rest[d + 1..]),
        None => (rest, &[][..]),
    };

    let prefill = {
        let joined = head.join(" ");
        let trimmed = joined.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };
    let body = body_parts.join(" ");
    let open_editor = dotdot.is_some() && body.is_empty();

    Ok(Command::Task(Task {
        task_type: TaskKind::Recurring,
        name: None,
        priority: None,
        date: None,
        body,
        open_editor,
        prefill,
        available_duration: None,
        parent: None,
    }))
}
/// `! @<time> [:name] [%<duration>] [.. [body]]` → scheduled task
/// creation.
fn parse_scheduled_task(args: &[String]) -> anyhow::Result<Command> {
    // Body split first, same positional rule as the oneshot parser:
    // everything before the first `..` is the command words, everything
    // after is body text (a later `..` inside the body is plain body text;
    // bare `..` → body editor).
    let dotdot = args.iter().position(|a| a == "..");
    let (head, body_parts) = match dotdot {
        Some(d) => (&args[..d], &args[d + 1..]),
        None => (args, &[][..]),
    };

    // The first marker word ends the time field. The dispatcher guarantees
    // the first word starts with '@', so the time field is never empty.
    let colon = head.iter().position(|w| w.starts_with(':'));
    let pct = head.iter().position(|w| w.starts_with('%'));
    let first = match (colon, pct) {
        (Some(c), Some(p)) => c.min(p),
        (Some(c), None) => c,
        (None, Some(p)) => p,
        (None, None) => head.len(),
    };

    let time_parts = &head[..first];
    let tail = &head[first..];

    let (mut name_parts, mut duration) = (&[][..], &[][..]);

    match tail.split_first() {
        None => {}
        Some((first_word, rest)) => {
            let first_is_name = first_word.starts_with(':');
            let other_marker = if first_is_name { '%' } else { ':' };

            match rest.iter().position(|w| w.starts_with(other_marker)) {
                Some(i) => {
                    let first_field = &tail[..=i];
                    let second_field = &tail[i + 1..];

                    if first_is_name {
                        name_parts = first_field;
                        duration = second_field;
                    } else {
                        duration = first_field;
                        name_parts = second_field;
                    }
                }
                None => {
                    if first_is_name {
                        name_parts = tail;
                    } else {
                        duration = tail;
                    }
                }
            }
        }
    }

    let date = if time_parts.is_empty() {
        None
    } else {
        let joined = time_parts.join(" ");
        let s = joined.strip_prefix('@').unwrap_or(&joined);
        Some(
            crate::date::parse_datetime(s, crate::date::DATE_DIALECT).with_context(|| {
                format!(
                    "Invalid scheduled task start time: '{}' \
                 (name starts with ':', duration with '%')",
                    s
                )
            })?,
        )
    };

    let name = if name_parts.is_empty() {
        None
    } else {
        // Every word in the name segment may carry a `:` marker — strip it
        // so the segment joins cleanly (plain words are kept verbatim).
        let trimmed = name_parts
            .iter()
            .map(|w| w.strip_prefix(':').unwrap_or(w))
            .collect::<Vec<_>>()
            .join(" ");
        let trimmed = trimmed.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    let available_duration = if duration.is_empty() {
        None
    } else {
        // Only the first word carries the `%` marker — strip it so the
        // segment joins cleanly. Marker words that land later in the
        // segment (a stray `:` or a duplicate `%`) are left verbatim and
        // fail the duration parse, which is the intended error path.
        let mut words = duration.iter();
        let mut d = words
            .next()
            .map(|w| w.strip_prefix('%').unwrap_or(w).to_string())
            .unwrap_or_default();
        for w in words {
            if !d.is_empty() {
                d.push(' ');
            }
            d.push_str(w);
        }
        Some(
            crate::date::parse_duration_secs(&d)
                .with_context(|| format!("Invalid scheduled task duration: '{}'", d))?,
        )
    };

    let body = body_parts.join(" ");
    let open_editor = dotdot.is_some() && body.is_empty();

    Ok(Command::Task(Task {
        task_type: TaskKind::Scheduled,
        name,
        priority: None,
        date,
        body,
        open_editor,
        prefill: None,
        available_duration,
        parent: None,
    }))
}
