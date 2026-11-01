//! Colours: the shared theme (`mallard_term::tui::theme`) plus the colours
//! of levels and statuses.

pub(super) use mallard_term::tui::theme::*;
use ratatui::style::{Color, Style};

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

pub(super) fn level_badge(level: &str) -> Style {
    badge(level_color(level))
}

pub(super) fn status_color(status: &str) -> Color {
    match status {
        "unresolved" => ORANGE,
        "resolved" => GREEN,
        _ => DIM,
    }
}
