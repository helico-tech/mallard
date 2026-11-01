//! The one colour theme of both TUIs. It uses the 256-colour palette, not
//! true colour, so it looks the same in macOS Terminal, iTerm, tmux and
//! Linux terminals.

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
pub const KHAKI: Color = Color::Indexed(186);
pub const GREEN: Color = Color::Indexed(114);
pub const TEAL: Color = Color::Indexed(79);
pub const CYAN: Color = Color::Indexed(117);
pub const BLUE: Color = Color::Indexed(75);
pub const MAGENTA: Color = Color::Indexed(170);
pub const PINK: Color = Color::Indexed(205);

/// Series colours for charts, in order.
pub const SERIES: [Color; 8] = [BLUE, GREEN, YELLOW, ACCENT, CYAN, RED, TEAL, ORANGE];

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
pub fn error() -> Style {
    Style::new().fg(RED)
}
pub fn border() -> Style {
    Style::new().fg(BORDER)
}
pub fn border_focus() -> Style {
    Style::new().fg(ACCENT)
}
/// Panel titles.
pub fn title() -> Style {
    Style::new().fg(ACCENT2).add_modifier(Modifier::BOLD)
}
/// Table header rows.
pub fn header_row() -> Style {
    Style::new().fg(DIM).add_modifier(Modifier::BOLD)
}
/// A key in hints and help.
pub fn key() -> Style {
    Style::new().fg(ACCENT2).add_modifier(Modifier::BOLD)
}
/// The selected row of a list or table.
pub fn selected() -> Style {
    Style::new().bg(SELECTED).add_modifier(Modifier::BOLD)
}
/// Dark text on a coloured background.
pub fn badge(c: Color) -> Style {
    Style::new().fg(ON_ACCENT).bg(c).add_modifier(Modifier::BOLD)
}
pub fn tab_active() -> Style {
    badge(ACCENT)
}
pub fn tab_inactive() -> Style {
    Style::new().fg(DIM)
}
