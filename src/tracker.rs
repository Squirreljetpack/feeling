use anyhow::{Context, Result};
use crossterm::style::{Color as CtColor, Stylize};
use sqlx::SqlitePool;
use std::io::Write;

use crate::badge::completion_badge;
use crate::cli::{CliOpts, TrackerItem, TrackerPeriod};
use crate::config::{Config, TrackerKind};
use crate::date;

/// Read a tracker score as f64. The `score` column is stored as
/// BLOB but SQLite's dynamic typing means values can be INTEGER, REAL, or
/// TEXT. `sql::fetch_tracker_entries` selects `CAST(score AS TEXT)` so
/// every storage type decodes as a String; parse that.
pub(crate) fn score_f64(s: &str) -> f64 {
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
pub async fn write_tracker_grid<W: Write>(
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
                    // Tracker: display score dots
                    if opts.verbose() {
                        writeln!(out, "{}", grid_title(id_str, period, opts.verbose_level()))?;
                    }
                    display_tracker(
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
    let feelings: Vec<crate::db::FeelingRow> =
        crate::db::fetch_feelings_between(pool, start_epoch, end_epoch)
            .await?
            .into_iter()
            .filter(|f| !f.mood.is_empty())
            .collect();

    if feelings.is_empty() {
        writeln!(out, "No mood entries in this period.")?;
        return Ok(());
    }

    let embedder = crate::embedding::global_embedder();
    let axes = config.moods.color_axes.as_ref().unwrap();

    let day_secs: i64 = 86400;
    let num_days = ((end_epoch - start_epoch) / day_secs + 1) as usize;
    let mut day_feelings: Vec<Vec<&crate::db::FeelingRow>> = vec![Vec::new(); num_days];
    let mut day_has_entry: Vec<bool> = vec![false; num_days];

    for f in &feelings {
        let time = f.time;
        let day_idx = ((time - start_epoch) / day_secs) as usize;
        if day_idx >= num_days {
            continue;
        }
        day_has_entry[day_idx] = true;
        day_feelings[day_idx].push(f);
    }

    let mut day_colors: Vec<Option<oklab::Oklab>> = vec![None; num_days];

    for (i, feelings_in_day) in day_feelings.iter().enumerate() {
        if feelings_in_day.is_empty() {
            continue;
        }
        let mut emb_sum: Vec<f32> = Vec::new();
        let mut score_sum: f32 = 0.0;
        let mut count: usize = 0;

        for f in feelings_in_day {
            let emb = match f
                .embedding
                .as_deref()
                .and_then(crate::embedding::blob_to_embedding)
            {
                Some(e) => Some(e),
                None => embedder.embed(&f.mood, &axes.prefix_string).ok(),
            };
            let Some(emb) = emb else { continue };

            let score = match f.score {
                Some(s) => s,
                None => crate::color::predict_saliency(embedder, &f.mood),
            };

            if emb_sum.is_empty() {
                emb_sum = emb;
            } else {
                for (s_elem, e_elem) in emb_sum.iter_mut().zip(&emb) {
                    *s_elem += e_elem;
                }
            }
            score_sum += score;
            count += 1;
        }

        if count > 0 {
            let inv_n = 1.0 / count as f32;
            for e_elem in &mut emb_sum {
                *e_elem *= inv_n;
            }
            let avg_score = score_sum * inv_n;

            let reg = axes.regression_weights(&emb_sum, embedder, Ok(avg_score));
            let oklab = axes.weights_to_color(reg.as_ref());
            day_colors[i] = Some(oklab);
        }
    }

    // The grid body follows; the section title (if any) is printed by
    // write_tracker_grid.

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
    for (i, &oklab_opt) in day_colors.iter().enumerate() {
        let d = if !day_has_entry[i] {
            "◯".to_string()
        } else if let Some(oklab) = oklab_opt {
            "●"
                .with(crate::color::conversion::oklab_to_crossterm(oklab))
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
    day_colors: &[Option<oklab::Oklab>],
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
                    } else if let Some(oklab) = day_colors[day] {
                        write!(
                            out,
                            "{}",
                            "●".with(crate::color::conversion::oklab_to_crossterm(oklab))
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

async fn display_tracker<W: Write>(
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
    let tracker = config
        .tracker
        .get(tracker_type)
        .ok_or_else(|| anyhow::anyhow!("Unknown tracker '{}' not found in config", tracker_type))?;

    // Fetch all entries in the period
    let entries =
        crate::db::fetch_tracker_entries(pool, tracker_type, start_epoch, end_epoch).await?;

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
    if tracker.kind == TrackerKind::Text {
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
    let colors = tracker.colors.as_ref().unwrap_or(&config.tasks.colors);
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
                        let idx = ((t * (colors.len() as f64 - 1.0)).round() as usize)
                            .min(colors.len() - 1);
                        colors[idx]
                    }
                    _ => {
                        // Both missing or min==max: use last color
                        *colors.last().unwrap()
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
                    let idx =
                        ((t * (colors.len() as f64 - 1.0)).round() as usize).min(colors.len() - 1);
                    colors[idx]
                }
                _ => {
                    // Both missing or min==max: use last color
                    *colors.last().unwrap()
                }
            };
            write!(out, "{}", "●".with(color))?;
        }
        writeln!(out)?;
    }
    Ok(())
}

/// Map a tracker score to a color by binning it across the tracker's
/// color override (if set) or the global task colors. Handles inverted ranges
/// (max < min → smaller values get the success color).
pub(crate) fn bin_score_color(
    config: &Config,
    tracker: &crate::config::TrackerSetting,
    score: f64,
) -> CtColor {
    let colors = tracker.colors.as_ref().unwrap_or(&config.tasks.colors);

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
    let Some(task) = crate::db::fetch_recurring_task_meta(pool, name).await? else {
        writeln!(out, "Recurring task '{}' not found.", name)?;
        return Ok(());
    };

    let task_id = task.id;
    let interval_secs = task.interval_secs;
    let target_count = task.target_count;

    // Get completion events (time, count) for this task in the period
    let completions =
        crate::db::fetch_completions_between(pool, task_id, start_epoch, end_epoch).await?;

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
                interval_sums[idx] += i64::from(count);
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
            let (ch, color) = completion_badge(config, i64::from(count), target_count);
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
}
