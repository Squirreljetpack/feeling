use anyhow::{Context, Result};
use crossterm::style::{Color as CtColor, Stylize};
use ratatui::backend::FromCrossterm;
use ratatui::style::Color as RatColor;
use sqlx::SqlitePool;
use std::io::Write;

use crate::badge::completion_badge;
use crate::clap::{CliOpts, ShowVariant, TrackerItem, TrackerPeriod, ViewMode};
use crate::config::{Config, TrackerType};
use crate::date;
use crate::sql::TaskRow;

/// Badge for text-payload custom tracker entries wherever a marker is needed
/// (e.g. the today view). A named constant so the glyph can be adjusted later.
pub(crate) const TEXT_ENTRY_BADGE: char = '◆';

/// How far ahead to include incomplete todos in the today view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodayHorizon {
    Today,
    Tomorrow,
    Week,
}

impl TodayHorizon {
    pub fn next(&self) -> Self {
        match self {
            TodayHorizon::Today => TodayHorizon::Tomorrow,
            TodayHorizon::Tomorrow => TodayHorizon::Week,
            TodayHorizon::Week => TodayHorizon::Today,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            TodayHorizon::Today => "today",
            TodayHorizon::Tomorrow => "+tomorrow",
            TodayHorizon::Week => "+this week",
        }
    }

    /// End of the horizon (inclusive) as epoch seconds, relative to the
    /// anchored day (its day-start). `Week` is always the next 7 days from
    /// the anchored day.
    pub fn end_epoch(&self, day_start: i64) -> i64 {
        match self {
            TodayHorizon::Today => date::day_end(day_start),
            TodayHorizon::Tomorrow => date::day_end(day_start + 86400),
            TodayHorizon::Week => date::day_end(day_start + 6 * 86400),
        }
    }
}

/// Category of a today-view entry, driving routing (edit / delete / preview)
/// and presentation. Replaces the old `entry_type` string and the task-only
/// `interval_secs` marker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryKind {
    /// One-shot task without a completion target (`target_count == 0`).
    Oneshot,
    /// One-shot task with a completion target (`target_count > 0`).
    Threshold,
    /// Recurring task (has an interval).
    Recurring,
    /// Scheduled task (no interval; has an availability window).
    Scheduled,
    /// Feeling entry carrying a mood label.
    Mood,
    /// Journal-only feeling entry (empty mood label; the body holds the text).
    Journal,
    /// Custom tracker entry.
    Custom,
}

impl EntryKind {
    pub fn is_task(self) -> bool {
        matches!(
            self,
            Self::Oneshot | Self::Threshold | Self::Recurring | Self::Scheduled
        )
    }

    pub fn is_mood(self) -> bool {
        matches!(self, Self::Mood | Self::Journal)
    }

    pub fn is_custom(self) -> bool {
        matches!(self, Self::Custom)
    }
}

/// Data for a single today-view entry.
#[derive(Debug, Clone)]

pub struct TodayEntry {
    pub id: Option<i64>,
    pub time: i64,
    /// Rendered time-cell text: "HH:MM", "Tu HH:MM" (two-letter weekday
    /// prefix for entries outside the anchored day), or empty for entries
    /// with no displayable time (all-day recurring tasks, undated oneshots)
    /// — those sort after all timed entries.
    pub time_label: String,
    pub kind: EntryKind,
    pub label: String,
    pub body: String,
    pub task_id: Option<i64>,
    pub priority: i32,
    /// Marker glyph rendered for this entry; `None` renders nothing (e.g.
    /// journal entries without a configured `journal_badge`).
    pub badge: Option<char>,
    /// Dynamic dot color: Oklab mood projection for feeling entries,
    /// bin_score_color for numeric custom entries, completion_badge
    /// colors for tasks, or a neutral dark gray for journal-only and
    /// text-tracker entries.
    pub color: RatColor,
}

/// Read a custom-tracker score as f64. The `score` column is stored as
/// BLOB but SQLite's dynamic typing means values can be INTEGER, REAL, or
/// TEXT. `sql::fetch_customs_for_tracker` selects `CAST(score AS TEXT)` so
/// every storage type decodes as a String; parse that.
fn score_f64(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(0.0)
}

/// Effective min/max for dot binning: configured endpoints win; a missing
/// endpoint falls back to the data range. With only one endpoint configured,
/// the derived one is clamped so the range collapses (min == max) instead of
/// silently inverting.
fn effective_range(
    cfg_min: Option<f64>,
    cfg_max: Option<f64>,
    nonzero: &[f64],
) -> (Option<f64>, Option<f64>) {
    let min = cfg_min.or_else(|| nonzero.iter().copied().reduce(f64::min));
    let max = cfg_max.or_else(|| nonzero.iter().copied().reduce(f64::max));
    let min = match (cfg_min, cfg_max) {
        (None, Some(mx)) => min.map(|mn| mn.min(mx)),
        _ => min,
    };
    let max = match (cfg_min, cfg_max) {
        (Some(mn), None) => max.map(|mx| mx.max(mn)),
        _ => max,
    };
    (min, max)
}

