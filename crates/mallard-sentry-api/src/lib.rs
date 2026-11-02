//! Types of the `/v1` HTTP API. Shared by the server (which derives the
//! OpenAPI schema from them with the `openapi` feature), the client, the
//! CLI and the TUI, so all of them always agree on the wire format.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[cfg(feature = "openapi")]
use utoipa::{IntoParams, ToSchema};

/// Issue status. Resolved issues reopen automatically on a new event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum IssueStatus {
    #[default]
    Unresolved,
    Resolved,
    Ignored,
}

impl IssueStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            IssueStatus::Unresolved => "unresolved",
            IssueStatus::Resolved => "resolved",
            IssueStatus::Ignored => "ignored",
        }
    }
}

impl std::fmt::Display for IssueStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for IssueStatus {
    type Err = UnknownStatus;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unresolved" => Ok(Self::Unresolved),
            "resolved" => Ok(Self::Resolved),
            "ignored" => Ok(Self::Ignored),
            _ => Err(UnknownStatus(s.to_string())),
        }
    }
}

/// A string that is not an [`IssueStatus`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownStatus(pub String);

impl std::fmt::Display for UnknownStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown issue status {:?} (unresolved, resolved or ignored)", self.0)
    }
}

impl std::error::Error for UnknownStatus {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct Project {
    pub id: u32,
    pub slug: String,
    pub name: String,
    pub platform: Option<String>,
    pub public_key: String,
    /// DSN to configure in the Sentry SDK.
    pub dsn: String,
    pub created_at: DateTime<Utc>,
}

/// The slug the server derives from a project name when none is given:
/// lowercase ASCII letters and digits, other runs as one `-`.
pub fn slugify(name: &str) -> String {
    let mut s = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    s.trim_end_matches('-').to_string()
}

/// A valid project slug for a service name (`unknown_service:node` →
/// `unknown-service-node`). The one slug rule (ADR 0027, 0029): a project and a service with
/// the same slug under it are one app, and the bridge sends exceptions there. Project slugs
/// go through it too, see [`app_projects`].
pub fn service_slug(service: &str) -> String {
    let mut s = slugify(service);
    s.truncate(64);
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() {
        "unknown-service".into()
    } else if s.bytes().all(|b| b.is_ascii_digit()) {
        format!("service-{s}")
    } else {
        s
    }
}

/// The project that takes each [`service_slug`], from `(id, slug)`. A project may have a slug
/// the rule never gives (`my_app`), and it still joins the service `my_app`. If several
/// projects share a slug under the rule, the one whose slug is exactly that wins, and then
/// the oldest. The others stay apps of their own.
pub fn app_projects<'a>(projects: impl IntoIterator<Item = (u32, &'a str)>) -> HashMap<String, (u32, &'a str)> {
    let mut found: HashMap<String, (u32, &str)> = HashMap::new();
    for (id, slug) in projects {
        let key = service_slug(slug);
        let rank = |(id, slug): (u32, &str)| (slug != key, id);
        match found.get_mut(&key) {
            Some(best) if rank((id, slug)) < rank(*best) => *best = (id, slug),
            Some(_) => {}
            None => {
                found.insert(key, (id, slug));
            }
        }
    }
    found
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct CreateProject {
    pub name: String,
    /// URL-safe identifier. Derived from `name` when omitted. A project that has the same
    /// slug under the slug rule (`my_app` next to `my-app`) is a 409, also for a slug that was
    /// asked for, because a second project would take its services.
    pub slug: Option<String>,
    pub platform: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct IssueSummary {
    /// 16 hex characters (the group hash).
    pub id: String,
    pub project_id: u32,
    pub title: String,
    pub culprit: Option<String>,
    pub level: String,
    pub platform: String,
    pub exception_type: Option<String>,
    pub status: IssueStatus,
    pub first_seen: DateTime<Utc>,
    /// Last event in the queried window.
    pub last_seen: DateTime<Utc>,
    /// Events in the queried window.
    pub count: u64,
    /// Distinct users in the queried window.
    pub user_count: u64,
    /// Event counts in equal buckets over the window, oldest first.
    pub trend: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct IssueList {
    pub issues: Vec<IssueSummary>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    /// Pass as `cursor` to fetch the next page (same filters); `None` on
    /// the last page. The cursor carries the window and the sort.
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct TagValueCount {
    pub value: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct TagFacet {
    pub key: String,
    /// Distinct values of this key in the window.
    pub unique: u64,
    pub top: Vec<TagValueCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct IssueDetail {
    #[serde(flatten)]
    pub summary: IssueSummary,
    pub first_release: Option<String>,
    pub last_release: Option<String>,
    pub latest_event_id: Option<String>,
    pub tags: Vec<TagFacet>,
    pub activity: Vec<IssueActivity>,
    /// Only in `PATCH` answers: false when the issue already had that status
    /// (nothing was written).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct IssueActivity {
    pub at: DateTime<Utc>,
    pub status: IssueStatus,
    /// `user`, `regression`, ...
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct UpdateIssue {
    pub status: IssueStatus,
    /// Why, e.g. `fixed in #123` (max 500 characters; default `user`).
    /// Shown in the issue's activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct EventSummary {
    pub event_id: String,
    pub project_id: u32,
    pub issue_id: String,
    pub timestamp: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub title: String,
    pub level: String,
    pub platform: String,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub user: Option<String>,
    pub message: Option<String>,
    /// Trace id (32 hex) from `contexts.trace`, for linking to traces.
    pub trace_id: Option<String>,
    /// Span id (16 hex) from `contexts.trace`.
    pub span_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct EventList {
    pub events: Vec<EventSummary>,
    /// Pass as `before` to fetch the next (older) page.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct EventDetail {
    #[serde(flatten)]
    pub summary: EventSummary,
    pub culprit: Option<String>,
    pub tags: Vec<(String, String)>,
    /// The full normalized (and symbolicated) Sentry event.
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct StatsBucket {
    pub start: DateTime<Utc>,
    pub events: u64,
    pub errors: u64,
    pub warnings: u64,
    pub other: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ProjectStats {
    pub project_id: u32,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub interval_seconds: u64,
    pub total_events: u64,
    pub unique_issues: u64,
    pub new_issues: u64,
    pub affected_users: u64,
    pub buckets: Vec<StatsBucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ArtifactBundle {
    /// SHA-1 of the bundle zip.
    pub id: String,
    pub release: Option<String>,
    pub dist: Option<String>,
    pub file_count: u32,
    pub size_bytes: u64,
    pub debug_ids: Vec<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ServerInfo {
    pub version: String,
    pub duckdb_version: String,
    pub catalog: String,
    pub storage: String,
    pub queue_depth: u64,
    pub queue_capacity: u64,
    /// Base URL the DSNs are built from, e.g. `https://errors.example.com/sentry`.
    #[serde(default)]
    pub public_url: Option<String>,
    /// How long events are kept, in seconds (0 = forever).
    #[serde(default)]
    pub retention_secs: Option<u64>,
    /// Longest query window (`sentry.query.max_window`), in seconds.
    #[serde(default)]
    pub max_window_secs: Option<u64>,
    /// `POST /v1/sql` is on (`sentry.query.enable_sql`).
    #[serde(default)]
    pub sql: Option<bool>,
}

/// Error body of every non-2xx `/v1` response: `error` is a code such as
/// `not_found`, `message` the text for people (shared with the OTel API).
pub use mallard_common::ApiError;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(IntoParams))]
pub struct IssueQuery {
    /// `unresolved` (default), `resolved`, `ignored` or `all`.
    pub status: Option<String>,
    /// Case-insensitive text search in title and culprit.
    pub query: Option<String>,
    /// Window, e.g. `24h`, `14d` (default `14d`).
    pub since: Option<String>,
    /// `last_seen` (default), `first_seen`, `count`, `users`.
    pub sort: Option<String>,
    pub environment: Option<String>,
    pub release: Option<String>,
    /// Max issues (default 50, max 500).
    pub limit: Option<u32>,
    /// Only issues first seen inside the window (new issues).
    pub new: Option<bool>,
    /// Only issues first seen in this release.
    pub first_release: Option<String>,
    /// Only events with these tags, `key:value` (all must match). Sent as
    /// repeated `tag` parameters, which the query string format here can't
    /// express: the server reads them apart and the client adds them.
    #[serde(skip)]
    pub tag: Vec<String>,
    /// Cursor from a previous page (`next_cursor`). It carries the window
    /// and the sort; pass the same filters.
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(IntoParams))]
pub struct EventQuery {
    /// Only events of this issue (16 hex characters).
    pub issue: Option<String>,
    /// Only events of this trace (32 hex characters).
    pub trace_id: Option<String>,
    /// Max events (default 50, max 500).
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    pub before: Option<String>,
    /// Only events received in this window, e.g. `1h` (with `from`/`to`;
    /// default: no window).
    pub since: Option<String>,
    /// Only events with these tags, `key:value` (all must match); repeated
    /// `tag` parameters (see [`IssueQuery::tag`]).
    #[serde(skip)]
    pub tag: Vec<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    /// Case-insensitive text search in title and message.
    pub query: Option<String>,
}

/// Most `tag` filters in one request.
pub const MAX_TAG_FILTERS: usize = 10;

/// Split a `key:value` tag filter at the first `:` (values may contain
/// `:`, keys may not). The key must not be empty.
pub fn parse_tag_filter(s: &str) -> Option<(&str, &str)> {
    s.split_once(':').filter(|(k, _)| !k.trim().is_empty())
}

/// A release, derived from the events that name it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct Release {
    pub version: String,
    /// First event of the release in the window.
    pub first_seen: DateTime<Utc>,
    /// Last event of the release in the window.
    pub last_seen: DateTime<Utc>,
    /// Events in the window.
    pub events: u64,
    /// Distinct issues with events in the window.
    pub issues: u64,
    /// Issues first seen in this release (`first_release`), at any time.
    pub new_issues: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ReleaseList {
    /// Newest `last_seen` first.
    pub releases: Vec<Release>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(IntoParams))]
pub struct ReleaseQuery {
    /// Window, e.g. `24h`, `30d` (default `14d`).
    pub since: Option<String>,
    /// Max releases (default 50, max 500).
    pub limit: Option<u32>,
}

/// `GET /v1/schema`: the tables `POST /v1/sql` can query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct Schema {
    pub tables: Vec<SchemaTable>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct SchemaTable {
    /// Table name; also reachable as `lake.sentry.<name>`.
    pub name: String,
    /// The table's time column; filter on it.
    pub time_column: String,
    /// Columns in storage order.
    pub columns: Vec<SqlColumn>,
}

/// `POST /v1/sql`: one read-only `SELECT` over `events`, `issues` and
/// `issue_activity` (schema `lake.sentry`, the default, so `FROM events`
/// works).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct SqlRequest {
    pub query: String,
    /// Max rows returned (default 1000, max 100000).
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct SqlResponse {
    pub columns: Vec<SqlColumn>,
    /// Row values as JSON. Other types (timestamps, UUIDs, maps) are strings
    /// or JSON objects.
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<Vec<Object>>))]
    pub rows: Vec<Vec<serde_json::Value>>,
    /// True when more rows existed than `limit`, or the rows reached the
    /// server's size cap (32 MiB of row data).
    pub truncated: bool,
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct SqlColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(IntoParams))]
pub struct TraceQuery {
    /// Trace id (32 hex characters). Required.
    pub trace_id: String,
    /// Only the event captured in this span (16 hex characters).
    pub span_id: Option<String>,
    /// Window to search, e.g. `24h`, `30d` (default `30d`).
    pub since: Option<String>,
    /// Max events (default 50, max 500).
    pub limit: Option<u32>,
}

/// Most trace ids in one `GET /v1/events/by-trace`.
pub const MAX_TRACES_PER_LOOKUP: usize = 200;
/// Most events listed per trace by `GET /v1/events/by-trace` (`count` has them all).
pub const MAX_EVENTS_PER_TRACE: u32 = 50;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(IntoParams))]
pub struct TracesQuery {
    /// Trace ids (32 hex characters), comma separated; at most 200. Required.
    pub trace_id: String,
    /// Window to search, e.g. `1h`, `30d` (default `30d`).
    pub since: Option<String>,
}

/// One error event of a trace, as `GET /v1/events/by-trace` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct TraceEvent {
    pub event_id: String,
    pub issue_id: String,
    pub project_id: u32,
    /// Span id (16 hex) the event was captured in.
    pub span_id: Option<String>,
    pub title: String,
}

/// The error events of one trace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct TraceEvents {
    /// All events of the trace in the window.
    pub count: u64,
    /// The newest events, at most 50.
    pub events: Vec<TraceEvent>,
}

