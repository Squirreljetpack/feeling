use anyhow::{Context, Result};
use crossterm::style::{Color as CtColor, Stylize};
use ratatui::backend::FromCrossterm;
use ratatui::style::Color as RatColor;
use sqlx::SqlitePool;
use std::io::Write;

use crate::clap::{TrackerItem, TrackerPeriod, ViewMode};
use crate::config::{Config, TrackerType};
use crate::date;

/// Badge for text-payload custom tracker entries wherever a marker is needed
/// (e.g. the today view). A named constant so the glyph can be adjusted later.
pub(crate) const TEXT_ENTRY_BADGE: char = '·';

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

    /// End of the horizon (inclusive) as epoch seconds.
    pub fn end_epoch(&self) -> i64 {
        match self {
            TodayHorizon::Today => date::today_end(),
            TodayHorizon::Tomorrow => date::day_end(date::today_start() + 86400),
            TodayHorizon::Week => date::day_end(date::week_sunday()),
        }
    }
}

/// Data for a single today-view entry.
#[derive(Debug, Clone)]
pub struct TodayEntry {
    pub id: Option<i64>,
    pub time: i64,
    pub entry_type: &'static str, // "feeling", "custom", "task", "completion"
    pub label: String,
    pub body: String,
    pub task_id: Option<i64>,
    pub priority: i32,
    /// Marker glyph rendered for this entry.
    pub badge: char,
    /// Dynamic dot color: Oklab mood projection for feeling entries,
    /// bin_score_color for numeric custom entries, completion_badge
    /// colors for tasks, last task_color for completion events, or a
    /// neutral dark gray for journal-only and text-tracker entries.
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
    period: TrackerPeriod,
    items: Vec<TrackerItem>,
    out: &mut W,
) -> Result<()> {
    let mut config = config.clone();
    config
        .moods
        .init_with(pool, crate::embed::global_embedder())
        .await?;
    let config = &config;
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

    for item in &items {
        match item {
            TrackerItem::Mood => {
                // Positional mood-grid marker: render the mood dots grid here.
                display_mood_tracker(pool, config, start_epoch, end_epoch, period, out).await?;
            }
            TrackerItem::Tracker(id_str) => {
                if let Some(name) = id_str.strip_prefix('@') {
                    // Recurring task: display completion dots
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
                    display_custom_tracker(
                        pool,
                        config,
                        id_str,
                        start_epoch,
                        end_epoch,
                        period,
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

    // Print header
    let header = match period {
        TrackerPeriod::Week => "Week",
        TrackerPeriod::Month => "Month",
        TrackerPeriod::Year => "Year",
    };
    writeln!(out, "Mood tracker ({})", header)?;

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
                        write!(out, "{}", "●".with(crate::color_conversion::oklab_to_crossterm(oklab)))?;
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
    let entries = crate::sql::fetch_customs_for_tracker(pool, tracker_type, start_epoch, end_epoch)
        .await?;

    if entries.is_empty() {
        writeln!(
            out,
            "No entries for tracker '{}' in this period.",
            tracker_type
        )?;
        return Ok(());
    }

    writeln!(out, "Tracker '{}' ({:?}):", tracker_type, period)?;

    // Text trackers list their entries as indented lines instead of dots.
    if tracker.kind == TrackerType::Text {
        for entry in &entries {
            write!(out, "{}", "> ".with(CtColor::DarkGrey))?;
            writeln!(out, "{}", entry.score)?;
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
                        let idx = ((t * (config.tasks.colors.len() as f64 - 1.0)).round()
                            as usize)
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
                    let idx = ((t * (config.tasks.colors.len() as f64 - 1.0)).round()
                        as usize)
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
fn bin_score_color(config: &Config, tracker: &crate::config::TrackerSetting, score: f64) -> CtColor {
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

        writeln!(out, "Task '{}' ({:?})", name, period)?;
        for (i, sum) in interval_sums.iter().enumerate() {
            // 0% (interval sum 0) → uncolored ◯, otherwise ● colored by binning.
            // Year grids are dense, so empty intervals use the compact · glyph.
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

        writeln!(out, "Task '{}' completions ({:?})", name, period)?;
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

/// Completion badge: (character, color) for a task's completion status.
///
/// - 0% (no entries, or per-interval sum 0) → ('◯', Reset), uncolored
/// - 100% (count >= target_count; any count when target_count <= 0) → ('●', last color)
/// - in between → ('●', binned into colors[..len-1] so the last color is
///   reserved exclusively for 100% completion). Binning only, no blending.
pub(crate) fn completion_badge(config: &Config, count: i64, target_count: i32) -> (char, CtColor) {
    let colors = &config.tasks.colors;
    if count <= 0 {
        return ('◯', CtColor::Reset);
    }
    if target_count <= 0 || count >= target_count as i64 {
        return ('●', *colors.last().unwrap());
    }
    // 0 < count < target_count: bin across colors[..len-1]
    if colors.len() <= 1 {
        return ('●', *colors.first().unwrap());
    }
    let n = colors.len() - 1;
    let t = count as f64 / target_count as f64;
    let idx = ((t * n as f64).round() as usize).min(n - 1);
    ('●', colors[idx])
}

/// Scheduled-task badge: (character, color) for a scheduled task's state.
///
/// - ongoing (window not yet elapsed, no entry) → ('◯', Reset)
/// - failed (entry 0) → ('●', colors[0])
/// - completed (entry >= 1, or no entry with the window elapsed —
///   auto-completed) → ('●', last color)
pub(crate) fn scheduled_badge(
    config: &Config,
    completions: Option<i32>,
    start_time: Option<i64>,
    available_duration: Option<i64>,
    now: i64,
) -> (char, CtColor) {
    let colors = &config.tasks.colors;
    match completions {
        Some(c) if c > 0 => ('●', *colors.last().unwrap_or(&CtColor::Reset)),
        Some(_) => ('●', *colors.first().unwrap_or(&CtColor::Reset)),
        None => match (start_time, available_duration) {
            (Some(st), Some(dur)) if st + dur < now => ('●', *colors.last().unwrap_or(&CtColor::Reset)),
            _ => ('◯', CtColor::Reset),
        },
    }
}

/// Text form of the completion badge: "● 2/5" (in progress), "●" alone (100%,
/// regardless of target_count), or "◯" alone (0%). Never shows "n/m" when
/// target_count <= 0. The 100% case dropped the "DONE" word per TODO; the
/// leading character matches `completion_badge`.
pub(crate) fn completion_badge_text(count: i64, target_count: i32) -> String {
    let ch = if count <= 0 { '◯' } else { '●' };
    if count > 0 && (target_count <= 0 || count >= target_count as i64) {
        ch.to_string()
    } else if count > 0 {
        // 0 < count < target_count (target_count > 0 here)
        format!("{} {}/{}", ch, count, target_count)
    } else {
        ch.to_string()
    }
}

/// Handle today view: displays today's feelings, custom entries, and task activity.
/// Fetch all today-view entries within the given horizon.
pub async fn fetch_today_entries(
    pool: &SqlitePool,
    config: &Config,
    horizon: TodayHorizon,
    color_cache: &mut std::collections::HashMap<String, oklab::Oklab>,
) -> Result<Vec<TodayEntry>> {
    let day_start_epoch = date::today_start();
    let day_end_epoch = date::today_end();
    let horizon_end = horizon.end_epoch();

    let mut entries: Vec<TodayEntry> = Vec::new();

    let embedder = crate::embed::global_embedder();
    let axes = config.moods.color_axes.as_ref().unwrap();

    // 1. Today's feelings
    let feelings = crate::sql::fetch_feelings_between(pool, day_start_epoch, day_end_epoch).await?;

    for f in feelings {
        let badge = if f.mood.is_empty() {
            TEXT_ENTRY_BADGE
        } else {
            '●'
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
            entry_type: "feeling",
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
                TEXT_ENTRY_BADGE,
                None,
            ),
            TrackerType::Number | TrackerType::Float => {
                let score = score_f64(&row.score);
                (format!("{}: {}", tracker_type, score), '◆', Some(score))
            }
        };
        let color = match score {
            Some(s) => RatColor::from_crossterm(bin_score_color(config, tracker, s)),
            None => RatColor::DarkGray,
        };
        entries.push(TodayEntry {
            id: Some(custom_id),
            time,
            entry_type: "custom",
            label,
            body: String::new(),
            task_id: None,
            priority: 0,
            badge,
            color,
        });
    }

    // 3. Oneshot tasks due by the end of the horizon (start_time <= horizon_end).
    // This upper bound alone would also match overdue tasks (due before today),
    // so unless config.today_view.include_overdue is set, a lower bound keeps
    // only tasks due from today onward. The floor is bound (i64::MIN =
    // effectively no filter) so the SQL stays static.
    let overdue_floor = if config.today_view.include_overdue {
        i64::MIN
    } else {
        day_start_epoch
    };
    let due_tasks =
        crate::sql::fetch_due_oneshot_tasks(pool, horizon_end, overdue_floor).await?;

    for task in &due_tasks {
        let urgency = match task.start_time {
            Some(st) if st < day_start_epoch => "OVERDUE",
            _ => "due",
        };
        let detail = format!("[p={}] {}", task.priority, urgency);
        let time = task.start_time.unwrap_or(day_start_epoch);
        let color = RatColor::from_crossterm(
            completion_badge(
                config,
                task.completions.unwrap_or(0) as i64,
                task.target_count,
            )
            .1,
        );
        entries.push(TodayEntry {
            id: None,
            time,
            entry_type: "task",
            label: task.name.clone(),
            body: detail,
            task_id: Some(task.id),
            priority: task.priority,
            badge: '○',
            color,
        });
    }

    // 3b. Scheduled tasks overlapping the horizon (window overlap: started
    // before horizon_end, still open past today_start). All states show —
    // ongoing ("scheduled"), completed / auto-completed ("done"), failed
    // ("overdue") — with the same badge semantics as the tasks app.
    let now_ts = date::now();
    let scheduled_tasks =
        crate::sql::fetch_scheduled_today(pool, horizon_end, day_start_epoch).await?;

    for task in &scheduled_tasks {
        let state = match task.completions {
            Some(c) if c > 0 => "done",
            Some(_) => "overdue",
            None => {
                let elapsed = task.start_time.unwrap_or(now_ts)
                    + task.available_duration_secs.unwrap_or(0)
                    < now_ts;
                if elapsed {
                    "done"
                } else {
                    "scheduled"
                }
            }
        };
        let detail = format!("[p={}] {}", task.priority, state);
        let (ch, color) = crate::views::scheduled_badge(
            config,
            task.completions,
            task.start_time,
            task.available_duration_secs,
            now_ts,
        );
        entries.push(TodayEntry {
            id: None,
            time: task.start_time.unwrap_or(day_start_epoch),
            entry_type: "task",
            label: task.name.clone(),
            body: detail,
            task_id: Some(task.id),
            priority: task.priority,
            badge: ch,
            color: RatColor::from_crossterm(color),
        });
    }

    // 4. Active recurring tasks (available today; the availability filter is
    // applied inside sql::fetch_active_recurring_tasks).
    let now_ts = date::now();
    let recurring_tasks = crate::sql::fetch_active_recurring_tasks(pool, now_ts).await?;

    for task in &recurring_tasks {
        let detail = format!("[p={}] recurring", task.priority);
        // Tasks without availability are active all day → sort to top
        let time = if task.available_duration_secs.is_none() {
            day_start_epoch
        } else {
            task.start_time.unwrap_or(now_ts)
        };
        entries.push(TodayEntry {
            id: None,
            time,
            entry_type: "task",
            label: task.name.clone(),
            body: detail,
            task_id: Some(task.id),
            priority: task.priority,
            badge: '○',
            color: RatColor::from_crossterm(
                completion_badge(
                    config,
                    task.completions.unwrap_or(0) as i64,
                    task.target_count,
                )
                .1,
            ),
        });
    }

    // 5. Today's todo completions
    let completions =
        crate::sql::fetch_completions_with_names(pool, day_start_epoch, day_end_epoch).await?;

    for completion in completions {
        let time = completion.time;
        let name = completion.name;
        entries.push(TodayEntry {
            id: None,
            time,
            entry_type: "completion",
            label: name.clone(),
            body: String::new(),
            task_id: Some(completion.todo_id),
            priority: 0,
            badge: '✓',
            color: RatColor::from_crossterm(*config.tasks.colors.last().unwrap()),
        });
    }

    // Sort all entries chronologically
    entries.sort_by_key(|e| e.time);

    Ok(entries)
}

/// Handle today view (non-terminal output): displays today's feelings, custom
/// entries, and task activity as tab-separated rows. TUI dispatch is handled by
/// [`crate::handlers::handle_command`].
pub async fn handle_today<W: Write>(pool: &SqlitePool, config: &Config, out: &mut W) -> Result<()> {
    let mut config = config.clone();
    config
        .moods
        .init_with(pool, crate::embed::global_embedder())
        .await?;
    let mut color_cache = std::collections::HashMap::new();
    let entries = fetch_today_entries(pool, &config, TodayHorizon::Today, &mut color_cache).await?;

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
    include_completed: bool,
    include_scheduled: bool,
    out: &mut W,
) -> Result<()> {
    let tasks = crate::sql::fetch_tasks_for_view(pool, mode, include_completed, include_scheduled)
        .await?;

    if tasks.is_empty() {
        writeln!(out, "No tasks found for view: {:?}", mode)?;
        return Ok(());
    }

    write!(
        out,
        "{}",
        crate::display::format_tasks_simple(&tasks, config)
    )?;

    Ok(())
}

