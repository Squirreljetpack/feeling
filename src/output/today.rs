use crate::today::{EntryKind, TodayEntry};

/// Format today view entries into tab-separated output text.
pub fn format_today_simple(entries: &[TodayEntry]) -> String {
    use crossterm::style::{Color as CtColor, Stylize};
    use ratatui::backend::IntoCrossterm;

    let mut output = String::new();
    for entry in entries {
        let ts = entry.time_label.clone();

        // Journal entries (empty mood label) carry the body as the label.
        let (label, detail) = if entry.kind == EntryKind::Journal {
            (entry.body.to_string(), String::new())
        } else {
            (entry.label.clone(), entry.body.clone())
        };

        // Same badge as the TUI: marker glyph colored with the entry's dot
        // color. Reset-colored badges (e.g. 0% tasks) stay plain; entries
        // without a badge (journal entries, no journal_badge configured)
        // render an empty cell.
        let color = entry.color.into_crossterm();
        let badge = match entry.badge {
            None => String::new(),
            Some(c) if color == CtColor::Reset => c.to_string(),
            Some(c) => c.to_string().with(color).to_string(),
        };

        output.push_str(&format!("{}\t{}\t{}\t{}\n", ts, badge, label, detail));
    }
    output
}
