//! Minimal ANSI styling for CLI output. Off when stdout is not a terminal
//! or `NO_COLOR` is set.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

static COLOR: AtomicBool = AtomicBool::new(false);

pub fn init() {
    let on = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    COLOR.store(on, Ordering::Relaxed);
}

fn wrap(code: &str, s: &str) -> String {
    if COLOR.load(Ordering::Relaxed) { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
}

pub fn bold(s: &str) -> String {
    wrap("1", s)
}
pub fn dim(s: &str) -> String {
    wrap("2", s)
}
pub fn red(s: &str) -> String {
    wrap("31", s)
}
pub fn green(s: &str) -> String {
    wrap("32", s)
}
pub fn yellow(s: &str) -> String {
    wrap("33", s)
}
pub fn blue(s: &str) -> String {
    wrap("34", s)
}
pub fn magenta(s: &str) -> String {
    wrap("35", s)
}
pub fn cyan(s: &str) -> String {
    wrap("36", s)
}
/// Highlighted line (bold yellow), used for the error line in source context.
pub fn highlight(s: &str) -> String {
    wrap("1;33", s)
}

/// Color a level name: fatal magenta, error red, warning yellow, info blue.
pub fn level(l: &str) -> String {
    match l {
        "fatal" => magenta(l),
        "error" => red(l),
        "warning" => yellow(l),
        "info" => blue(l),
        _ => dim(l),
    }
}

pub fn status(s: &str) -> String {
    match s {
        "unresolved" => yellow(s),
        "resolved" => green(s),
        _ => dim(s),
    }
}
