//! sdl: command-line tool and terminal UI for sentry-ducklake.
//!
//! Also a library, so other programs can mount the commands or embed the
//! TUI:
//! - [`Command`] + [`run`]: the CLI subcommands (global flags such as the
//!   server URL and token live in the host and arrive via [`Ctx`]).
//! - [`Embedded`]: the dashboard as a component without terminal setup.

#![warn(unreachable_pub)]

mod cli;
pub mod concise;
mod event_view;
mod fmt;
mod paint;
mod send_test;
mod tui;

pub use cli::{Ctx, HINTS, classify, create_or_get, describe, run};
pub use mallard_term::error::Hints;
pub use tui::{Embedded, Outcome};

use clap::{Args, Subcommand, ValueEnum};
use mallard_term::wait::WaitArgs;
use mallard_term::window::WindowArgs;

#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum Command {
    /// Server version, storage and ingest queue.
    Info,
    /// List, create and show projects.
    #[command(subcommand)]
    Projects(ProjectsCmd),
    /// List, inspect and change issues.
    #[command(subcommand)]
    Issues(IssuesCmd),
    /// List and inspect events.
    #[command(subcommand)]
    Events(EventsCmd),
    /// Event counts over time for the project (default window 24h).
    Stats {
        #[command(flatten)]
        window: WindowArgs,
        /// Bucket size, e.g. 1h (default: window / 24).
        #[arg(long)]
        interval: Option<String>,
    },
    /// Releases seen in events: first/last seen, events, issues, new issues.
    #[command(subcommand)]
    Releases(ReleasesCmd),
    /// Read-only SQL over events, issues and issue_activity (the server must enable it).
    Sql(SqlArgs),
    /// Uploaded sourcemap artifact bundles.
    #[command(subcommand)]
    Sourcemaps(SourcemapsCmd),
    /// Flush, compact and expire data now.
    Maintenance,
    /// Send a test error to a DSN with the Sentry envelope protocol.
    SendTest {
        /// DSN to send to, e.g. http://KEY@localhost:8000/1.
        #[arg(long, env = "SENTRY_DSN")]
        dsn: String,
        /// Error message of the test event.
        #[arg(long, default_value = "send-test: hello from the mallard CLI")]
        message: String,
    },
    /// Interactive terminal dashboard.
    Tui,
}

#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ProjectsCmd {
    /// All projects.
    List,
    /// Create a project and print its DSN.
    Create {
        name: String,
        /// URL-safe id (derived from the name when omitted).
        #[arg(long)]
        slug: Option<String>,
        /// SDK platform, e.g. node, javascript, python.
        #[arg(long)]
        platform: Option<String>,
        /// Print the existing project instead of failing when the slug is taken.
        #[arg(long)]
        if_not_exists: bool,
    },
    /// One project, with its DSN.
    Show { project: String },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum StatusFilter {
    Unresolved,
    Resolved,
    Ignored,
    All,
}

impl StatusFilter {
    pub const ALL: [StatusFilter; 4] =
        [StatusFilter::Unresolved, StatusFilter::Resolved, StatusFilter::Ignored, StatusFilter::All];

    /// The API's `status` value.
    pub fn as_str(self) -> &'static str {
        match self {
            StatusFilter::Unresolved => "unresolved",
            StatusFilter::Resolved => "resolved",
            StatusFilter::Ignored => "ignored",
            StatusFilter::All => "all",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum SortBy {
    #[value(name = "last_seen")]
    LastSeen,
    #[value(name = "first_seen")]
    FirstSeen,
    Count,
    Users,
}

impl SortBy {
    pub const ALL: [SortBy; 4] = [SortBy::LastSeen, SortBy::FirstSeen, SortBy::Count, SortBy::Users];

    /// The API's `sort` value.
    pub fn as_str(self) -> &'static str {
        match self {
            SortBy::LastSeen => "last_seen",
            SortBy::FirstSeen => "first_seen",
            SortBy::Count => "count",
            SortBy::Users => "users",
        }
    }

    /// Name of the sorted column.
    pub(crate) fn label(self) -> &'static str {
        match self {
            SortBy::LastSeen => "Last seen",
            SortBy::FirstSeen => "First seen",
            SortBy::Count => "Events",
            SortBy::Users => "Users",
        }
    }
}

