use anyhow::Result;
use ratatui::{backend::FromCrossterm, style::Color as RatColor};
use sqlx::SqlitePool;
use std::io::Write;

use crate::cli::CliOpts;
use crate::config::{Config, TrackerKind};
use crate::date::{self, Epoch};
use crate::db::TaskRow;
use crate::task::pending_sort_time;
use crate::types::{TaskKind, TodayHorizon, ViewVariant};

/// Badge for text-payload tracker entries wherever a marker is needed
/// (e.g. the today view). A named constant so the glyph can be adjusted later.
pub(crate) const TEXT_ENTRY_BADGE: char = '◆';

/// Category of a today-view entry, driving routing (edit / delete / preview)
/// and presentation. Replaces the old `entry_type` string and the task-only
/// `interval_secs` marker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryKind {
    /// A task, carrying its [`TaskKind`].
    Task(TaskKind),
    /// Feeling entry carrying a mood label.
    Mood,
    /// Journal-only feeling entry (empty mood label; the body holds the text).
    Journal,
    /// Tracker entry, carrying the tracker's configured payload kind.
    Tracker(TrackerKind),
}

impl EntryKind {
    pub fn is_task(self) -> bool {
        matches!(self, Self::Task(_))
    }

    pub fn is_mood(self) -> bool {
        matches!(self, Self::Mood | Self::Journal)
    }

