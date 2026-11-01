//! Minimal ANSI styling for CLI output. Off when stdout is not a terminal
//! or `NO_COLOR` is set.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

static COLOR: AtomicBool = AtomicBool::new(false);

pub(crate) fn init() {
    let on = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    COLOR.store(on, Ordering::Relaxed);
}

fn wrap(code: &str, s: &str) -> String {
    if COLOR.load(Ordering::Relaxed) { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
}

pub(crate) fn bold(s: &str) -> String {
    wrap("1", s)
}
pub(crate) fn dim(s: &str) -> String {
    wrap("2", s)
}
pub(crate) fn red(s: &str) -> String {
    wrap("31", s)
}
pub(crate) fn green(s: &str) -> String {
    wrap("32", s)
}
pub(crate) fn yellow(s: &str) -> String {
    wrap("33", s)
}
pub(crate) fn blue(s: &str) -> String {
    wrap("34", s)
}
pub(crate) fn magenta(s: &str) -> String {
    wrap("35", s)
}
pub(crate) fn cyan(s: &str) -> String {
    wrap("36", s)
}
/// Highlighted line (bold yellow), used for the error line in source context.
pub(crate) fn highlight(s: &str) -> String {
    wrap("1;33", s)
}

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
        COLOR.store(true, Ordering::Relaxed);
        assert_eq!(level("[error]"), red("[error]"));
        assert_eq!(level("fatal   "), magenta("fatal   "));
        assert_eq!(status("resolved  "), green("resolved  "));
        assert_eq!(level("debug"), dim("debug"));
    }
}
