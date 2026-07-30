use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Take the first `max` characters of `s` (Unicode scalar values, not bytes).
pub fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Render the confirm modal's navigable Yes/No buttons (fist-style): the
/// selected option is inverted (black on white), the hotkey letter (Y/N) is
/// bold, both options are italic, and the two options are separated by a
/// two-space gap. The caller centers the resulting line.
///
/// `cursor` selects the highlighted option: 0 = Yes, 1 = No.
pub fn confirm_buttons(cursor: usize) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, name) in ["Yes", "No"].iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let selected = i == cursor;
        let style = if selected {
            Style::default()
                .bg(Color::White)
                .fg(Color::Black)
                .add_modifier(Modifier::ITALIC)
        } else {
            Style::default().add_modifier(Modifier::ITALIC)
        };
        for (ci, ch) in name.chars().enumerate() {
            let s = if ci == 0 {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            };
            spans.push(Span::styled(ch.to_string(), s));
        }
    }
    Line::from(spans)
}

pub fn priority_color(p: i32) -> Style {
    match p {
        0..=3 => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        4..=6 => Style::default().fg(Color::Yellow),
        7..=9 => Style::default().fg(Color::Green),
        _ => Style::default().fg(Color::DarkGray),
    }
}

pub fn mode_label(mode: crate::clap::ViewMode) -> &'static str {
    match mode {
        crate::clap::ViewMode::OneShotTasks => "! Oneshot Tasks",
        crate::clap::ViewMode::RecurringTasks => "@ Recurring Tasks",
        crate::clap::ViewMode::DoneTasks => "@done Completed",
        crate::clap::ViewMode::DueTasks => "@due Due",
    }
}
