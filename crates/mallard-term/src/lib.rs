//! Terminal output shared by mallard's two CLIs (`sdl-cli`, `odl-cli`), their
//! TUIs and the `mallard` host, so `mallard errors …` and
//! `mallard telemetry …` look and behave like one tool (ADR 0006).
//!
//! - [`text`]: make untrusted text safe for a terminal; width-aware cutting.
//! - [`fmt`]: numbers, sizes, durations and times (always UTC).
//! - [`color`]: the colour policy (`--color`, TTY, `NO_COLOR`) and ANSI styling.
//! - [`table`]: the one table style (plain when piped).
//! - [`error`]: friendly error messages with hints, error kinds and exit
//!   codes, and printing them (text or JSON).
//! - [`output`]: `--format`, `--fields`, the list envelope (ADR 0008).
//! - [`window`]: the shared `--since/--from/--to` flags.
//! - [`tui`]: theme, shared widgets, the shared keys and [`tui::Outcome`].

#![warn(unreachable_pub)]

pub mod color;
pub mod error;
pub mod fmt;
pub mod output;
pub mod table;
pub mod text;
pub mod tui;
pub mod window;
