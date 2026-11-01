//! sdl: command-line tool and terminal UI for sentry-ducklake.
//!
//! Also a library, so other programs can mount the commands or embed the
//! TUI:
//! - [`Command`] + [`run`]: the CLI subcommands (global flags such as the
//!   server URL and token live in the host and arrive via [`Ctx`]).
//! - [`Embedded`]: the dashboard as a component without terminal setup.

#![warn(unreachable_pub)]

mod cli;
mod event_view;
mod fmt;
mod paint;
mod send_test;
mod tui;

pub use cli::{Ctx, HINTS, describe, run};
pub use mallard_term::error::Hints;
pub use tui::{Embedded, Outcome};

use clap::{Args, Subcommand, ValueEnum};

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
    /// Event counts over time for the project.
    Stats {
        /// Window, e.g. 1h, 24h, 7d.
        #[arg(short, long, default_value = "24h")]
        since: String,
        /// Bucket size, e.g. 1h (default: window / 24).
        #[arg(long)]
        interval: Option<String>,
    },
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
        #[arg(long, default_value = "sdl send-test: hello from the sdl CLI")]
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
    /// Window, e.g. 1h, 24h, 14d (server default 14d).
    #[arg(short, long)]
    pub since: Option<String>,
    #[arg(long, value_enum, default_value = "last_seen")]
    pub sort: SortBy,
    /// Only this environment.
    #[arg(long)]
    pub env: Option<String>,
    /// Only this release.
    #[arg(long)]
    pub release: Option<String>,
    /// Max issues (server default 50, max 500).
    #[arg(short = 'n', long)]
    pub limit: Option<u32>,
}

#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum IssuesCmd {
    /// Issues with counts and a trend sparkline.
    List(IssueListArgs),
    /// One issue: details, releases and tag breakdown.
    Show {
        id: String,
        /// Window for counts and tags, e.g. 24h (server default 14d).
        #[arg(short, long)]
        since: Option<String>,
    },
    /// Mark issues resolved (they reopen on a new event).
    Resolve {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Mark issues ignored.
    Ignore {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Mark issues unresolved again.
    Unresolve {
        #[arg(required = true)]
        ids: Vec<String>,
    },
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
        /// Max events.
        #[arg(short = 'n', long)]
        limit: Option<u32>,
    },
    /// One event: exception, stack trace, breadcrumbs, tags, user, request.
    Show {
        event_id: String,
        /// Issue of the event (makes the lookup much faster).
        #[arg(long)]
        issue: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum SourcemapsCmd {
    /// Artifact bundles of the project.
    List,
}
