# linking tasks and moods
add a table linking tasks to (mood) entries. Read help.txt, currently we allow [-tracker v].. mood [-tracker], want to also allow [-id] in place of tracker where numeric means it refers to a short id of a task. This is not a completion, simply adds an entry in the link table (short id resolved to actual task id). In make_preview, if any linked moods, display a field moods:, below '  - {mood badge} {mood text}', use mood_color_cached with a mutexed global hashmap.

- change mood_color_cached to be sync (no backfill)
- change :prune to :db, with subcommands :db prune, :backfill


Switch tracker interval, recurring tasks interval type to Span from jiff.
Switch from chrono to jiff everywhere.
where chrono-english is used, use this bridge:
// Chrono -> Jiff
let chrono_dt: DateTime<Utc> = Utc::now();
let sys_time: SystemTime = chrono_dt.into();
let jiff_ts: Timestamp = Timestamp::try_from(sys_time)?;

// Jiff -> Chrono
let sys_time: SystemTime = jiff_ts.into();
let chrono_dt: DateTime<Utc> = DateTime::from(sys_time);

We store Span from jiff like this:

(note that span/interval) should express the same thing.

use jiff::Span;
pub type DbSpan = i64;

/// Packs Years, Months, Weeks, Days, Hours, Minutes, and Seconds into a single i64.
pub fn span_to_db(span: &Span) -> DbSpan {
    let is_neg = span.is_negative();

    // Extract absolute values for each target unit
    let years = span.get_years().unsigned_abs() as u64;     // 16 bits (0..=65535)
    let months = span.get_months().unsigned_abs() as u64;   // 8 bits  (0..=255)
    let weeks = span.get_weeks().unsigned_abs() as u64;     // 8 bits  (0..=255)
    let days = span.get_days().unsigned_abs() as u64;       // 11 bits (0..=2047)
    let hours = span.get_hours().unsigned_abs() as u64;     // 8 bits  (0..=255)
    let minutes = span.get_minutes().unsigned_abs() as u64; // 6 bits  (0..=63)
    let seconds = span.get_seconds().unsigned_abs() as u64; // 6 bits  (0..=63)

    let packed: u64 = ((is_neg as u64) << 63)
        | ((years & 0xFFFF) << 47)
        | ((months & 0xFF) << 39)
        | ((weeks & 0xFF) << 31)
        | ((days & 0x7FF) << 20)
        | ((hours & 0xFF) << 12)
        | ((minutes & 0x3F) << 6)
        | (seconds & 0x3F);

    packed as i64
}

/// Unpacks a DbSpan back into a jiff::Span.
pub fn db_to_span(db_span: DbSpan) -> Span {
    let raw = db_span as u64;

    let is_neg = ((raw >> 63) & 1) == 1;
    let years = ((raw >> 47) & 0xFFFF) as i16;
    let months = ((raw >> 39) & 0xFF) as i8;
    let weeks = ((raw >> 31) & 0xFF) as i8;
    let days = ((raw >> 20) & 0x7FF) as i16;
    let hours = ((raw >> 12) & 0xFF) as i16;
    let minutes = ((raw >> 6) & 0x3F) as i8;
    let seconds = (raw & 0x3F) as i8;

    let mut span = Span::new()
        .years(years)
        .months(months)
        .weeks(weeks)
        .days(days)
        .hours(hours)
        .minutes(minutes)
        .seconds(seconds);

    if is_neg {
        span = span.negate();
    }

    span
}

### Helper fns:

/// Calculates the start time of the active interval, preserving local time and DST rules.
///
/// - `anchor`: Fixed local start time with a time zone (e.g. 2026-03-01T00:00:00[America/New_York]).
/// - `now`: Current local datetime in the same or equivalent time zone.
/// - `span`: Interval span (e.g. 1 day, 1 month, 6 hours).
pub fn current_interval_start_zoned(
    anchor: &Zoned,
    now: &Zoned,
    span: Span,
) -> Result<Zoned, jiff::Error> {
    if now < anchor {
        return Err(jiff::Error::other("`now` cannot be earlier than `anchor`"));
    }

    // 1. Estimate how many intervals have passed using rough duration division
    // to avoid stepping one-by-one from years in the past.
    let rough_span_secs = span.total(jiff::Unit::Second).unwrap_or(86400.0);
    let total_elapsed_secs = (now.timestamp() - anchor.timestamp()).as_second() as f64;
    let estimated_steps = (total_elapsed_secs / rough_span_secs).floor() as i64;

    // 2. Jump close to the target interval using calendar addition
    let mut current = anchor.checked_add(span.checked_mul(estimated_steps as i32)?)?;

    // 3. Fine-tune forward if the estimate landed slightly behind
    while let Ok(next) = current.checked_add(span) {
        if &next > now {
            break;
        }
        current = next;
    }

    // 4. Fine-tune backward if DST transition caused estimate to overshoot
    while &current > now {
        current = current.checked_sub(span)?;
    }

    Ok(current)
}

# db start time is currently represented in seconds (Epoch) and should not be updated:
fn zoned_from_unix_secs(unix_secs: i64) -> Result<Zoned, jiff::Error> {
    // 1. Create a UTC Timestamp from unix seconds
    let ts = Timestamp::from_second(unix_secs)?;

    // 2. Attach the local system time zone
    let zoned = ts.to_zoned(TimeZone::system());

    Ok(zoned)
}


# Tracker

Add a TrackerKind::Null:

in cli parsing where we expect trackers, null tracker doesn't consume a next token.

- change interval for trackers to be calendar based. Remember that the rule for adding tracker entries (for tracker with interval), each entry replaces the previous tracker entry in that interval.

- tracker config interval: type changes to (Epoch, Span), use a custom deserialization method which deserializes this from [ "string", "string"] in toml using methods in date module.

- null entry: if it has interval (span), tracker color is computed like this
min/max represent epoch seconds from interval start/end (endpoints are span end). direction is always forward. So if span is 1 day, and start is 23*seconds in an hour, and end is "2* seconds in an hour", the range bound is 23:00-24:00, cycling back to start, then 24:00 - 2:00. The color is binned by where it is in that interval, i.e. if [r, g, b] then 1pm is red, before 23:00 is blue. The cycle back point (>= , before 23:00, color is blue, < midpoint, >= 2:00, color is red) is the midpoint of (config.min.min(config.max), config.max.max(config.max)).
  If time endpoints are "right side", then midpoint is max + (day seconds - max + max 2) / 2 % day seconds.
	- when adding entry, use 0 for score. If it has both min and max, we don't change the score, simply update the old entry's date.
	- if it does not have either min/max, then Null is interpreted as a simple interval count tracker: when adding a entry, increment the score (of the existing entry if there is one in the current interval) by 1 and update the date. Color choice is similar to the other numeric TrackerKinds in this case (of either min or max missing).

In today_preview, trackers with interval should display next: and last (unscoped): like recurring.

TrackerKind::null without interval specified is basically unsupported in this update, u can even use todo!() if u don't know how to handle it anywhere, i.e. grid view for this case is skipped with bog::error, color is Color::Reset.