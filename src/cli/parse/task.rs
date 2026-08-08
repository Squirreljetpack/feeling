use anyhow::Context;

use super::super::{Command, BODY_DELIMITER};
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

    // everything before the delimiter is the command
    // arguments (name/time), everything after it is body text: Option<String>. How an empty/absent body
    // is resolved (editor or not) is a handler concern, decided from whether
    // the creation flow is interactive.
    let (args, body) = match args.iter().position(|a| a == BODY_DELIMITER) {
        Some(d) => (&args[..d], Some(args[d + 1..].join(" "))),
        None => (args, None),
    };

    // ! → interactive oneshot creation
    if args.is_empty() {
        return Ok(Command::Task(Task {
            task_type: TaskKind::Oneshot,
            name: None,
            priority: None,
            date: None,
            body,
            prefill: None,
            available_duration: None,
            parent,
        }));
    }

    // `! @ [name]` → interactive recurring task creation.
    if args[0] == "@" {
        return parse_recurring_task(&args[1..], body);
    }

    // `! @<time> [:name] [%<duration>]` → scheduled task
    if args[0].starts_with('@') {
        return parse_scheduled_task(args, body);
    }

    // Creating oneshot task: ! <name> [@<time> ...]
    let at = args.iter().position(|a| a.starts_with('@'));
    let (name_parts, time_parts) = match at {
        Some(a) => (&args[..a], &args[a..]),
        None => (args, &[][..]),
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

    Ok(Command::Task(Task {
        task_type: TaskKind::Oneshot,
        name,
        priority: None,
        date,
        body,
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
/// creation. `args` holds everything after the bare `@` (the `..` split
/// already happened in `parse_task_command`); the name (everything before
/// `..`) pre-fills the name prompt, and `body` carries the text after `..`
/// (the handler decides editor-vs-text from the interactive flow).
fn parse_recurring_task(args: &[String], body: Option<String>) -> anyhow::Result<Command> {
    // `! @ <name>` — the name is free text that pre-fills the
    // name prompt. @-words inside it (e.g. `! @ buy milk @x`) stay literal:
    // they are part of the name, never parsed as a time (unlike the
    // oneshot/scheduled @-word handling). To keep a literal `@` at the start
    // of a word, use `..` as the escape.
    let prefill = {
        let joined = args.join(" ");
        let trimmed = joined.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    Ok(Command::Task(Task {
        task_type: TaskKind::Recurring,
        name: None,
        priority: None,
        date: None,
        body,
        prefill,
        available_duration: None,
        parent: None,
    }))
}
/// `! @<time> [:name] [%<duration>] [.. [body]]` → scheduled task
/// creation. `args` holds the command words (the `..` split already
/// happened in `parse_task_command`); `body` carries the text after `..`.
fn parse_scheduled_task(args: &[String], body: Option<String>) -> anyhow::Result<Command> {
    // The first marker word ends the time field. The dispatcher guarantees
    // the first word starts with '@', so the time field is never empty.
    let colon = args.iter().position(|w| w.starts_with(':'));
    let pct = args.iter().position(|w| w.starts_with('%'));
    let first = match (colon, pct) {
        (Some(c), Some(p)) => c.min(p),
        (Some(c), None) => c,
        (None, Some(p)) => p,
        (None, None) => args.len(),
    };

    let time_parts = &args[..first];
    let tail = &args[first..];

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

    Ok(Command::Task(Task {
        task_type: TaskKind::Scheduled,
        name,
        priority: None,
        date,
        body,
        prefill: None,
        available_duration,
        parent: None,
    }))
}
