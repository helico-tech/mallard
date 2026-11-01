//! OTLP log severity, shared by the CLI and the TUI.

/// Minimum severity number for a level name (`warn`, `err`, …) or a number
/// from 1 to 24.
pub(crate) fn parse(s: &str) -> Option<u8> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "trace" => 1,
        "debug" => 5,
        "info" => 9,
        "warn" | "warning" => 13,
        "error" | "err" => 17,
        "fatal" => 21,
        n => n.parse::<u8>().ok().filter(|v| (1..=24).contains(v))?,
    })
}

/// Short name for a severity number. Without a valid number (0 means
/// unspecified) the severity text decides; `-` when that says nothing either.
pub(crate) fn name(n: u8, text: &str) -> &'static str {
    match n {
        1..=4 => "TRACE",
        5..=8 => "DEBUG",
        9..=12 => "INFO",
        13..=16 => "WARN",
        17..=20 => "ERROR",
        21..=24 => "FATAL",
        _ => match text.to_ascii_uppercase().as_str() {
            t if t.starts_with("FATAL") => "FATAL",
            t if t.starts_with("ERR") => "ERROR",
            t if t.starts_with("WARN") => "WARN",
            t if t.starts_with("INFO") => "INFO",
            t if t.starts_with("DEBUG") => "DEBUG",
            t if t.starts_with("TRACE") => "TRACE",
            _ => "-",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_names() {
        assert_eq!(parse("err"), Some(17));
        assert_eq!(parse("Warning"), Some(13));
        assert_eq!(parse("24"), Some(24));
        assert_eq!(parse("25"), None);
        assert_eq!(parse("0"), None);
        assert_eq!(parse("nope"), None);
        assert_eq!(name(17, ""), "ERROR");
        assert_eq!(name(0, "warning"), "WARN");
        assert_eq!(name(0, "\u{1b}[2J"), "-");
        assert_eq!(name(30, ""), "-");
    }
}