/// Handle tracker view (`: [week|month|year] [ids]`): display
/// dot-sequence history.
pub async fn handle_tracker<W: Write>(
    pool: &SqlitePool,
    config: &Config,
    opts: &CliOpts,
    period: TrackerPeriod,
    items: Vec<TrackerItem>,
    out: &mut W,
) -> Result<()> {
    // Grid ranges follow config.grid. Non-rolling grids anchor the start
    // to the calendar period (week_start / month start) and end at today;
    // rolling grids use a fixed-size window — the full week (always 7 dots) or
    // the "last 4 weeks" window from the feeling/ subrepo.
    let gv = &config.grid;
    let (start_epoch, end_epoch) = match period {
        TrackerPeriod::Week => {
            if gv.week_rolling {
                // Rolling 7-day window ending today.
                let start = date::today_start() - 6 * 86400;
                (start, date::today_end())
            } else {
                // Calendar week so far: from week_start through today.
                let ws = date::week_start(gv.week_start);
                (ws, date::today_end())
            }
        }
        TrackerPeriod::Month => {
            if gv.month_rolling {
                // Rolling 4-week window ending today, aligned to week_start.
                (date::rolling_month_start(gv.week_start), date::today_end())
            } else {
                // Month so far: from the month start through today.
                (date::month_start(), date::today_end())
            }
        }
        TrackerPeriod::Year => {
            if gv.year_rolling {
                // Calendar year aligned to week_start: start from the week_start on
                // or before Jan 1, through today. The grid never opens with blank
                // cells in the first column.
                (date::aligned_year_start(gv.week_start), date::today_end())
            } else {
                // Calendar year (January 1 through today). First column may have
                // blank rows if Jan 1 doesn't fall on week_start.
                (date::year_start(), date::today_end())
            }
        }
    };

    for (i, item) in items.iter().enumerate() {
        // Section header: at -v a title line (the ({period:?}) suffix only
        // from -vv); otherwise a blank-line separator — skipped before the
        // first item so there's no double leading newline.
        if i > 0 && !opts.verbose() {
            writeln!(out)?;
        }
        match item {
            TrackerItem::Mood => {
                // Positional mood-grid marker: render the mood dots grid here.
                if opts.verbose() {
                    writeln!(out, "{}", grid_title("Moods", period, opts.verbose_level()))?;
                }
                display_mood_tracker(pool, config, start_epoch, end_epoch, period, out).await?;
            }
            TrackerItem::Tracker(id_str) => {
                if let Some(name) = id_str.strip_prefix('@') {
                    // Recurring task: display completion dots
                    if opts.verbose() {
                        writeln!(
                            out,
                            "{}",
                            grid_title(&format!("@{name}"), period, opts.verbose_level())
                        )?;
                    }
                    display_recurring_tracker(
                        pool,
                        config,
                        name,
                        start_epoch,
                        end_epoch,
                        period,
                        out,
                        None,
                    )
                    .await?;
                } else {
                    // Custom tracker: display score dots
                    if opts.verbose() {
                        writeln!(out, "{}", grid_title(id_str, period, opts.verbose_level()))?;
                    }
                    display_custom_tracker(
                        pool,
                        config,
                        id_str,
                        start_epoch,
                        end_epoch,
                        period,
                        opts,
                        out,
                        None,
                    )
                    .await?;
                }
            }
        }
    }

    Ok(())
}

/// Grid section title: the bare label at `-v` (e.g. `Moods`, `idea`,
/// `@name`); the ` ({period:?})` suffix only at `-vv` and above.
fn grid_title(label: &str, period: TrackerPeriod, verbose_level: u8) -> String {
    if verbose_level >= 2 {
        format!("{label} ({period:?})")
    } else {
        label.to_string()
    }
}

async fn display_mood_tracker<W: Write>(
    pool: &SqlitePool,
    config: &Config,
    start_epoch: i64,
    end_epoch: i64,
    period: TrackerPeriod,
    out: &mut W,
) -> Result<()> {
    // Fetch mood entries in the period, grouped by day. Journal-only entries
    // (empty mood → no embedding) are excluded from the grid.
    let feelings: Vec<crate::sql::FeelingRow> =
        crate::sql::fetch_feelings_between(pool, start_epoch, end_epoch)
            .await?
            .into_iter()
            .filter(|f| !f.mood.is_empty())
            .collect();

    if feelings.is_empty() {
        writeln!(out, "No mood entries in this period.")?;
        return Ok(());
    }

    let embedder = crate::embed::global_embedder();
    let axes = config.moods.color_axes.as_ref().unwrap();
    let mut color_cache: std::collections::HashMap<String, oklab::Oklab> =
        std::collections::HashMap::new();

    // Group colors by day. Prefer the stored embedding BLOB; fall back to
    // embedding the mood text when the model is available.
    let day_secs: i64 = 86400;
    let num_days = ((end_epoch - start_epoch) / day_secs + 1) as usize;
    let mut day_colors: Vec<Vec<oklab::Oklab>> = vec![Vec::new(); num_days];
    let mut day_has_entry: Vec<bool> = vec![false; num_days];

    for f in &feelings {
        let time = f.time;
        let day_idx = ((time - start_epoch) / day_secs) as usize;
        if day_idx >= num_days {
            continue;
        }
        day_has_entry[day_idx] = true;

        let oklab = axes
            .mood_color_cached(pool, embedder, f, &mut color_cache)
            .await;

        if let Some(oklab) = oklab {
            day_colors[day_idx].push(oklab);
        }
    }

    // The grid body follows; the section title (if any) is printed by
    // handle_tracker.

    // Year grids always use the heatmap layout: one column per week, one
    // row per weekday (rows start at grid.week_start), dots from the
    // window start through today.
    if period == TrackerPeriod::Year {
        render_year_heatmap(out, &day_colors, &day_has_entry, start_epoch, config)?;
        return Ok(());
    }

    // Print dots: colored by the average mood color of each day when
    // available, otherwise a plain filled dot for days with an entry and ◯
    // for empty days. Dots are separated by two spaces and wrap at 7 per
    // row (the last row may be short, e.g. a month that ends mid-week).
    for (i, colors) in day_colors.iter().enumerate() {
        let d = if !day_has_entry[i] {
            "◯".to_string()
        } else if let Some(oklab) = crate::color::average_oklab(colors) {
            "●"
                .with(crate::color_conversion::oklab_to_crossterm(oklab))
                .to_string()
        } else {
            "●".to_string()
        };
        write!(out, "{}", d)?;
        if (i + 1) % 7 == 0 || i == num_days - 1 {
            writeln!(out)?;
        } else {
            write!(out, "  ")?;
        }
    }

    Ok(())
}