    pub fn is_tracker(self) -> bool {
        matches!(self, Self::Tracker(_))
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
    /// tracker_color for numeric tracker entries, completion_badge
    /// colors for tasks, or a neutral dark gray for journal-only and
    /// text-tracker entries.
    pub color: RatColor,
    /// Recurring-task entries only: the availability window this row
    /// represents, with the window-scoped task row (completions and
    /// `last_time` limited to the window's interval). `None` for every
    /// other entry kind. Drives the D10 confirm (`now >= window_end` on a
    /// not-done window) and the selection preview.
    pub recurring_window: Option<crate::db::RecurringWindow>,
    /// Tracker entries with a configured interval: the (anchor, span) pair,
    /// so the preview can show the next interval start like recurring tasks.
    pub tracker_interval: Option<(Epoch, jiff::Span)>,
    /// Tracker entries: the most recent entry time of this tracker overall
    /// (unscoped — the preview's `last:` field).
    pub tracker_last: Option<Epoch>,
}

/// Today-view time cell for a timestamp: "HH:MM" when it falls on the
/// anchored day, "Tu HH:MM" (two-letter weekday prefix) when it falls
/// within a week of it — entries outside the anchored day stay
/// distinguishable in the +tomorrow/+week horizons — and the compact
/// day-time form ("DD HH:MM") outside that week entirely.
fn today_time_label(time: i64, day_start_epoch: i64) -> String {
    if time < day_start_epoch || time > day_start_epoch + 7 * 86_400 {
        crate::date::format_day_time(time)
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

/// The today-view time cell for a task row: "HH:MM" (weekday prefix when
/// outside the anchored day) for timed rows — completion time when done,
/// otherwise the task's deadline/availability end — and empty for the
/// untimed group (undated oneshots).
fn task_time_label(task: &TaskRow, time: i64, day_start_epoch: i64) -> String {
    if task.is_done() {
        return today_time_label(time, day_start_epoch);
    }
    if !task.is_scheduled() && !task.is_recurring() && task.end_time.is_none() {
        // Undated oneshot.
        return String::new();
    }
    today_time_label(time, day_start_epoch)
}

/// Today-view time for a recurring availability window (one row per
/// window): a completed window — or one that has passed (`now >=
/// window_end`) — shows the last completion within its interval, else the
/// window end; an open or future window shows the window start.
fn recurring_window_time(w: &crate::db::RecurringWindow, now: i64) -> i64 {
    if w.task.is_done() || now >= w.window_end {
        w.task.last_time.unwrap_or(w.window_end)
    } else {
        w.window_start
    }
}

/// Today-view sort: timed entries first (by timestamp ascending); then the
/// no-time group (undated oneshots) by priority descending, then by
/// untruncated availability end ascending.
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
/// tasks-only (no feelings/trackers) and carries `coalesce_completions`
/// (D11 — no behavior yet). See docs/VIEWS.md.
pub async fn fetch_today_entries(
    pool: &SqlitePool,
    config: &Config,
    horizon: TodayHorizon,
    day_epoch: Option<i64>,
    show: ViewVariant,
    color_cache: &mut std::collections::HashMap<String, oklab::Oklab>,
) -> Result<Vec<TodayEntry>> {
    // `feeling @<date>` anchors the day; bare `feeling` is today.
    let day_start_epoch = day_epoch.unwrap_or_else(date::today_start);
    let day_end_epoch = date::day_end(day_start_epoch);
    let horizon_end = horizon.end_epoch(day_start_epoch);
    let now_ts = date::now();

    let mut entries: Vec<TodayEntry> = Vec::new();

    // B is tasks-only: no feelings, no tracker entries.
    if show != ViewVariant::B {
        let embedder = crate::embedding::global_embedder();
        let axes = config.moods.color_axes.as_ref().unwrap();

        // 1. Feelings within the horizon (day start through horizon end,
        // matching the task fetches below).
        let feelings =
            crate::db::fetch_feelings_between(pool, day_start_epoch, horizon_end).await?;

        for f in feelings {
            // Journal-only entries (empty mood) use the configured journal
            // badge, or none at all; mood entries always get the filled dot.
            let badge = if f.mood.is_empty() {
                config.today_view.journal_badge
            } else {
                Some('●')
            };

            // Resolve this entry's embedding → color (cached per mood;
            // legacy rows without a stored embedding are re-embedded on the
            // fly — no backfill; `:db backfill` persists those).
            let oklab = axes.mood_color_cached(embedder, &f, color_cache);

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
                recurring_window: None,
                tracker_interval: None,
                tracker_last: None,
            });
        }

        // 2. Tracker entries within the horizon.
        let trackers =
            crate::db::fetch_tracker_entries_today(pool, day_start_epoch, horizon_end).await?;
        // Unscoped last entry time per tracker (the preview `last:` field).
        let tracker_lasts = crate::db::fetch_tracker_last_times(pool).await?;

        for row in trackers {
            let tracker_id = row.id;
            let tracker_type = row.tracker_type;
            let time = row.time;
            let tracker = config.tracker.get(&tracker_type).ok_or_else(|| {
                anyhow::anyhow!("Unknown tracker '{}' not found in config", tracker_type)
            })?;
            let (label, badge, score) = match tracker.kind {
                // Text payloads have no score; they use the shared text badge.
                TrackerKind::Text => (
                    format!("{}: {}", tracker_type, row.score),
                    Some(TEXT_ENTRY_BADGE),
                    None,
                ),
                TrackerKind::Number | TrackerKind::Float => {
                    let score = crate::tracker::score_f64(&row.score);
                    (
                        format!("{}: {}", tracker_type, score),
                        Some('◆'),
                        Some(score),
                    )
                }
                // Null payloads carry no value: the label is the tracker
                // name alone (the time column shows the moment). The score
                // still holds the count in count mode.
                TrackerKind::Null => {
                    let score = crate::tracker::score_f64(&row.score);
                    (tracker_type.clone(), Some('◆'), Some(score))
                }
            };
            // Color: Null trackers with an interval and both bounds use the
            // time-of-day coloring; otherwise the configured min/max bin the
            // score like any numeric tracker. Null trackers without an
            // interval are unsupported → Reset.
            let colors = tracker.colors.as_ref().unwrap_or(&config.tasks.colors);
            let color = match tracker.kind {
                TrackerKind::Null => RatColor::from_crossterm(crate::badge::null_tracker_color(
                    colors,
                    tracker,
                    time,
                    score.unwrap_or(0.0),
                )),
                // Text entries have no score; a single-color palette
                // override (validated to exactly 1 entry in Config::init)
                // colors their badge, otherwise neutral gray.
                TrackerKind::Text => tracker
                    .colors
                    .as_ref()
                    .and_then(|c| c.first())
                    .map(|c| RatColor::from_crossterm(*c))
                    .unwrap_or(RatColor::DarkGray),
                _ => match score {
                    Some(s) => RatColor::from_crossterm(crate::badge::tracker_color(
                        colors,
                        s,
                        tracker.min,
                        tracker.max,
                    )),
                    None => RatColor::DarkGray,
                },
            };
            entries.push(TodayEntry {
                id: Some(tracker_id),
                time,
                time_label: today_time_label(time, day_start_epoch),
                kind: EntryKind::Tracker(tracker.kind),
                label,
                body: String::new(),
                task_id: None,
                priority: 0,
                badge,
                color,
                recurring_window: None,
                tracker_interval: tracker.interval.map(|iv| (iv.anchor, iv.span)),
                tracker_last: tracker_lasts.get(&tracker_type).copied(),
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
    let due_tasks = crate::db::fetch_due_oneshot_tasks(pool, horizon_end, overdue_floor).await?;

    for task in &due_tasks {
        // A filters completed tasks out.
        if show == ViewVariant::A && task.is_done() {
            continue;
        }
        // Time: done → completion time; else the due time (`end_time` when
        // set — `! name @<time>`; undated oneshots are untimed).
        let time = pending_sort_time(task, now_ts);
        let time_label = task_time_label(task, time, day_start_epoch);
        let (badge, color) = crate::badge::task_badge(task, config, false);
        entries.push(TodayEntry {
            id: None,
            time,
            time_label,
            kind: EntryKind::Task(task.kind()),
            label: task.name.clone(),
            body: task.body.clone(),
            task_id: Some(task.id),
            priority: task.priority,
            // The badge rules (✓ done / ○ not done, overdue coloring) live
            // in badge::task_badge — see docs/BADGE.md.
            badge: Some(badge),
            color: RatColor::from_crossterm(color),
            recurring_window: None,
            tracker_interval: None,
            tracker_last: None,
        });
    }

    // 3b. Scheduled tasks overlapping the horizon (window overlap: started
    // before horizon_end, still open past today_start). All states show —
    // ongoing, completed / auto-completed, failed — with the same badge
    // semantics as the tasks app.
    let scheduled_tasks =
        crate::db::fetch_scheduled_today(pool, horizon_end, day_start_epoch).await?;

    for task in &scheduled_tasks {
        // A filters completed tasks out (incl. auto-completed).
        if show == ViewVariant::A && task.is_done() {
            continue;
        }
        let (badge, color) = crate::badge::task_badge(task, config, false);
        // Time: done → completion time (auto-completed has no entry, so it
        // falls back to the window end); else `start_time`.
        let time = pending_sort_time(task, now_ts);
        let time_label = task_time_label(task, time, day_start_epoch);
        entries.push(TodayEntry {
            id: None,
            time,
            time_label,
            kind: EntryKind::Task(task.kind()),
            label: task.name.clone(),
            body: task.body.clone(),
            task_id: Some(task.id),
            priority: task.priority,
            badge: Some(badge),
            color: RatColor::from_crossterm(color),
            recurring_window: None,
            tracker_interval: None,
            tracker_last: None,
        });
    }

    // 4. Recurring tasks: one entry per availability window intersecting
    // the period (all variants; interval-aware availability-window overlap
    // — VIEWS.md). Each window's completions / last completion are scoped
    // to its own interval, so time, done state, and badge are per window.
    // `B` keeps only the next (earliest) window per task.
    let recurring_windows =
        crate::db::fetch_recurring_windows_for_period(pool, day_start_epoch, horizon_end).await?;

    let mut seen_recurring = std::collections::HashSet::new();
    for w in &recurring_windows {
        // B: only the next recurring window per task.
        if show == ViewVariant::B && !seen_recurring.insert(w.task.id) {
            continue;
        }
        // A filters completed windows out (the window's own completion
        // state, not the current interval's).
        if show == ViewVariant::A && w.task.is_done() {
            continue;
        }
        // Time (VIEWS.md): a completed or passed (`now >= window_end`)
        // window shows the last completion within its interval, else the
        // window end; an open or future window shows the window start.
        let time = recurring_window_time(w, now_ts);
        let time_label = task_time_label(&w.task, time, day_start_epoch);
        let (badge, color) =
            crate::badge::recurring_window_badge(&w.task, w.window_end, config, now_ts);
        entries.push(TodayEntry {
            id: None,
            time,
            time_label,
            kind: EntryKind::Task(w.task.kind()),
            label: w.task.name.clone(),
            body: w.task.body.clone(),
            task_id: Some(w.task.id),
            priority: w.task.priority,
            badge: Some(badge),
            color: RatColor::from_crossterm(color),
            recurring_window: Some(w.clone()),
            tracker_interval: None,
            tracker_last: None,
        });
    }

    // 5. Tasks with a completion entry today (All and B — B is the same as
    // All minus the feelings/trackers sections): merged over the regular
    // rows (dedup by task_id — the completed-today row wins, time = last
    // completion timestamp) so a task completed today shows its completion
    // time even when it is no longer active (or not in the regular lists
    // at all). Recurring tasks with a per-window entry (step 4) are
    // skipped: the window rows already carry the window-scoped completion
    // state and rule-based times. `A` filters completed tasks out, so the
    // fetch is skipped there.
    if show != ViewVariant::A {
        let completed_today =
            crate::db::fetch_tasks_completed_on(pool, day_start_epoch, day_end_epoch).await?;
        for task in &completed_today {
            // Recurring windows already have entries (step 4) carrying the
            // window-scoped completion state — merging here would override
            // the rule-based window time with a day-scoped one. Tasks with
            // no window row (expired chain with a late completion) still
            // merge in below.
            if task.is_recurring() && entries.iter().any(|e| e.task_id == Some(task.id)) {
                continue;
            }
            let last_time = task.last_time.unwrap_or(now_ts);
            let (badge, color) = crate::badge::task_badge(task, config, false);
            let entry = TodayEntry {
                id: None,
                time: last_time,
                time_label: today_time_label(last_time, day_start_epoch),
                kind: EntryKind::Task(task.kind()),
                label: task.name.clone(),
                body: task.body.clone(),
                task_id: Some(task.id),
                priority: task.priority,
                badge: Some(badge),
                color: RatColor::from_crossterm(color),
                recurring_window: None,
                tracker_interval: None,
                tracker_last: None,
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

/// Handle today view (non-terminal output): displays today's feelings, tracker
/// entries, and task activity as tab-separated rows. TUI dispatch is handled by
/// [`crate::commands::execute_command`].
pub async fn write_today_view<W: Write>(
    pool: &SqlitePool,
    config: &Config,
    day_epoch: Option<i64>,
    show: ViewVariant,
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

    write!(out, "{}", crate::output::format_today_simple(&entries))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_today_time_label() {
        // 2024-03-15 is a Friday; 2024-03-16 a Saturday.
        let day =
            crate::date::parse_datetime("2024-03-15 00:00", crate::date::DATE_DIALECT).unwrap();
        let same =
            crate::date::parse_datetime("2024-03-15 09:30", crate::date::DATE_DIALECT).unwrap();
        let next =
            crate::date::parse_datetime("2024-03-16 09:30", crate::date::DATE_DIALECT).unwrap();
        assert_eq!(today_time_label(same, day), "09:30");
        assert_eq!(today_time_label(next, day), "Sa 09:30");
        // Outside the week window → compact day-time form ("DD HH:MM").
        let far =
            crate::date::parse_datetime("2024-03-25 09:30", crate::date::DATE_DIALECT).unwrap();
        let early =
            crate::date::parse_datetime("2024-03-01 09:30", crate::date::DATE_DIALECT).unwrap();
        assert_eq!(today_time_label(far, day), "25 09:30");
        assert_eq!(today_time_label(early, day), "01 09:30");
        crate::date::parse_datetime("2024-03-25 09:30", crate::date::DATE_DIALECT).unwrap();
        // The weekday form covers days 1-6 after the anchor; the 7th day
        // (>= day_start + week) is already the short form.
        let within =
            crate::date::parse_datetime("2024-03-21 09:30", crate::date::DATE_DIALECT).unwrap();
        assert_eq!(today_time_label(within, day), "Th 09:30");
        let boundary =
            crate::date::parse_datetime("2024-03-22 00:00", crate::date::DATE_DIALECT).unwrap();
        assert_eq!(today_time_label(boundary, day), "Fr 00:00");
    }

    fn task_row(
        start_time: Option<i64>,
        available_duration_secs: Option<i64>,
        interval: Option<jiff::Span>,
        end_time: Option<i64>,
        completions: Option<i32>,
        last_time: Option<i64>,
    ) -> TaskRow {
        // The row stores the packed DbSpan.
        let interval_secs = interval.map(|s| crate::date::span_to_db(&s));
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
            parent: None,
            completions,
            last_time,
        }
    }

    #[test]
    fn test_pending_sort_time() {
        let day =
            crate::date::parse_datetime("2024-03-16 00:00", crate::date::DATE_DIALECT).unwrap();
        let anchor =
            crate::date::parse_datetime("2024-03-15 08:00", crate::date::DATE_DIALECT).unwrap();
        let now =
            crate::date::parse_datetime("2024-03-16 14:00", crate::date::DATE_DIALECT).unwrap();
        let at = |s: &str| crate::date::parse_datetime(s, crate::date::DATE_DIALECT).unwrap();
        let day_secs = 86400;
        let hour_secs = 3600;

        let check = |task: &TaskRow, expect_time: i64, expect_label: &str| {
            let time = crate::task::pending_sort_time(task, now);
            let label = task_time_label(task, time, day);
            assert_eq!(time, expect_time, "time for {}", task.name);
            assert_eq!(label, expect_label, "label for {}", task.name);
        };

        // Recurring with an availability window (08:00-09:00), window
        // already closed at 14:00: the next interval's start (17th 08:00),
        // with a weekday prefix (outside the anchored day).
        check(
            &task_row(
                Some(anchor),
                Some(hour_secs),
                Some(jiff::Span::new().days(1)),
                None,
                None,
                None,
            ),
            at("2024-03-17 08:00"),
            "Su 08:00",
        );
        // Same recurring window, still open (now 08:30, before the 09:00
        // end): the window end of the current interval.
        let open_now = at("2024-03-16 08:30");
        let open = task_row(
            Some(anchor),
            Some(hour_secs),
            Some(jiff::Span::new().days(1)),
            None,
            None,
            None,
        );
        assert_eq!(
            crate::task::pending_sort_time(&open, open_now),
            at("2024-03-16 09:00"),
            "window still open → window end"
        );
        // Recurring without an explicit duration: the whole interval is the
        // window, so the closed window defers to the next interval's start
        // (timed — every recurring window has a time cell now).
        check(
            &task_row(
                Some(anchor),
                None,
                Some(jiff::Span::new().days(1)),
                None,
                None,
                None,
            ),
            at("2024-03-17 08:00"),
            "Su 08:00",
        );
        // Recurring whose duration would swallow the whole interval
        // (dur == interval — not enforced at ingestion): deferred to the
        // next interval's start, like the untimed group.
        check(
            &task_row(
                Some(anchor),
                Some(day_secs),
                Some(jiff::Span::new().days(1)),
                None,
                None,
                None,
            ),
            at("2024-03-17 08:00"),
            "Su 08:00",
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
        // — not done, so the pending key is the next interval's start (the
        // window closed at 09:00, before now); the done-view key is the
        // last completion entry.
        let partial = TaskRow {
            name: "partial history".to_string(),
            target_count: 2,
            ..task_row(
                Some(anchor),
                Some(hour_secs),
                Some(jiff::Span::new().days(1)),
                None,
                Some(1),
                Some(at("2024-03-16 13:00")),
            )
        };
        assert_eq!(
            crate::task::pending_sort_time(&partial, now),
            at("2024-03-17 08:00"),
            "pending view: next interval start (window passed)"
        );
        assert_eq!(
            crate::task::completed_sort_time(&partial),
            at("2024-03-16 13:00"),
            "done view: last completion entry"
        );
    }

    #[test]
    fn test_completed_sort_time() {
        let at = |s: &str| crate::date::parse_datetime(s, crate::date::DATE_DIALECT).unwrap();
        let day_secs = 86400;
        let hour_secs = 3600;

        // Done oneshot with an entry: the last completion entry.
        assert_eq!(
            crate::task::completed_sort_time(&task_row(
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
            crate::task::completed_sort_time(&task_row(
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
            crate::task::completed_sort_time(&task_row(
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
            crate::task::completed_sort_time(&task_row(
                Some(at("2024-03-15 08:00")),
                Some(2 * hour_secs),
                Some(jiff::Span::new().days(1)),
                None,
                None,
                None,
            )),
            at("2024-03-15 08:00")
        );
        // Undated: i64::MAX (defensive — can't appear in a done view).
        assert_eq!(
            crate::task::completed_sort_time(&task_row(None, None, None, None, None, None)),
            i64::MAX
        );
    }

    #[test]
    fn test_today_sort() {
        let entry = |time: i64, time_label: &str, priority: i32| TodayEntry {
            id: None,
            time,
            time_label: time_label.to_string(),
            kind: EntryKind::Task(TaskKind::Oneshot),
            label: String::new(),
            body: String::new(),
            task_id: None,
            priority,
            badge: None,
            color: RatColor::DarkGray,
            recurring_window: None,
            tracker_interval: None,
            tracker_last: None,
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
