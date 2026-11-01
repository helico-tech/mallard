//! Colour policy and ANSI styling for CLI output.
//!
//! The style functions always add ANSI codes. Whether they reach the
//! terminal is decided once, when the text is written ([`Out::line`]): with
//! colour off the codes are stripped. So printers need no global state and
//! no flag, and untrusted text is safe as long as it went through
//! [`crate::text::clean`] first (then the only escapes are ours).

use std::io::{IsTerminal, Write};

/// The `--color` flag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    /// Colour when the stream is a terminal, `NO_COLOR` is unset or empty
    /// and `TERM` is not `dumb`.
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    /// Whether a stream that is (or is not) a terminal gets colour.
    pub fn enabled(self, is_tty: bool) -> bool {
        match self {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => {
                is_tty
                    && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
                    && std::env::var("TERM").map_or(true, |t| t != "dumb")
            }
        }
    }
}

fn wrap(code: &str, s: &str) -> String {
    format!("\x1b[{code}m{s}\x1b[0m")
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
/// Bold yellow, e.g. the error line in source context.
pub fn highlight(s: &str) -> String {
    wrap("1;33", s)
}
/// Bold red, e.g. the `error:` label.
pub fn alert(s: &str) -> String {
    wrap("1;31", s)
}

/// Remove ANSI CSI sequences (`ESC [ … final`). Only our own styling has
/// them: data is cleaned before it is styled.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' && it.peek() == Some(&'[') {
            it.next();
            for d in it.by_ref() {
                if ('@'..='~').contains(&d) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Standard output, with or without colour. Cheap to copy; decide it once
/// per command with [`Out::stdout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Out {
    pub color: bool,
}

impl Out {
    /// Colour as `choice` says for stdout.
    pub fn stdout(choice: ColorChoice) -> Out {
        Out { color: choice.enabled(std::io::stdout().is_terminal()) }
    }

    /// Write `text` and a final newline (if it has none) to stdout. Write
    /// errors are returned: a closed pipe (`… | head`) is a `BrokenPipe`
    /// error that [`crate::error::report`] treats as success.
    pub fn line(&self, text: &str) -> std::io::Result<()> {
        write_line(&mut std::io::stdout().lock(), text, self.color)
    }
}

fn write_line(w: &mut impl Write, text: &str, color: bool) -> std::io::Result<()> {
    let text = if color { std::borrow::Cow::Borrowed(text) } else { std::borrow::Cow::Owned(strip_ansi(text)) };
    w.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        w.write_all(b"\n")?;
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BrokenPipe;
    impl Write for BrokenPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn colour_is_stripped_when_off_and_kept_when_on() {
        let mut buf = Vec::new();
        write_line(&mut buf, &red("red"), false).unwrap();
        assert_eq!(buf, b"red\n");
        let mut buf = Vec::new();
        write_line(&mut buf, &format!("{}\n", red("red")), true).unwrap();
        assert_eq!(buf, b"\x1b[31mred\x1b[0m\n", "one newline, codes kept");
    }

    #[test]
    fn write_errors_reach_the_caller() {
        let e = write_line(&mut BrokenPipe, "x", false).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn choice() {
        assert!(ColorChoice::Always.enabled(false));
        assert!(!ColorChoice::Never.enabled(true));
        assert!(!ColorChoice::Auto.enabled(false), "never colour into a pipe by default");
    }

    #[test]
    fn strip_only_removes_csi() {
        assert_eq!(strip_ansi(&bold(&cyan("x"))), "x");
        assert_eq!(strip_ansi("a\\u{1b}[2J"), "a\\u{1b}[2J", "cleaned data is left alone");
    }
}