#[derive(Args, Debug)]
pub struct IssueListArgs {
    #[arg(long, value_enum, default_value = "unresolved")]
    pub status: StatusFilter,
    /// Text search in title and culprit.
    #[arg(long, short = 'q')]
    pub query: Option<String>,
    /// Issues with events in this window (default 14d).
    #[command(flatten)]
    pub window: WindowArgs,
    /// Only issues first seen in the window (new issues).
    #[arg(long)]
    pub new: bool,
    /// Only issues first seen in this release.
    #[arg(long)]
    pub first_release: Option<String>,
    #[arg(long, value_enum, default_value = "last_seen")]
    pub sort: SortBy,
    /// Only this environment.
    #[arg(long)]
    pub env: Option<String>,
    /// Only issues with events in this release.
    #[arg(long)]
    pub release: Option<String>,
    /// Only issues with events that have this tag, `key=value`; repeat for more (all must match).
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_tag)]
    pub tag: Vec<String>,
    /// Max issues (server default 50, max 500).
    #[arg(short = 'n', long)]
    pub limit: Option<u32>,
    /// Continue a list: the `next_cursor` of the previous page (same
    /// filters; the cursor keeps the window and the sort).
    #[arg(long)]
    pub cursor: Option<String>,
}

/// `--tag key=value` → the API's `key:value`. Keys can't contain `:`.
pub fn parse_tag(s: &str) -> Result<String, String> {
    match s.split_once('=') {
        Some((k, v)) if !k.trim().is_empty() && !k.contains(':') => Ok(format!("{k}:{v}")),
        _ => Err(format!("expected key=value (key without ':'), got {s:?}")),
    }
}

#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum IssuesCmd {
    /// Issues with counts and a trend sparkline.
    List(IssueListArgs),
    /// One issue: details, releases, tag breakdown and (JSON) the latest event.
    Show {
        id: String,
        /// Window for counts and tags (default 14d).
        #[command(flatten)]
        window: WindowArgs,
    },
    /// Mark issues resolved (they reopen on a new event).
    Resolve(SetStatusArgs),
    /// Mark issues ignored.
    Ignore(SetStatusArgs),
    /// Mark issues unresolved again.
    Unresolve(SetStatusArgs),
}

/// Issue ids and why. Already in that status: nothing changes (`changed: false`).
#[derive(Args, Debug)]
pub struct SetStatusArgs {
    #[arg(required = true)]
    pub ids: Vec<String>,
    /// Why, e.g. "fixed in #123" (shown in the issue's activity).
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum EventsCmd {
    /// Newest events first.
    List {
        /// Only events of this issue.
        #[arg(long)]
        issue: Option<String>,
        /// Only events of this trace id (searches all projects).
        #[arg(short, long, conflicts_with = "issue")]
        trace: Option<String>,
        /// Max events (default 50, max 500).
        #[arg(short = 'n', long)]
        limit: Option<u32>,
        /// Only events in this window (default: all; with --trace 30d).
        #[command(flatten)]
        window: WindowArgs,
        /// Only events with this tag, `key=value`; repeat for more (all must match).
        #[arg(long, value_name = "KEY=VALUE", value_parser = parse_tag, conflicts_with = "trace")]
        tag: Vec<String>,
        /// Only this release.
        #[arg(long, conflicts_with = "trace")]
        release: Option<String>,
        /// Only this environment.
        #[arg(long, conflicts_with = "trace")]
        env: Option<String>,
        /// Text search in title and message.
        #[arg(short = 'q', long, conflicts_with = "trace")]
        query: Option<String>,
        /// Continue a list: the `next_cursor` of the previous page.
        #[arg(long, conflicts_with = "trace")]
        cursor: Option<String>,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// One event: exception, stack trace, breadcrumbs, tags, user, request
    /// (JSON: concise; --raw adds the Sentry payload).
    Show {
        event_id: String,
        /// Issue of the event (makes the lookup much faster).
        #[arg(long)]
        issue: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ReleasesCmd {
    /// Releases named by events in the window (default 14d), newest first.
    List {
        #[command(flatten)]
        window: WindowArgs,
        /// Max releases (default 50, max 500).
        #[arg(short = 'n', long)]
        limit: Option<u32>,
    },
}

/// `sql`.
#[derive(Args, Debug, Clone)]
pub struct SqlArgs {
    /// One SELECT over `events`, `issues` and `issue_activity`.
    pub query: String,
    /// Max rows.
    #[arg(short = 'n', long, default_value_t = 100)]
    pub limit: u32,
}

#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum SourcemapsCmd {
    /// Artifact bundles of the project.
    List,
}
