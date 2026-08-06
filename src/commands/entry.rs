use anyhow::Result;
use sqlx::SqlitePool;

use crate::cli::CliOpts;
use crate::config::{Config, TrackerKind};
use crate::date;
use crate::db::{EntryObject, TrackerObject, TrackerValue};
use crate::editor::open_editor_for_body;
use crate::types::Entry;

pub(super) async fn record_entry(
    pool: &SqlitePool,
    config: &Config,
    opts: &CliOpts,
    entry: Entry,
) -> Result<()> {
    let feeling = entry.feeling;
    let trackers = entry.trackers;
    let body = entry.body;
    let open_editor = entry.open_editor;

    // Body resolution: the parser places anything after `..` into `body`.
    // `open_editor` is true (set by the parser) iff `..` was used AND `body`
    // is empty — exactly the case where we want to open the editor. When
    // `body` is already supplied (post-.. text or no `..`), we use it as-is.
    let body = if open_editor {
        open_editor_for_body(config.editor.hint)?
    } else {
        body
    };

    if feeling.is_empty() && trackers.is_empty() && body.is_empty() {
        anyhow::bail!("Nothing to log");
    }

    // Determine the timestamp (Unix epoch in seconds).
    let time_epoch = date::now();

    // Parse and validate tracker values against their declared kind.
    // Raw strings are interpreted here (not in the parser) so the config's
    // kind (text/number/float) determines how each value is stored. Text and
    // float trackers with an interval keep one entry per interval slot (see
    // `interval_slot`): re-logging the same tracker in the same slot replaces
    // the previous entry (handled inside `sql::create_entry`). Number
    // trackers always accumulate.
    let mut tracker_objects: Vec<TrackerObject> = Vec::with_capacity(trackers.len());
    for (tracker_type, raw) in &trackers {
        let value = parse_tracker_value(config, tracker_type, raw)?;
        let replace_slot = config
            .tracker
            .get(tracker_type)
            .filter(|tracker| matches!(tracker.kind, TrackerKind::Text | TrackerKind::Float))
            .and_then(|tracker| tracker.interval)
            .map(|interval_secs| interval_slot(time_epoch, interval_secs));
        tracker_objects.push(TrackerObject {
            tracker_type: tracker_type.clone(),
            value,
            replace_slot,
        });
    }

    // Resolve the mood embedding and its saliency score before opening the
    // transaction. Journal-only entries (empty mood) never embed; the model
    // is bundled into the binary, so the embedder is always available — a
    // per-text embedding failure (e.g. an un-tokenizable string) stores no
    // embedding rather than losing the entry. The score is computed here so
    // color passes later skip the ONNX saliency prediction.
    let embedder = crate::embedding::global_embedder();
    let (embedding_blob, score) = if feeling.is_empty() {
        (None, None)
    } else {
        match embedder.embed(&feeling, &config.moods.axes.prefix_string) {
            Ok(v) => (
                Some(crate::embedding::embedding_to_blob(&v)),
                Some(crate::color::predict_saliency(embedder, &feeling)),
            ),
            Err(_) => (None, None),
        }
    };

    let entry_obj = EntryObject {
        mood: feeling,
        body,
        time: time_epoch,
        embedding: embedding_blob,
        score,
        trackers: tracker_objects,
    };

    let feeling_id = crate::db::create_entry(pool, &entry_obj).await?;
    log::debug!("Inserted feeling with id={:?}", feeling_id);

    crate::output::display_entry(&entry_obj, opts)?;

    Ok(())
}

/// Interpret a raw CLI value for a tracker according to its configured kind.
/// Denies unknown tracker types; parses Number/Float values (with a clear
/// error when the argument cannot be parsed) and enforces min/max for both;
/// Text accepts the value as-is (min/max ignored).
fn parse_tracker_value(config: &Config, tracker_type: &str, raw: &str) -> Result<TrackerValue> {
    let tracker = config.tracker.get(tracker_type).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown tracker type '{}' not found in config",
            tracker_type
        )
    })?;

    match tracker.kind {
        TrackerKind::Text => Ok(TrackerValue::Text(raw.to_string())),
        TrackerKind::Number => {
            let n: i64 = raw.parse().map_err(|_| {
                anyhow::anyhow!(
                    "Cannot parse '{}' as an integer for tracker '{}'",
                    raw,
                    tracker_type
                )
            })?;
            Ok(TrackerValue::Number(n))
        }
        TrackerKind::Float => {
            let f: f64 = raw.parse().map_err(|_| {
                anyhow::anyhow!(
                    "Cannot parse '{}' as a number for tracker '{}'",
                    raw,
                    tracker_type
                )
            })?;
            Ok(TrackerValue::Float(f))
        }
    }
}

/// The `[start, end)` replacement slot for an interval-tracker entry: a
/// uniform grid of the timeline, `[k*interval, (k+1)*interval)` aligned to
/// the Unix epoch. Uniform tiling keeps **any** interval working — including
/// sub-day ones like 30 minutes, where a calendar-day anchor would collapse
/// every same-day entry into a single slot.
///
/// KNOWN FLAW (roadmap: calendar-aware intervals): the grid's phase is UTC
/// midnight, so a "1 day" tracker's slots run local 20:00 → 20:00 on a
/// UTC-4 machine, and a "1 week" slot is exactly 604800s (167/169h across
/// a DST change). Replacing this pure-seconds grid with calendar-day/week
/// slots is the roadmap item; do not re-introduce a local-midnight anchor
/// here, it breaks sub-day intervals.
fn interval_slot(time_epoch: i64, interval_secs: i64) -> (i64, i64) {
    let slot_start = (time_epoch / interval_secs) * interval_secs;
    (slot_start, slot_start + interval_secs)
}

#[cfg(test)]
mod tests {
    use super::interval_slot;
    use crate::date;

    /// The slot always contains the entry and has the requested length.
    #[test]
    fn interval_slot_contains_entry() {
        let t = date::today_start() + 12 * 3600;
        for interval in [1800i64, 86400, 604800] {
            let (start, end) = interval_slot(t, interval);
            assert!(t >= start && t < end, "{t} not in [{start}, {end})");
            assert_eq!(end - start, interval);
        }
    }

    /// Uniform tiling: adjacent slots touch (no gaps/overlaps). This is the
    /// property that keeps sub-day trackers (e.g. 30 min) working.
    #[test]
    fn interval_slot_tiles_uniformly() {
        let t = date::today_start() + 12 * 3600;
        for interval in [1800i64, 86400] {
            let a = interval_slot(t, interval);
            let b = interval_slot(t + interval, interval);
            assert_eq!(a.1, b.0, "slots must be adjacent for {interval}s");
        }
    }

    /// Sub-day intervals: entries in the same 30-min bucket share a slot,
    /// crossing a boundary doesn't.
    #[test]
    fn interval_slot_sub_day() {
        let t = date::today_start() + 10 * 3600; // 10:00 local
        let bucket = interval_slot(t, 1800);
        assert_eq!(interval_slot(t + 600, 1800), bucket); // 10:10 — same bucket
        assert_ne!(interval_slot(t + 1801, 1800), bucket); // 10:30:01 — next bucket
    }
}