/// Year heatmap: one column per week, one row per weekday (rows start at
/// `grid.week_start`, so Monday is the top row by default). The window
/// runs from `start_epoch` through today — the calendar year (Jan 1) when
/// `grid.year_rolling` is false, otherwise the calendar year aligned to
/// a full week start (the week_start on or before Jan 1, so the grid never
/// opens with blank cells in the first column). Days before Jan 1 (when
/// year_rolling is true) render as single spaces. Days after today in the
/// last partial week also render as spaces. There is no horizontal spacing
/// between columns.
fn render_year_heatmap<W: Write>(
    out: &mut W,
    day_colors: &[Vec<oklab::Oklab>],
    day_has_entry: &[bool],
    start_epoch: i64,
    config: &Config,
) -> Result<()> {
    use chrono::{Datelike, Duration, TimeZone, Weekday};

    let week_start = config.grid.week_start;
    let start_date = chrono::Local
        .timestamp_opt(start_epoch, 0)
        .earliest()
        .map(|dt| dt.date_naive())
        .context("year heatmap: start_epoch is not a valid local date")?;
    let today = chrono::Local::now().date_naive();
    let jan1 = today.with_ordinal(1).unwrap(); // Jan 1 of current year

    // Row = weekday offset from week_start; a week ends the day before
    // week_start (mirrors the subrepo's week_end_day).
    let weekday_row = |wd: Weekday| -> usize {
        let start_num = week_start.num_days_from_monday();
        let wd_num = wd.num_days_from_monday();
        ((wd_num + 7 - start_num) % 7) as usize
    };
    let week_end = match week_start {
        Weekday::Mon => Weekday::Sun,
        Weekday::Sun => Weekday::Sat,
        other => other.pred(),
    };

    // One column per real week; None marks days outside Jan 1..=today.
    let mut weeks: Vec<[Option<usize>; 7]> = Vec::new();
    let mut week: [Option<usize>; 7] = [None; 7];
    let mut date = start_date;
    let mut day = 0usize;
    while date <= today {
        week[weekday_row(date.weekday())] = Some(day);
        if date.weekday() == week_end || date == today {
            weeks.push(week);
            week = [None; 7];
        }
        day += 1;
        date += Duration::days(1);
    }

    for row in 0..7 {
        for w in &weeks {
            match w[row] {
                Some(day) => {
                    let day_date = start_date + Duration::days(day as i64);
                    if day_date < jan1 {
                        // Day is before Jan 1 (previous year), render as space
                        write!(out, " ")?;
                    } else if !day_has_entry[day] {
                        write!(out, "·")?;
                    } else if let Some(oklab) = crate::color::average_oklab(&day_colors[day]) {
                        write!(
                            out,
                            "{}",
                            "●".with(crate::color_conversion::oklab_to_crossterm(oklab))
                        )?;
                    } else {
                        write!(out, "●")?;
                    }
                }
                None => write!(out, " ")?,
            }
        }
        writeln!(out)?;
    }

    Ok(())
}

async fn display_custom_tracker<W: Write>(
    pool: &SqlitePool,
    config: &Config,
    tracker_type: &str,
    start_epoch: i64,
    end_epoch: i64,
    period: TrackerPeriod,
    opts: &CliOpts,
    out: &mut W,
    wrap: Option<usize>,
) -> Result<()> {
    let tracker = config.tracker.get(tracker_type).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown custom tracker '{}' not found in config",
            tracker_type
        )
    })?;

    // Fetch all entries in the period
    let entries =
        crate::sql::fetch_customs_for_tracker(pool, tracker_type, start_epoch, end_epoch).await?;

    if entries.is_empty() {
        writeln!(
            out,
            "No entries for tracker '{}' in this period.",
            tracker_type
        )?;
        return Ok(());
    }

    // Text trackers list their entries as indented lines instead of dots;
    // at -v each line gains the entry's own timestamp in Darkgray.
    if tracker.kind == TrackerType::Text {
        for entry in &entries {
            write!(out, "{}", "> ".with(CtColor::DarkGrey))?;
            write!(out, "{}", entry.score)?;
            if opts.verbose() {
                writeln!(
                    out,
                    "{}",
                    format!(" [{}]", crate::date::format_datetime_short(entry.time))
                        .with(CtColor::DarkGrey)
                )?;
            } else {
                writeln!(out)?;
            }
        }
        return Ok(());
    }

    // If the tracker defines an interval, render one dot per interval slot;
    // otherwise one dot per entry (newer entry wins the slot).
    if let Some(interval) = tracker.interval {
        let interval_secs = interval;
        let num_slots = ((end_epoch - start_epoch) / interval_secs + 1) as usize;
        let mut slot_sums: Vec<f64> = vec![0.0; num_slots];
        let mut slot_has_entry: Vec<bool> = vec![false; num_slots];

        for entry in &entries {
            let score = score_f64(&entry.score);
            let time = entry.time;
            let idx = ((time - start_epoch) / interval_secs) as usize;
            if idx < num_slots {
                slot_sums[idx] += score;
                slot_has_entry[idx] = true;
            }
        }

        let nonzero_sums: Vec<f64> = slot_sums
            .iter()
            .zip(slot_has_entry.iter())
            .filter_map(|(&sum, &has)| {
                if has && sum.abs() > f64::EPSILON {
                    Some(sum)
                } else {
                    None
                }
            })
            .collect();

        let (eff_min, eff_max) = effective_range(tracker.min, tracker.max, &nonzero_sums);

        let use_circle = period != TrackerPeriod::Year;
        for (i, &has_entry) in slot_has_entry.iter().enumerate() {
            if !has_entry {
                if use_circle {
                    write!(out, "◯")?;
                } else {
                    write!(out, "·")?;
                }
            } else {
                let color = match (eff_min, eff_max) {
                    (Some(min), Some(max)) if (max - min).abs() > f64::EPSILON => {
                        // Normal binning
                        let t = if min < max {
                            ((slot_sums[i] - min) / (max - min)).clamp(0.0, 1.0)
                        } else {
                            // Inverted range (min > max): lower score → success
                            ((min - slot_sums[i]) / (min - max)).clamp(0.0, 1.0)
                        };
                        let idx = ((t * (config.tasks.colors.len() as f64 - 1.0)).round() as usize)
                            .min(config.tasks.colors.len() - 1);
                        config.tasks.colors[idx]
                    }
                    _ => {
                        // Both missing or min==max: use last color
                        *config.tasks.colors.last().unwrap()
                    }
                };
                write!(out, "{}", "●".with(color))?;
            }
            if let Some(w) = wrap {
                if (i + 1) % w == 0 || i == num_slots - 1 {
                    writeln!(out)?;
                } else {
                    write!(out, "  ")?;
                }
            } else if i < num_slots - 1 {
                write!(out, "  ")?;
            }
        }
        writeln!(out)?;
    } else {
        // One dot per entry
        let scores: Vec<f64> = entries.iter().map(|e| score_f64(&e.score)).collect();

        let nonzero_scores: Vec<f64> = scores
            .iter()
            .filter(|&&s| s.abs() > f64::EPSILON)
            .cloned()
            .collect();

        let (eff_min, eff_max) = effective_range(tracker.min, tracker.max, &nonzero_scores);

        for &score in &scores {
            let color = match (eff_min, eff_max) {
                (Some(min), Some(max)) if (max - min).abs() > f64::EPSILON => {
                    // Normal binning
                    let t = if min < max {
                        ((score - min) / (max - min)).clamp(0.0, 1.0)
                    } else {
                        // Inverted range (min > max): lower score → success
                        ((min - score) / (min - max)).clamp(0.0, 1.0)
                    };
                    let idx = ((t * (config.tasks.colors.len() as f64 - 1.0)).round() as usize)
                        .min(config.tasks.colors.len() - 1);
                    config.tasks.colors[idx]
                }
                _ => {
                    // Both missing or min==max: use last color
                    *config.tasks.colors.last().unwrap()
                }
            };
            write!(out, "{}", "●".with(color))?;
        }
        writeln!(out)?;
    }
    Ok(())
}

