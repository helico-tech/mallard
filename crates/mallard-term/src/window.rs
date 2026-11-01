//! The time window flags every command with a window shares:
//! `-s/--since`, `--from`, `--to`. The window is resolved to absolute UTC
//! times once, in the CLI, so both sides get the same `from`/`to` and the
//! JSON output can say which window it covered.

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;

use crate::error::usage;

/// `--since 1h`, `--from 2026-11-01T09:00:00Z --to 2h`. Each command picks
/// its own default length (see [`WindowArgs::resolve`]).
#[derive(clap::Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowArgs {
    /// Window ending at --to (or now), e.g. 15m, 6h, 2d, 1h30m.
    #[arg(short, long, value_parser = parse_since, conflicts_with = "from")]
    pub since: Option<String>,
    /// Start: RFC 3339 (2026-11-01T09:00:00Z) or a duration ago (2h).
    #[arg(long, value_parser = parse_time)]
    pub from: Option<DateTime<Utc>>,
    /// End: RFC 3339 or a duration ago. Default: now.
    #[arg(long, value_parser = parse_time)]
    pub to: Option<DateTime<Utc>>,
}

/// A resolved window: `from` inclusive, `to` exclusive, UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Window {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

impl WindowArgs {
    /// Only `--since`.
    pub fn since(s: &str) -> Self {
        WindowArgs { since: Some(s.to_string()), ..Default::default() }
    }

    /// Whether any of the flags was given.
    pub fn is_set(&self) -> bool {
        self.since.is_some() || self.from.is_some() || self.to.is_some()
    }

    /// The window: `--to` or now, back `--since` (or `default`), unless
    /// `--from` says where it starts. A usage error when it is empty.
    pub fn resolve(&self, default: Duration) -> anyhow::Result<Window> {
        self.resolve_at(Utc::now(), default)
    }

    pub fn resolve_at(&self, now: DateTime<Utc>, default: Duration) -> anyhow::Result<Window> {
        let to = self.to.unwrap_or(now);
        let from = match (self.from, &self.since) {
            (Some(f), _) => f,
            (None, Some(s)) => back(to, parse_duration(s).map_err(|e| usage(&e))?)?,
            (None, None) => back(to, default)?,
        };
        if from >= to {
            return Err(usage(format!("empty time window: --from {} is not before --to {}", fmt(from), fmt(to))));
        }
        Ok(Window { from, to })
    }
}

fn back(to: DateTime<Utc>, d: Duration) -> anyhow::Result<DateTime<Utc>> {
    TimeDelta::from_std(d).ok().and_then(|d| to.checked_sub_signed(d)).ok_or_else(|| usage("time window too large"))
}

fn fmt(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// `15m`, `2d`, `1h30m`; longer than zero.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    match mallard_common::duration::parse(s) {
        Ok(d) if d.is_zero() => Err(format!("invalid duration {s:?}: must be longer than zero")),
        Ok(d) => Ok(d),
        Err(e) => Err(format!("invalid duration {s:?}: {e} (e.g. 15m, 6h, 2d)")),
    }
}

/// clap parser for `--since`: a valid duration, kept as typed.
pub fn parse_since(s: &str) -> Result<String, String> {
    parse_duration(s).map(|_| s.trim().to_string())
}

/// clap parser for `--from`/`--to`: RFC 3339, or a duration meaning that
/// long ago.
pub fn parse_time(s: &str) -> Result<DateTime<Utc>, String> {
    parse_time_at(s, Utc::now())
}

pub fn parse_time_at(s: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    match parse_duration(s) {
        Ok(d) => TimeDelta::from_std(d)
            .ok()
            .and_then(|d| now.checked_sub_signed(d))
            .ok_or_else(|| format!("{s:?} is too long ago")),
        Err(_) => Err(format!("invalid time {s:?}: use RFC 3339 (2026-11-01T09:00:00Z) or a duration ago (2h)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        w: WindowArgs,
    }

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn times_are_rfc3339_or_a_duration_ago() {
        let now = t("2026-11-01T10:00:00Z");
        assert_eq!(parse_time_at("2026-11-01T09:00:00Z", now), Ok(t("2026-11-01T09:00:00Z")));
        assert_eq!(parse_time_at("2026-11-01T11:00:00+02:00", now), Ok(t("2026-11-01T09:00:00Z")));
        assert_eq!(parse_time_at("2h", now), Ok(t("2026-11-01T08:00:00Z")));
        assert!(parse_time_at("yesterday", now).is_err());
        assert!(parse_time_at("0s", now).is_err());
    }

    #[test]
    fn resolve() {
        let now = t("2026-11-01T10:00:00Z");
        let day = Duration::from_secs(86_400);
        let w = WindowArgs::default().resolve_at(now, day).unwrap();
        assert_eq!(w, Window { from: t("2026-10-31T10:00:00Z"), to: now }, "default length");
        let w = WindowArgs::since("1h").resolve_at(now, day).unwrap();
        assert_eq!(w.from, t("2026-11-01T09:00:00Z"));
        let a = WindowArgs { since: Some("30m".into()), to: Some(t("2026-11-01T08:00:00Z")), from: None };
        assert_eq!(a.resolve_at(now, day).unwrap().from, t("2026-11-01T07:30:00Z"), "since counts back from --to");
        let a = WindowArgs { from: Some(t("2026-11-01T09:30:00Z")), ..Default::default() };
        assert_eq!(a.resolve_at(now, day).unwrap(), Window { from: t("2026-11-01T09:30:00Z"), to: now });
        let empty = WindowArgs { from: Some(now), to: Some(now), since: None };
        let e = empty.resolve_at(now, day).unwrap_err();
        assert_eq!(crate::error::exit_code(&Err(e)), 2, "an empty window is a usage error");
    }

    #[test]
    fn flags() {
        let c = Cli::try_parse_from(["x", "-s", "1h30m"]).unwrap();
        assert_eq!(c.w.since.as_deref(), Some("1h30m"));
        assert!(Cli::try_parse_from(["x", "--since", "bogus"]).is_err());
        assert!(Cli::try_parse_from(["x", "--since", "1h", "--from", "2h"]).is_err(), "since or from");
        let c = Cli::try_parse_from(["x", "--from", "2h", "--to", "1h"]).unwrap();
        assert!(c.w.from.unwrap() < c.w.to.unwrap());
    }
}
