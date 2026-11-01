//! Color theme. Uses the 256-color palette (not true color) so it looks the
//! same in macOS Terminal, iTerm, tmux and Linux terminals.

use ratatui::style::{Color, Modifier, Style};

pub(super) const ACCENT: Color = Color::Indexed(141); // lavender
pub(super) const ACCENT2: Color = Color::Indexed(81); // cyan
pub(super) const TEXT: Color = Color::Indexed(252);
pub(super) const DIM: Color = Color::Indexed(245);
pub(super) const FAINT: Color = Color::Indexed(240);
pub(super) const BORDER: Color = Color::Indexed(238);
pub(super) const PANEL: Color = Color::Indexed(235);
pub(super) const SELECTED: Color = Color::Indexed(237);
pub(super) const ON_ACCENT: Color = Color::Indexed(234);

pub(super) const RED: Color = Color::Indexed(203);
pub(super) const ORANGE: Color = Color::Indexed(209);
pub(super) const YELLOW: Color = Color::Indexed(221);
pub(super) const GREEN: Color = Color::Indexed(114);
pub(super) const BLUE: Color = Color::Indexed(75);
pub(super) const MAGENTA: Color = Color::Indexed(170);
pub(super) const PINK: Color = Color::Indexed(205);

pub(super) fn level_color(level: &str) -> Color {
    match level {
        "fatal" => MAGENTA,
        "error" => RED,
        "warning" => YELLOW,
        "info" => BLUE,
        _ => DIM,
    }
}

/// Short fixed-width label for level badges.
pub(super) fn level_label(level: &str) -> &'static str {
    match level {
        "fatal" => " FATAL ",
        "error" => " ERROR ",
        "warning" => " WARN  ",
        "info" => " INFO  ",
        "debug" => " DEBUG ",
        _ => " OTHER ",
    }
}

pub(super) fn badge(level: &str) -> Style {
    Style::new().fg(ON_ACCENT).bg(level_color(level)).add_modifier(Modifier::BOLD)
}

pub(super) fn status_color(status: &str) -> Color {
    match status {
        "unresolved" => ORANGE,
        "resolved" => GREEN,
        _ => DIM,
    }
}

pub(super) fn text() -> Style {
    Style::new().fg(TEXT)
}
pub(super) fn dim() -> Style {
    Style::new().fg(DIM)
}
pub(super) fn faint() -> Style {
    Style::new().fg(FAINT)
}
pub(super) fn bold() -> Style {
    Style::new().fg(TEXT).add_modifier(Modifier::BOLD)
}
pub(super) fn accent() -> Style {
    Style::new().fg(ACCENT)
}
pub(super) fn border() -> Style {
    Style::new().fg(BORDER)
}
pub(super) fn key() -> Style {
    Style::new().fg(ACCENT2).add_modifier(Modifier::BOLD)
}
pub(super) fn selected_row() -> Style {
    Style::new().bg(SELECTED).add_modifier(Modifier::BOLD)
}
pub(super) fn tab_active() -> Style {
    Style::new().fg(ON_ACCENT).bg(ACCENT).add_modifier(Modifier::BOLD)
}
pub(super) fn tab_inactive() -> Style {
    Style::new().fg(DIM)
}
