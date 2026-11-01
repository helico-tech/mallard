//! Types of the API `mallard-server` adds over both sides (`/api/v1/...`): what
//! needs one list of apps or one answer for an id (ADR 0027, 0028). Here so the
//! server and the slim CLI share them without DuckDB (ADR 0023).

use chrono::{DateTime, Utc};
pub use mallard_common::ApiError;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AppsQuery {
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending at `to`, e.g. `15m`, `1h`, `7d` (default `24h`). Ignored when `from` is set.
    pub since: Option<String>,
}

/// A project and the services whose slugs are equal to its slug, or either alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct App {
    pub slug: String,
    /// The project's name, or else the first service name.
    pub name: String,
    /// The project id, when the app has a project. Projects are listed whatever the window.
    pub project: Option<u32>,
    /// The service names with data in the window, sorted. May be empty.
    pub services: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AppList {
    /// Sorted by slug.
    pub apps: Vec<App>,
    /// What is missing or cut, e.g. when one side could not be read.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FindQuery {
    /// A trace or event id (32 hex, dashes allowed), or an issue or span id (16 hex).
    pub id: String,
    /// Look for issues in this project only (id or slug). Default: all projects.
    pub project: Option<String>,
    /// Start of the window for traces and spans (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Relative window ending at `to`, e.g. `15m`, `1h`, `7d` (default `7d`). Ignored when `from` is set.
    pub since: Option<String>,
}

/// One thing an id points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FindMatch {
    /// A trace with data in the window. Counts stop at 100000 spans, 50 errors and 1000 logs.
    Trace {
        trace_id: String,
        /// The earliest span, error or log found. Pass it to the trace page, which
        /// only looks back 7 days without a time.
        time: DateTime<Utc>,
        spans: u64,
        errors: u64,
        logs: u64,
    },
    /// A Sentry event, in any project and at any age.
    Event {
        event_id: String,
        issue_id: String,
        /// The project's slug, which links use.
        project: String,
        project_id: u32,
        title: String,
        trace_id: Option<String>,
    },
    /// A Sentry issue. An id can exist in several projects.
    Issue {
        issue_id: String,
        /// The project's slug, which links use.
        project: String,
        project_id: u32,
        title: String,
        status: String,
        /// Events in the last 14 days.
        count: u64,
    },
    /// A span in the window.
    Span {
        span_id: String,
        trace_id: String,
        service: String,
        name: String,
        /// When the span started.
        time: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FindResult {
    /// Never empty: no match is a 404.
    pub matches: Vec<FindMatch>,
    /// Sources that could not be read, while something else was found.
    pub warnings: Vec<String>,
}

/// What kind of id this can be, in lowercase hex without dashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindId {
    /// Trace id or Sentry event id.
    Hex32(String),
    /// Issue id or span id.
    Hex16(String),
}

impl FindId {
    pub fn parse(id: &str) -> Option<FindId> {
        let hex = id.trim().chars().filter(|c| *c != '-').collect::<String>().to_ascii_lowercase();
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        match hex.len() {
            32 => Some(FindId::Hex32(hex)),
            16 => Some(FindId::Hex16(hex)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_by_length() {
        let t = "0af7651916cd43dd8448eb211c80319c";
        assert_eq!(FindId::parse(t), Some(FindId::Hex32(t.into())));
        assert_eq!(FindId::parse("0AF76519-16CD-43DD-8448-EB211C80319C"), Some(FindId::Hex32(t.into())));
        assert_eq!(FindId::parse("85fede14250ec6be"), Some(FindId::Hex16("85fede14250ec6be".into())));
        assert_eq!(FindId::parse("req-7f3a"), None);
        assert_eq!(FindId::parse("85fede14250ec6bz"), None);
        assert_eq!(FindId::parse("' OR 1=1 --"), None);
    }

    #[test]
    fn matches_say_what_they_are() {
        let m = FindMatch::Span {
            span_id: "s".into(),
            trace_id: "t".into(),
            service: "checkout".into(),
            name: "POST /pay".into(),
            time: DateTime::UNIX_EPOCH,
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["kind"], "span");
        assert_eq!(serde_json::from_value::<FindMatch>(v).unwrap(), m);
    }
}