/// Map a custom-tracker score to a color by binning it across task_color.colors.
/// Handles inverted ranges (max < min → smaller values get the success color).
fn bin_score_color(
    config: &Config,
    tracker: &crate::config::TrackerSetting,
    score: f64,
) -> CtColor {
    let colors = &config.tasks.colors;

    let (min, max) = (tracker.min, tracker.max);

    let t = match (min, max) {
        (Some(min), Some(max)) if (max - min).abs() > f64::EPSILON => {
            if min < max {
                // normal: higher score → success
                ((score - min) / (max - min)).clamp(0.0, 1.0)
            } else {
                // Inverted range (min > max): lower score → success
                ((min - score) / (min - max)).clamp(0.0, 1.0)
            }
        }
        _ => 0.5,
    };

    let idx = ((t * (colors.len() as f64 - 1.0)).round() as usize).min(colors.len() - 1);
    colors[idx]
}

async fn display_recurring_tracker<W: Write>(
    pool: &SqlitePool,
    config: &Config,
    name: &str,
    start_epoch: i64,
    end_epoch: i64,
    period: TrackerPeriod,
    out: &mut W,
    wrap: Option<usize>,
) -> Result<()> {
    // Find the recurring task: id can be numeric or the unique task name.
    let Some(task) = crate::sql::fetch_recurring_task_meta(pool, name).await? else {
        writeln!(out, "Recurring task '{}' not found.", name)?;
        return Ok(());
    };

    let task_id = task.id;
    let interval_secs = task.interval_secs;
    let target_count = task.target_count;

    // Get completion events (time, count) for this task in the period
    let completions =
        crate::sql::fetch_completions_between(pool, task_id, start_epoch, end_epoch).await?;

    if let Some(interval) = interval_secs {
        // For interval-based recurring tasks, show dots per interval,
        // summing the per-event counts into each interval.
        let num_intervals = ((end_epoch - start_epoch) / interval + 1) as usize;
        let mut interval_sums: Vec<i64> = vec![0; num_intervals];

        for completion in &completions {
            let ctime = completion.time;
            let count = completion.count;
            let idx = ((ctime - start_epoch) / interval) as usize;
            if idx < num_intervals {
                interval_sums[idx] += count;
            }
        }

        for (i, sum) in interval_sums.iter().enumerate() {
            let (ch, color) = completion_badge(config, *sum, target_count);
            let d = if ch == '◯' && period == TrackerPeriod::Year {
                "·".to_string()
            } else if color == CtColor::Reset {
                ch.to_string()
            } else {
                ch.to_string().with(color).to_string()
            };
            write!(out, "{}", d)?;
            if let Some(w) = wrap {
                if (i + 1) % w == 0 || i == num_intervals - 1 {
                    writeln!(out)?;
                } else {
                    write!(out, "  ")?;
                }
            } else if i < num_intervals - 1 {
                write!(out, "  ")?;
            }
        }
        if wrap.is_none() {
            writeln!(out)?;
        }
    } else {
        // No interval: one dot per completion event, colored by its count
        if completions.is_empty() {
            writeln!(out, "No completions for '{}' in this period.", name)?;
            return Ok(());
        }

        for completion in &completions {
            let count = completion.count;
            let (ch, color) = completion_badge(config, count, target_count);
            if color == CtColor::Reset {
                write!(out, "{}", ch)?;
            } else {
                write!(out, "{}", ch.to_string().with(color))?;
            }
        }
        writeln!(out)?;
    }

    Ok(())
}

/// Today-view time cell for a timestamp: "HH:MM" when it falls on the
/// anchored day, "Tu HH:MM" (two-letter weekday prefix) when it falls
/// within a week of it — entries outside the anchored day stay
/// distinguishable in the +tomorrow/+week horizons — and the short
/// datetime form ("YYYY-MM-DD HH:MM") outside that week entirely.
fn today_time_label(time: i64, day_start_epoch: i64) -> String {
    if time < day_start_epoch || time > day_start_epoch + 7 * 86_400 {
        crate::date::format_datetime_short(time)
    } else if crate::date::day_start(time) == day_start_epoch {
        crate::date::format_time(time)
    } else {
        format!(
            "{} {}",
            crate::date::format_weekday(time),
            crate::date::format_time(time)
        )
    }
}

/// The today-view timestamp for a task row: the sort key and the time
/// shown in the time cell. Done tasks show their completion time (the last
/// completion entry; an entry-less auto-completed scheduled task falls back
/// to its window end — `start + duration`). Not-done: scheduled →
/// `start_time`; recurring → the availability-window end of the current
/// interval (the implicit next-interval start when there's no explicit
/// duration — the untimed group); oneshot → the due time (`end_time`, else
/// untimed). Untimed rows return `i64::MAX` and sort after all timed
/// entries (`today_sort` groups by empty time cell). Shared with the tasks
/// app's pending-view sort (`render/tasks.rs`); the done view sorts by
/// [`task_done_time`] instead.
pub(crate) fn task_entry_time(task: &TaskRow, now: i64) -> i64 {
    if task.is_done() {
        return task.last_time.unwrap_or_else(|| {
            // Auto-completed scheduled (no entry): the completion moment.
            task.start_time
                .unwrap_or(i64::MAX)
                .saturating_add(task.available_duration_secs.unwrap_or(0))
        });
    }
    if task.is_scheduled() {
        // Schedule start
        return task.start_time.unwrap_or(i64::MAX);
    }
    if task.is_recurring() {
        return recurring_window_end(task, now);
    }
    // Oneshot, not done: the due time; undated oneshots are untimed.
    task.end_time.unwrap_or(i64::MAX)
}

