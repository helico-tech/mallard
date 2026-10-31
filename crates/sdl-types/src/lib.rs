//! Types of the `/v1` HTTP API. Shared by the server (which derives the
//! OpenAPI schema from them with the `openapi` feature), the client, the
//! CLI and the TUI, so all of them always agree on the wire format.

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
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "unresolved" => Some(Self::Unresolved),
            "resolved" => Some(Self::Resolved),
            "ignored" => Some(Self::Ignored),
            _ => None,
        }
    }
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct CreateProject {
    pub name: String,
    /// URL-safe identifier. Derived from `name` when omitted.
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct EventSummary {
    pub event_id: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(ToSchema))]
pub struct ApiError {
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(IntoParams))]
pub struct IssueQuery {
    /// `unresolved` (default), `resolved`, `ignored` or `all`.
    #[serde(default)]
    pub status: Option<String>,
    /// Case-insensitive text search in title and culprit.
    #[serde(default)]
    pub query: Option<String>,
    /// Window, e.g. `24h`, `14d` (default `14d`).
    #[serde(default)]
    pub since: Option<String>,
    /// `last_seen` (default), `first_seen`, `count`, `users`.
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    /// Max issues (default 50, max 500).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(IntoParams))]
pub struct EventQuery {
    /// Only events of this issue (16 hex characters).
    #[serde(default)]
    pub issue: Option<String>,
    /// Max events (default 50, max 500).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub before: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(IntoParams))]
pub struct WindowQuery {
    /// Window, e.g. `24h`, `14d` (default `24h` for stats, `14d` for issues).
    #[serde(default)]
    pub since: Option<String>,
    /// Bucket size for time series, e.g. `1h` (default: window / 24).
    #[serde(default)]
    pub interval: Option<String>,
}
