//! Colours: the shared theme (`mallard_term::tui::theme`) plus the colours
//! of services, severities and error rates.

pub(crate) use mallard_term::tui::theme::*;
use ratatui::style::Color;

/// Distinct colors for services (stable per name).
const SERVICE_PALETTE: [Color; 10] = [
    BLUE,
    GREEN,
    ACCENT,
    YELLOW,
    CYAN,
    ORANGE,
    TEAL,
    RED,
    Color::Indexed(159), // pale cyan
    Color::Indexed(180), // tan
];

pub(crate) fn service_color(name: &str) -> Color {
    // FNV-1a: stable across runs and platforms.
    let mut h: u32 = 0x811c9dc5;
    for b in name.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    SERVICE_PALETTE[(h as usize) % SERVICE_PALETTE.len()]
}

/// Color for an error rate (0..1).
pub(crate) fn error_rate_color(r: f64) -> Color {
    if r >= 0.05 {
        RED
    } else if r >= 0.01 {
        YELLOW
    } else if r > 0.0 {
        KHAKI
    } else {
        GREEN
    }
}

/// Short severity label and color for an OTLP severity number.
pub(crate) fn severity(n: u8, text: &str) -> (&'static str, Color) {
    let name = crate::severity::name(n, text);
    let color = match name {
        "DEBUG" => DIM,
        "INFO" => CYAN,
        "WARN" => YELLOW,
        "ERROR" => RED,
        "FATAL" => MAGENTA,
        _ => FAINT,
    };
    (name, color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_service_colors() {
        assert_eq!(service_color("checkout"), service_color("checkout"));
        assert_eq!(severity(17, "").0, "ERROR");
        assert_eq!(severity(0, "warning").0, "WARN");
        assert_eq!(error_rate_color(0.0), GREEN);
        assert_eq!(error_rate_color(0.2), RED);
    }
}
