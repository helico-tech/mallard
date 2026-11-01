//! Colors and styles. One palette for the whole UI (a dark "night" theme that
//! also reads well on light terminals because it never paints backgrounds
//! except for selections and badges).

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Rgb(122, 162, 247);
pub const ACCENT2: Color = Color::Rgb(187, 154, 247);
pub const CYAN: Color = Color::Rgb(125, 207, 255);
pub const OK: Color = Color::Rgb(158, 206, 106);
pub const WARN: Color = Color::Rgb(224, 175, 104);
pub const ERR: Color = Color::Rgb(247, 118, 142);
pub const TEXT: Color = Color::Rgb(192, 202, 245);
pub const DIM: Color = Color::Rgb(122, 130, 170);
pub const MUTED: Color = Color::Rgb(86, 95, 137);
pub const BORDER: Color = Color::Rgb(59, 66, 97);
pub const SURFACE: Color = Color::Rgb(41, 46, 66);
pub const SELECT: Color = Color::Rgb(54, 62, 94);

/// Distinct colors for services (stable per name).
const SERVICE_PALETTE: [Color; 10] = [
    Color::Rgb(122, 162, 247),
    Color::Rgb(158, 206, 106),
    Color::Rgb(187, 154, 247),
    Color::Rgb(224, 175, 104),
    Color::Rgb(125, 207, 255),
    Color::Rgb(255, 158, 100),
    Color::Rgb(115, 218, 202),
    Color::Rgb(255, 117, 127),
    Color::Rgb(180, 249, 248),
    Color::Rgb(201, 167, 107),
];

/// Series colors for charts.
pub const SERIES: [Color; 8] = [
    Color::Rgb(122, 162, 247),
    Color::Rgb(158, 206, 106),
    Color::Rgb(224, 175, 104),
    Color::Rgb(187, 154, 247),
    Color::Rgb(125, 207, 255),
    Color::Rgb(247, 118, 142),
    Color::Rgb(115, 218, 202),
    Color::Rgb(255, 158, 100),
];

pub fn service_color(name: &str) -> Color {
    // FNV-1a: stable across runs and platforms.
    let mut h: u32 = 0x811c9dc5;
    for b in name.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    SERVICE_PALETTE[(h as usize) % SERVICE_PALETTE.len()]
}

pub fn text() -> Style {
    Style::new().fg(TEXT)
}
pub fn dim() -> Style {
    Style::new().fg(DIM)
}
pub fn muted() -> Style {
    Style::new().fg(MUTED)
}
pub fn bold() -> Style {
    Style::new().fg(TEXT).add_modifier(Modifier::BOLD)
}
pub fn accent() -> Style {
    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
}
pub fn border() -> Style {
    Style::new().fg(BORDER)
}
pub fn border_focus() -> Style {
    Style::new().fg(ACCENT)
}
pub fn title() -> Style {
    Style::new().fg(ACCENT2).add_modifier(Modifier::BOLD)
}
pub fn selected() -> Style {
    Style::new().bg(SELECT).add_modifier(Modifier::BOLD)
}
pub fn header_row() -> Style {
    Style::new().fg(DIM).add_modifier(Modifier::BOLD)
}
pub fn error() -> Style {
    Style::new().fg(ERR)
}
pub fn key() -> Style {
    Style::new().fg(Color::Rgb(26, 27, 38)).bg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Color for an error rate (0..1).
pub fn error_rate_color(r: f64) -> Color {
    if r >= 0.05 {
        ERR
    } else if r >= 0.01 {
        WARN
    } else if r > 0.0 {
        Color::Rgb(220, 200, 120)
    } else {
        OK
    }
}

/// Short severity label and color for an OTLP severity number.
pub fn severity(n: u8, text: &str) -> (&'static str, Color) {
    let name = crate::severity::name(n, text);
    let color = match name {
        "DEBUG" => DIM,
        "INFO" => CYAN,
        "WARN" => WARN,
        "ERROR" => ERR,
        "FATAL" => Color::Rgb(255, 80, 110),
        _ => MUTED,
    };
    (name, color)
}

/// Badge style: dark text on a colored background.
pub fn badge(c: Color) -> Style {
    Style::new().fg(Color::Rgb(26, 27, 38)).bg(c).add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_service_colors() {
        assert_eq!(service_color("checkout"), service_color("checkout"));
        assert_eq!(severity(17, "").0, "ERROR");
        assert_eq!(severity(0, "warning").0, "WARN");
        assert_eq!(error_rate_color(0.0), OK);
        assert_eq!(error_rate_color(0.2), ERR);
    }
}
