//! Terminal output shared by mallard's two CLIs (`mallard-sentry-cli`, `mallard-otel-cli`), their
//! TUIs and the `mallard` host, so all commands look and behave like one tool
//! (ADR 0006).
//!
//! - [`text`]: make untrusted text safe for a terminal; width-aware cutting.
//! - [`fmt`]: numbers, sizes, durations and times (always UTC).
//! - [`color`]: the colour policy (`--color`, TTY, `NO_COLOR`) and ANSI styling.
//! - [`table`]: the one table style (plain when piped).
//! - [`error`]: friendly error messages with hints, error kinds and exit
//!   codes, and printing them (text or JSON).
//! - [`output`]: `--format`, `--fields`, the list envelope (ADR 0008).
//! - [`window`]: the shared `--since/--from/--to` flags.
//! - [`wait`]: `--wait <dur>`, poll until there is a result (exit 8 on timeout).
//! - [`tui`]: theme, shared widgets (also [`tui::suggest`], the filter bar's list), the
//!   shared keys, [`tui::Outcome`] and [`tui::Scope`].

#![warn(unreachable_pub)]

pub mod color;
pub mod error;
pub mod fmt;
pub mod output;
pub mod table;
pub mod text;
pub mod tui;
pub mod wait;
pub mod window;