/// The done-view sort key ("done time"): the last completion entry, else
/// `start + duration` for scheduled rows — `@done` date sort and the
/// priority-mode equal-priority fallback use it in reverse (newest first).
/// The fallback covers entry-less rows: auto-completed scheduled tasks
/// complete at their window end, while zero-entry recurring history rows
/// in `@done:b` fall back to `start_time` only (their
/// `available_duration_secs` is the per-interval availability window, not
/// a completion moment). Only used for sorting `@done` lists, where every
/// row either has an entry or is auto-completed, so a "no done time" row
/// can't occur. Mirrors the done SQL ordering
/// `COALESCE(MAX(tc.time), CASE WHEN interval_secs IS NULL THEN
/// start_time + COALESCE(available_duration_secs, 0) ELSE start_time END)`.
pub(crate) fn task_done_time(task: &TaskRow) -> i64 {
    if let Some(last) = task.last_time {
        return last;
    }
    let start = task.start_time.unwrap_or(i64::MAX);
    if task.interval_secs.is_none() {
        // Scheduled: auto-completed at the window end.
        start.saturating_add(task.available_duration_secs.unwrap_or(0))
    } else {
        // Recurring history row (zero entries): the start time.
        start
    }
}

/// The today-view time cell for a task row: "HH:MM" (weekday prefix when
/// outside the anchored day) for timed rows — completion time when done,
/// otherwise the task's deadline/availability end — and empty for the
/// untimed group (undated oneshots, recurring tasks without an explicit
/// duration window).
fn task_time_label(task: &TaskRow, time: i64, day_start_epoch: i64) -> String {
    if task.is_done() {
        return today_time_label(time, day_start_epoch);
    }
    if task.is_recurring() && task.available_duration_secs.is_none() {
        return String::new();
    }
    if !task.is_scheduled() && !task.is_recurring() && task.end_time.is_none() {
        // Undated oneshot.
        return String::new();
    }
    today_time_label(time, day_start_epoch)
}

/// End of the availability window in the current interval: `interval_start
/// + available_duration_secs`; a task without an explicit duration is
/// available for the whole interval, so its implicit window end is the
/// next interval start.
fn recurring_window_end(task: &TaskRow, now: i64) -> i64 {
    match (
        task.start_time,
        task.interval_secs,
        task.available_duration_secs,
    ) {
        (Some(st), Some(interval), Some(dur)) if dur < interval => {
            crate::task::current_interval_start(st, interval, now) + dur
        }
        (Some(st), Some(interval), _) => {
            crate::task::current_interval_start(st, interval, now) + interval
        }
        // Defensive: interval-less recurring row (the fetch guarantees
        // interval_secs IS NOT NULL) — fall back to the anchor.
        _ => task.start_time.unwrap_or(now),
    }
}

/// Today-view sort: timed entries first (by timestamp ascending); then the
/// no-time group (all-day recurring tasks, undated oneshots) by priority
/// descending, then by untruncated availability end ascending.
pub(crate) fn today_sort(a: &TodayEntry, b: &TodayEntry) -> std::cmp::Ordering {
    let (a_blank, b_blank) = (a.time_label.is_empty(), b.time_label.is_empty());
    match (a_blank, b_blank) {
        (false, true) => std::cmp::Ordering::Less,
        (true, false) => std::cmp::Ordering::Greater,
        (false, false) => a.time.cmp(&b.time),
        (true, true) => b.priority.cmp(&a.priority).then(a.time.cmp(&b.time)),
    }
}

