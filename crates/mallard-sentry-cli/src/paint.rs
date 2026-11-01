//! ANSI styling for CLI output: `mallard_term::color`, plus colours for
//! levels and statuses. Styling is always added; `Out::line` strips it when
//! colour is off.

use mallard_term::color::blue;
pub(crate) use mallard_term::color::{bold, cyan, dim, green, highlight, magenta, red, yellow};

/// The word in `[error]` or a padded `error   `, to pick a color by.
fn word(s: &str) -> &str {
    s.trim().trim_start_matches('[').trim_end_matches(']')
}

/// Color a level name: fatal magenta, error red, warning yellow, info blue.
/// `l` may be bracketed or padded.
pub(crate) fn level(l: &str) -> String {
    match word(l) {
        "fatal" => magenta(l),
        "error" => red(l),
        "warning" => yellow(l),
        "info" => blue(l),
        _ => dim(l),
    }
}

/// Color an issue status; `s` may be padded.
pub(crate) fn status(s: &str) -> String {
    match word(s) {
        "unresolved" => yellow(s),
        "resolved" => green(s),
        _ => dim(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_picked_by_the_bare_word() {
        assert_eq!(level("[error]"), red("[error]"));
        assert_eq!(level("fatal   "), magenta("fatal   "));
        assert_eq!(status("resolved  "), green("resolved  "));
        assert_eq!(level("debug"), dim("debug"));
    }
}
