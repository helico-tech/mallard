//! Color theme. Uses the 256-color palette (not true color) so it looks the
//! same in macOS Terminal, iTerm, tmux and Linux terminals.

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Indexed(141); // lavender
pub const ACCENT2: Color = Color::Indexed(81); // cyan
pub const TEXT: Color = Color::Indexed(252);
pub const DIM: Color = Color::Indexed(245);
pub const FAINT: Color = Color::Indexed(240);
pub const BORDER: Color = Color::Indexed(238);
pub const PANEL: Color = Color::Indexed(235);
pub const SELECTED: Color = Color::Indexed(237);
pub const ON_ACCENT: Color = Color::Indexed(234);

pub const RED: Color = Color::Indexed(203);
pub const ORANGE: Color = Color::Indexed(209);
pub const YELLOW: Color = Color::Indexed(221);
pub const GREEN: Color = Color::Indexed(114);
pub const BLUE: Color = Color::Indexed(75);
pub const MAGENTA: Color = Color::Indexed(170);
pub const PINK: Color = Color::Indexed(205);

pub fn level_color(level: &str) -> Color {
    match level {
        "fatal" => MAGENTA,
        "error" => RED,
        "warning" => YELLOW,
        "info" => BLUE,
        _ => DIM,
    }
}

/// Short fixed-width label for level badges.
pub fn level_label(level: &str) -> &'static str {
    match level {
        "fatal" => " FATAL ",
        "error" => " ERROR ",
        "warning" => " WARN  ",
        "info" => " INFO  ",
        "debug" => " DEBUG ",
        _ => " OTHER ",
    }
}

pub fn badge(level: &str) -> Style {
    Style::new().fg(ON_ACCENT).bg(level_color(level)).add_modifier(Modifier::BOLD)
}

pub fn status_color(status: &str) -> Color {
    match status {
        "unresolved" => ORANGE,
        "resolved" => GREEN,
        _ => DIM,
    }
}

pub fn text() -> Style {
    Style::new().fg(TEXT)
}
pub fn dim() -> Style {
    Style::new().fg(DIM)
}
pub fn faint() -> Style {
    Style::new().fg(FAINT)
}
pub fn bold() -> Style {
    Style::new().fg(TEXT).add_modifier(Modifier::BOLD)
}
pub fn accent() -> Style {
    Style::new().fg(ACCENT)
}
pub fn border() -> Style {
    Style::new().fg(BORDER)
}
pub fn key() -> Style {
    Style::new().fg(ACCENT2).add_modifier(Modifier::BOLD)
}
pub fn selected_row() -> Style {
    Style::new().bg(SELECTED).add_modifier(Modifier::BOLD)
}
pub fn tab_active() -> Style {
    Style::new().fg(ON_ACCENT).bg(ACCENT).add_modifier(Modifier::BOLD)
}
pub fn tab_inactive() -> Style {
    Style::new().fg(DIM)
}