/// Fetch all today-view entries within the given horizon.
///
/// All variants share the same task base — tasks active at any point
/// during the period (interval-aware availability-window overlap for
/// recurring). `show` selects what rides on top: `All` also merges tasks
/// with a completion today (time = last completion); `A` filters completed
/// tasks out and shows no completions; `B` is the same as `All` but
/// tasks-only (no feelings/customs) and carries `coalesce_completions`
/// (D11 — no behavior yet). See docs/VIEWS.md.
pub async fn fetch_today_entries(
    pool: &SqlitePool,
    config: &Config,
    horizon: TodayHorizon,
    day_epoch: Option<i64>,
    show: ShowVariant,
    color_cache: &mut std::collections::HashMap<String, oklab::Oklab>,
) -> Result<Vec<TodayEntry>> {
    // `feeling @<date>` anchors the day; bare `feeling` is today.
    let day_start_epoch = day_epoch.unwrap_or_else(date::today_start);
    let day_end_epoch = date::day_end(day_start_epoch);
    let horizon_end = horizon.end_epoch(day_start_epoch);
    let now_ts = date::now();

    let mut entries: Vec<TodayEntry> = Vec::new();

    // B is tasks-only: no feelings, no custom tracker entries.
    if show != ShowVariant::B {
        let embedder = crate::embed::global_embedder();
        let axes = config.moods.color_axes.as_ref().unwrap();

        // 1. Today's feelings
        let feelings =
            crate::sql::fetch_feelings_between(pool, day_start_epoch, day_end_epoch).await?;

        for f in feelings {
            // Journal-only entries (empty mood) use the configured journal
            // badge, or none at all; mood entries always get the filled dot.
            let badge = if f.mood.is_empty() {
                config.today_view.journal_badge
            } else {
                Some('●')
            };

            // Resolve this entry's embedding → color (cached per mood; legacy
            // rows without a stored embedding are re-embedded + backfilled).
            let oklab = axes
                .mood_color_cached(pool, embedder, &f, color_cache)
                .await;

            let id = f.id;
            let mood = f.mood;
            let body = f.body;
            let time = f.time;
            let color = oklab
                .map(|oklab| {
                    let rgb = oklab.to_srgb();
                    RatColor::Rgb(rgb.r, rgb.g, rgb.b)
                })
                .unwrap_or(RatColor::DarkGray);
            entries.push(TodayEntry {
                id: Some(id),
                time,
                time_label: today_time_label(time, day_start_epoch),
                kind: if mood.is_empty() {
                    EntryKind::Journal
                } else {
                    EntryKind::Mood
                },
                label: mood,
                body,
                task_id: None,
                priority: 0,
                badge,
                color,
            });
        }

        // 2. Today's custom tracker entries
        let customs = crate::sql::fetch_customs_today(pool, day_start_epoch, day_end_epoch).await?;

        for row in customs {
            let custom_id = row.id;
            let tracker_type = row.tracker_type;
            let time = row.time;
            let tracker = config.tracker.get(&tracker_type).ok_or_else(|| {
                anyhow::anyhow!(
                    "Unknown custom tracker '{}' not found in config",
                    tracker_type
                )
            })?;
            let (label, badge, score) = match tracker.kind {
                // Text payloads have no score; they use the shared text badge.
                TrackerType::Text => (
                    format!("{}: {}", tracker_type, row.score),
                    Some(TEXT_ENTRY_BADGE),
                    None,
                ),
                TrackerType::Number | TrackerType::Float => {
                    let score = score_f64(&row.score);
                    (
                        format!("{}: {}", tracker_type, score),
                        Some('◆'),
                        Some(score),
                    )
                }
            };
            let color = match score {
                Some(s) => RatColor::from_crossterm(bin_score_color(config, tracker, s)),
                None => RatColor::DarkGray,
            };
            entries.push(TodayEntry {
                id: Some(custom_id),
                time,
                time_label: today_time_label(time, day_start_epoch),
                kind: EntryKind::Custom,
                label,
                body: String::new(),
                task_id: None,
                priority: 0,
                badge,
                color,
            });
        }
    } // show != ShowVariant::B

    // 3. Oneshot tasks due by the end of the horizon (due time — `end_time`
    // when set, else the legacy `start_time` — <= horizon_end). This upper
    // bound alone would also match overdue tasks (due before today), so
    // unless config.today_view.include_overdue is set, a lower bound keeps
    // only tasks due from today onward. The floor is bound (i64::MIN =
    // effectively no filter) so the SQL stays static.
    let overdue_floor = if config.today_view.include_overdue {
        i64::MIN
    } else {
        day_start_epoch
    };
    let due_tasks = crate::sql::fetch_due_oneshot_tasks(pool, horizon_end, overdue_floor).await?;

    for task in &due_tasks {
        // A filters completed tasks out.
        if show == ShowVariant::A && task.is_done() {
            continue;
        }
        // Time: done → completion time; else the due time (`end_time` when
        // set — `! name @<time>`; undated oneshots are untimed).
        let time = task_entry_time(task, now_ts);
        let time_label = task_time_label(task, time, day_start_epoch);
        let (badge, color) = crate::badge::task_badge(task, config, false);
        entries.push(TodayEntry {
            id: None,
            time,
            time_label,
            kind: if task.target_count > 0 {
                EntryKind::Threshold
            } else {
                EntryKind::Oneshot
            },
            label: task.name.clone(),
            body: task.body.clone(),
            task_id: Some(task.id),
            priority: task.priority,
            // The badge rules (✓ done / ○ not done, overdue coloring) live
            // in badge::task_badge — see docs/BADGE.md.
            badge: Some(badge),
            color: RatColor::from_crossterm(color),
        });
    }

    // 3b. Scheduled tasks overlapping the horizon (window overlap: started
    // before horizon_end, still open past today_start). All states show —
    // ongoing, completed / auto-completed, failed — with the same badge
    // semantics as the tasks app.
    let scheduled_tasks =
        crate::sql::fetch_scheduled_today(pool, horizon_end, day_start_epoch).await?;

    for task in &scheduled_tasks {
        // A filters completed tasks out (incl. auto-completed).
        if show == ShowVariant::A && task.is_done() {
            continue;
        }
        let (badge, color) = crate::badge::task_badge(task, config, false);
        // Time: done → completion time (auto-completed has no entry, so it
        // falls back to the window end); else `start_time`.
        let time = task_entry_time(task, now_ts);
        let time_label = task_time_label(task, time, day_start_epoch);
        entries.push(TodayEntry {
            id: None,
            time,
            time_label,
            kind: EntryKind::Scheduled,
            label: task.name.clone(),
            body: task.body.clone(),
            task_id: Some(task.id),
            priority: task.priority,
            badge: Some(badge),
            color: RatColor::from_crossterm(color),
        });
    }

    // 4. Recurring tasks active at any point during the period (all
    // variants; interval-aware availability-window overlap — VIEWS.md).
    let recurring_tasks =
        crate::sql::fetch_recurring_tasks_for_period(pool, day_start_epoch, horizon_end).await?;

    for task in &recurring_tasks {
        // A filters completed tasks out.
        if show == ShowVariant::A && task.is_done() {
            continue;
        }
        // Time: done → completion time (current-interval scoped); else the
        // availability-window end rule.
        let time = task_entry_time(task, now_ts);
        let time_label = task_time_label(task, time, day_start_epoch);
        let (badge, color) = crate::badge::task_badge(task, config, false);
        entries.push(TodayEntry {
            id: None,
            time,
            time_label,
            kind: EntryKind::Recurring,
            label: task.name.clone(),
            body: task.body.clone(),
            task_id: Some(task.id),
            priority: task.priority,
            badge: Some(badge),
            color: RatColor::from_crossterm(color),
        });
    }

    // 5. Tasks with a completion entry today (All and B — B is the same as
    // All minus the feelings/customs sections): merged over the regular
    // rows (dedup by task_id — the completed-today row wins, time = last
    // completion timestamp) so a task completed today shows its completion
    // time even when it is no longer active (or not in the regular lists
    // at all). `A` filters completed tasks out, so the fetch is skipped
    // there.
    if show != ShowVariant::A {
        let completed_today =
            crate::sql::fetch_tasks_completed_on(pool, day_start_epoch, day_end_epoch).await?;
        for task in &completed_today {
            let last_time = task.last_time.unwrap_or(now_ts);
            let (badge, color) = crate::badge::task_badge(task, config, false);
            let entry = TodayEntry {
                id: None,
                time: last_time,
                time_label: today_time_label(last_time, day_start_epoch),
                kind: if task.is_recurring() {
                    EntryKind::Recurring
                } else if task.is_scheduled() {
                    EntryKind::Scheduled
                } else if task.target_count > 0 {
                    EntryKind::Threshold
                } else {
                    EntryKind::Oneshot
                },
                label: task.name.clone(),
                body: task.body.clone(),
                task_id: Some(task.id),
                priority: task.priority,
                badge: Some(badge),
                color: RatColor::from_crossterm(color),
            };
            match entries.iter_mut().find(|e| e.task_id == Some(task.id)) {
                Some(existing) => *existing = entry,
                None => entries.push(entry),
            }
        }
    }

    // Sort: timed entries first by timestamp, then the no-time group by
    // priority descending and untruncated availability end.
    entries.sort_by(today_sort);

    Ok(entries)
}