/// Error events per trace id. Traces without events are left out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct EventsByTrace {
    pub traces: std::collections::BTreeMap<String, TraceEvents>,
}

/// Absolute window bounds and the bucket step, accepted next to `since` by
/// every endpoint with a window (same names as the OTel API). `from` wins
/// over `since`; `to` defaults to now.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(IntoParams))]
pub struct WindowRange {
    /// Start of the window (inclusive, RFC 3339).
    pub from: Option<DateTime<Utc>>,
    /// End of the window (exclusive, RFC 3339). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Bucket size for time series, e.g. `1h` (same as `interval`).
    pub step: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(IntoParams))]
pub struct TagQuery {
    /// Window, e.g. `1h`, `14d`, ending at `to` (default `14d`). Ignored when `from` is set.
    pub since: Option<String>,
    /// Top values of this tag instead of the tag keys.
    pub key: Option<String>,
    /// Only keys, or with `key` only values, that start with this (any case).
    pub prefix: Option<String>,
    /// Max keys or values returned (default 50, max 1000).
    pub limit: Option<u32>,
}

/// Tag keys (or values of one key) with how many sampled events have them.
/// The same shape as the telemetry side's `/api/v1/attributes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct TagStats {
    /// Matching events since `counted_from`.
    pub total_rows: u64,
    /// Events counted: a sample of at most 10,000 of the matching events.
    pub sampled_rows: u64,
    /// The events looked at start here: the newest part of the window with enough
    /// events (at least 500,000), or the start of the window. A value seen only
    /// before this is not listed.
    pub counted_from: DateTime<Utc>,
    /// Most frequent first.
    pub items: Vec<TagCount>,
}

/// One tag key, or one value of the key asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct TagCount {
    pub key: String,
    /// The value, when a `key` was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Sampled events with this key (or value).
    pub count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(IntoParams))]
pub struct WindowQuery {
    /// Window, e.g. `24h`, `14d` (default `24h` for stats, `14d` for issues).
    pub since: Option<String>,
    /// Bucket size for time series, e.g. `1h` (default: window / 24).
    pub interval: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_status_round_trips() {
        for s in [IssueStatus::Unresolved, IssueStatus::Resolved, IssueStatus::Ignored] {
            assert_eq!(s.to_string().parse::<IssueStatus>(), Ok(s));
        }
        assert_eq!("all".parse::<IssueStatus>(), Err(UnknownStatus("all".into())));
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("My Web App!"), "my-web-app");
        assert_eq!(slugify("  API  "), "api");
    }

    #[test]
    fn missing_optional_fields_are_none() {
        let q: IssueQuery = serde_json::from_str("{}").unwrap();
        assert!(q.status.is_none() && q.limit.is_none());
        let p: CreateProject = serde_json::from_str(r#"{"name": "x"}"#).unwrap();
        assert!(p.slug.is_none() && p.platform.is_none());
    }
}
