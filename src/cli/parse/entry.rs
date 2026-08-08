use super::super::Command;
use crate::types::Entry;

pub(crate) fn parse_entry_command(args: &[String]) -> anyhow::Result<Command> {
    // Trackers are parsed only at the beginning and end of the line — the
    // mood words must be contiguous:
    //   im <mood> [-tracker value] [.. [body]]  — tracker(s) after the mood
    //   im -tracker value                       — tracker only (no mood)
    //   im [-tracker value]… <mood>             — tracker(s) before the mood
    // Once a `-tracker value` pair has been consumed *after* the mood
    // started, the rest of the line must stay tracker-shaped: another
    // `-tracker value` pair, `..`, or end of input. A bare word after that
    // point is an error, e.g. `im pretty ok -sleep 8 but not great`
    // (the word after `8` is not another valid tracker pattern, `..`, or
    // the end of the line).
    //
    // `..` may appear anywhere in args (not only at the end). Words before
    // the first `..` are parsed as mood / tracker values.
    // Words after `..` are joined (space-separated) into `body`. The editor
    // opens iff `..` was used AND `body` is empty.
    let mut has_dotdot = false;
    let mut mood_parts: Vec<String> = Vec::new();
    let mut trackers: Vec<(String, String)> = Vec::new();
    let mut task_links: Vec<i64> = Vec::new();
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
                // Tracker entry: -type value (e.g., -sleep 8, -accomplishment "fixed 2 bugs").
                // A -type followed by another dash token (or by the end of
                // the line) parses as a valueless tracker (Null trackers —
                // `-sleep -xyz -withvalue abc` chains work); the handler
                // rejects empty values for text/number/float trackers. A
                // purely numeric -<id> is a task short-id link: a single
                // token (resolved to a row id at write time).
                let tracker_type = s[1..].to_string();
                let numeric =
                    !tracker_type.is_empty() && tracker_type.chars().all(|c| c.is_ascii_digit());
                if numeric {
                    // Task link; like a tracker pair, a link after the mood
                    // starts the end-of-line tracker/link run.
                    if !mood_parts.is_empty() {
                        after_mood_tracker = true;
                    }
                    task_links.push(tracker_type.parse().map_err(|_| {
                        anyhow::anyhow!("Invalid task short id '{}'", tracker_type)
                    })?);
                    i += 1;
                } else if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    if !mood_parts.is_empty() {
                        after_mood_tracker = true;
                    }
                    trackers.push((tracker_type, args[i + 1].clone()));
                    i += 2;
                } else {
                    if !mood_parts.is_empty() {
                        after_mood_tracker = true;
                    }
                    trackers.push((tracker_type, String::new()));
                    i += 1;
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
                mood_parts.push(arg.clone());
                i += 1;
            }
        }
    }

    let mood = if mood_parts.is_empty() {
        String::new()
    } else {
        mood_parts.join(" ")
    };
    let body = body_parts.join(" ");
    let open_editor = has_dotdot && body.is_empty();

    // Mood must not contain tabs: view output uses tab separators.
    if mood.contains('\t') {
        anyhow::bail!("Mood cannot contain tab characters");
    }

    Ok(Command::Entry(Entry {
        mood,
        trackers,
        task_links,
        body,
        open_editor,
    }))
}