/// Handle today view (non-terminal output): displays today's feelings, custom
/// entries, and task activity as tab-separated rows. TUI dispatch is handled by
/// [`crate::handlers::handle_command`].
pub async fn handle_today<W: Write>(
    pool: &SqlitePool,
    config: &Config,
    day_epoch: Option<i64>,
    show: ShowVariant,
    horizon: TodayHorizon,
    _opts: &CliOpts,
    out: &mut W,
) -> Result<()> {
    let mut color_cache = std::collections::HashMap::new();
    let entries =
        fetch_today_entries(pool, config, horizon, day_epoch, show, &mut color_cache).await?;

    if entries.is_empty() {
        writeln!(out, "Nothing logged today.")?;
        return Ok(());
    }

    write!(out, "{}", crate::display::format_today_simple(&entries))?;
    Ok(())
}

/// Handle a task view (non-terminal output): writes tab-separated rows to the
/// writer. TUI dispatch is handled by [`crate::handlers::handle_command`].
pub async fn handle_view<W: Write>(
    pool: &SqlitePool,
    mode: ViewMode,
    config: &Config,
    show: ShowVariant,
    out: &mut W,
) -> Result<()> {
    let mut tasks = crate::sql::fetch_tasks_for_view(
        pool,
        mode,
        show,
        config.tasks_view.persist_pending_seconds,
    )
    .await?;

    // CLI ordering uses the same date keys as the TUIs: pending views sort
    // priority descending with `task_entry_time` (date ascending) as the
    // fallback; the done view sorts by `task_done_time` (last completion
    // entry, else start + duration) newest first. The SQL ORDER BY only
    // provides a deterministic base for equal keys.
    let now = crate::date::now();
    if mode == ViewMode::DoneTasks {
        // Date sort: done time, newest first.
        tasks.sort_by_key(|t| std::cmp::Reverse(task_done_time(t)));
    } else {
        // Priority sort with the date key as fallback (ascending).
        tasks.sort_by_key(|t| (std::cmp::Reverse(t.priority), task_entry_time(t, now)));
    }

    if tasks.is_empty() {
        writeln!(out, "No tasks found for view: {:?}", mode)?;
        return Ok(());
    }

    write!(
        out,
        "{}",
        crate::display::format_tasks_simple(&tasks, config, mode == ViewMode::DoneTasks)
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grid_title() {
        // Bare label at -v; period suffix from -vv (and -vvv).
        assert_eq!(grid_title("Moods", TrackerPeriod::Week, 0), "Moods");
        assert_eq!(grid_title("Moods", TrackerPeriod::Week, 1), "Moods");
        assert_eq!(grid_title("Moods", TrackerPeriod::Week, 2), "Moods (Week)");
        assert_eq!(grid_title("idea", TrackerPeriod::Month, 2), "idea (Month)");
        assert_eq!(grid_title("@run", TrackerPeriod::Year, 3), "@run (Year)");
        assert_eq!(grid_title("idea", TrackerPeriod::Month, 1), "idea");
    }

    #[test]
    fn test_today_time_label() {
        // 2024-03-15 is a Friday; 2024-03-16 a Saturday.
        let day =
            crate::date::parse_datetime("2024-03-15 00:00", crate::date::DateDialect::Uk).unwrap();
        let same =
            crate::date::parse_datetime("2024-03-15 09:30", crate::date::DateDialect::Uk).unwrap();
        let next =
            crate::date::parse_datetime("2024-03-16 09:30", crate::date::DateDialect::Uk).unwrap();
        assert_eq!(today_time_label(same, day), "09:30");
        assert_eq!(today_time_label(next, day), "Sa 09:30");
        // Outside the week window → short datetime form.
        let far =
            crate::date::parse_datetime("2024-03-25 09:30", crate::date::DateDialect::Uk).unwrap();
        let early =
            crate::date::parse_datetime("2024-03-01 09:30", crate::date::DateDialect::Uk).unwrap();
        assert_eq!(today_time_label(far, day), "2024-03-25 09:30");
        assert_eq!(today_time_label(early, day), "2024-03-01 09:30");
        // The weekday form covers days 1-6 after the anchor; the 7th day
        // (>= day_start + week) is already the short form.
        let within =
            crate::date::parse_datetime("2024-03-21 09:30", crate::date::DateDialect::Uk).unwrap();
        assert_eq!(today_time_label(within, day), "Th 09:30");
        let boundary =
            crate::date::parse_datetime("2024-03-22 00:00", crate::date::DateDialect::Uk).unwrap();
        assert_eq!(today_time_label(boundary, day), "Fr 00:00");
    }

    fn task_row(
        start_time: Option<i64>,
        available_duration_secs: Option<i64>,
        interval_secs: Option<i64>,
        end_time: Option<i64>,
        completions: Option<i32>,
        last_time: Option<i64>,
    ) -> TaskRow {
        TaskRow {
            id: 1,
            short_id: Some(1),
            name: "t".to_string(),
            body: String::new(),
            priority: 5,
            start_time,
            available_duration_secs,
            interval_secs,
            target_count: 0,
            optional: 0,
            end_time,
            completions,
            last_time,
        }
    }

    #[test]
    fn test_task_entry_time() {
        let day =
            crate::date::parse_datetime("2024-03-16 00:00", crate::date::DateDialect::Uk).unwrap();
        let anchor =
            crate::date::parse_datetime("2024-03-15 08:00", crate::date::DateDialect::Uk).unwrap();
        let now =
            crate::date::parse_datetime("2024-03-16 14:00", crate::date::DateDialect::Uk).unwrap();
        let at = |s: &str| crate::date::parse_datetime(s, crate::date::DateDialect::Uk).unwrap();
        let day_secs = 86400;
        let hour_secs = 3600;

        let check = |task: &TaskRow, expect_time: i64, expect_label: &str| {
            let time = task_entry_time(task, now);
            let label = task_time_label(task, time, day);
            assert_eq!(time, expect_time, "time for {}", task.name);
            assert_eq!(label, expect_label, "label for {}", task.name);
        };

        // Recurring with an availability window: ends 09:00 in the current
        // interval (16th), same day as the anchor — no weekday prefix.
        check(
            &task_row(
                Some(anchor),
                Some(hour_secs),
                Some(day_secs),
                None,
                None,
                None,
            ),
            at("2024-03-16 09:00"),
            "09:00",
        );
        // Recurring without one: implicit end = next interval start, empty
        // time cell (the untimed group).
        check(
            &task_row(Some(anchor), None, Some(day_secs), None, None, None),
            at("2024-03-17 08:00"),
            "",
        );
        // Scheduled, not done (window still open): the deadline.
        check(
            &task_row(
                Some(at("2024-03-16 08:00")),
                Some(10 * hour_secs),
                None,
                None,
                None,
                None,
            ),
            at("2024-03-16 18:00"),
            "18:00",
        );
        // Scheduled, done with an entry: the completion time.
        check(
            &task_row(
                Some(at("2024-03-16 08:00")),
                Some(10 * hour_secs),
                None,
                None,
                Some(1),
                Some(at("2024-03-16 13:30")),
            ),
            at("2024-03-16 13:30"),
            "13:30",
        );
        // Scheduled, auto-completed (no entry, window elapsed): the window
        // end is the completion moment.
        check(
            &task_row(
                Some(at("2024-03-16 08:00")),
                Some(2 * hour_secs),
                None,
                None,
                None,
                None,
            ),
            at("2024-03-16 10:00"),
            "10:00",
        );
        // Oneshot, not done, with a due time.
        check(
            &task_row(
                Some(anchor),
                None,
                None,
                Some(at("2024-03-16 12:00")),
                None,
                None,
            ),
            at("2024-03-16 12:00"),
            "12:00",
        );
        // Oneshot, not done, undated: untimed (sorts last).
        check(
            &task_row(Some(anchor), None, None, None, None, None),
            i64::MAX,
            "",
        );
        // Oneshot, done: the completion time.
        check(
            &task_row(
                Some(anchor),
                None,
                None,
                Some(at("2024-03-16 12:00")),
                Some(1),
                Some(at("2024-03-16 13:00")),
            ),
            at("2024-03-16 13:00"),
            "13:00",
        );

        // `@done:b` partial history: recurring with target 2, one entry ever
        // — not done, so the pending key is the window end; the done-view
        // key is the last completion entry.
        let partial = TaskRow {
            name: "partial history".to_string(),
            target_count: 2,
            ..task_row(
                Some(anchor),
                Some(hour_secs),
                Some(day_secs),
                None,
                Some(1),
                Some(at("2024-03-16 13:00")),
            )
        };
        assert_eq!(
            task_entry_time(&partial, now),
            at("2024-03-16 09:00"),
            "pending view: window end (not done)"
        );
        assert_eq!(
            task_done_time(&partial),
            at("2024-03-16 13:00"),
            "done view: last completion entry"
        );
    }

    #[test]
    fn test_task_done_time() {
        let at = |s: &str| crate::date::parse_datetime(s, crate::date::DateDialect::Uk).unwrap();
        let day_secs = 86400;
        let hour_secs = 3600;

        // Done oneshot with an entry: the last completion entry.
        assert_eq!(
            task_done_time(&task_row(
                Some(at("2024-03-16 08:00")),
                None,
                None,
                None,
                Some(1),
                Some(at("2024-03-16 13:00")),
            )),
            at("2024-03-16 13:00")
        );
        // Scheduled with an entry: the entry.
        assert_eq!(
            task_done_time(&task_row(
                Some(at("2024-03-16 08:00")),
                Some(10 * hour_secs),
                None,
                None,
                Some(1),
                Some(at("2024-03-16 13:30")),
            )),
            at("2024-03-16 13:30")
        );
        // Scheduled without an entry (auto-completed): the window end.
        assert_eq!(
            task_done_time(&task_row(
                Some(at("2024-03-16 08:00")),
                Some(2 * hour_secs),
                None,
                None,
                None,
                None,
            )),
            at("2024-03-16 10:00")
        );
        // Recurring, zero entries (`@done:b` history row): falls back to
        // the start time only — `available_duration_secs` is the
        // per-interval availability window, not a completion moment.
        assert_eq!(
            task_done_time(&task_row(
                Some(at("2024-03-15 08:00")),
                Some(2 * hour_secs),
                Some(day_secs),
                None,
                None,
                None,
            )),
            at("2024-03-15 08:00")
        );
        // Undated: i64::MAX (defensive — can't appear in a done view).
        assert_eq!(
            task_done_time(&task_row(None, None, None, None, None, None)),
            i64::MAX
        );
    }

    #[test]
    fn test_today_sort() {
        let entry = |time: i64, time_label: &str, priority: i32| TodayEntry {
            id: None,
            time,
            time_label: time_label.to_string(),
            kind: EntryKind::Oneshot,
            label: String::new(),
            body: String::new(),
            task_id: None,
            priority,
            badge: None,
            color: RatColor::DarkGray,
        };
        let mut entries = [
            entry(200, "20:00", 1),
            entry(350, "", 5),
            entry(300, "", 2),
            entry(400, "", 5),
            entry(100, "10:00", 9),
        ];
        entries.sort_by(today_sort);
        let got: Vec<(i64, i32, String)> = entries
            .iter()
            .map(|e| (e.time, e.priority, e.time_label.clone()))
            .collect();
        // Timed entries first by timestamp; the no-time group then by
        // priority descending, then by untruncated availability end.
        assert_eq!(
            got,
            vec![
                (100, 9, "10:00".to_string()),
                (200, 1, "20:00".to_string()),
                (350, 5, String::new()),
                (400, 5, String::new()),
                (300, 2, String::new()),
            ]
        );
    }
}
