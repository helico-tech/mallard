//! Fixture data for TUI tests.

use super::app::*;
use chrono::{DateTime, Duration, TimeZone, Utc};
use sdl_client::types::*;

/// The text of a test terminal, one line per row.
pub(super) fn screen(term: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    let buf = term.backend().buffer();
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 31, 12, 0, 0).unwrap()
}

pub(super) fn projects() -> Vec<Project> {
    let p = |id: u32, slug: &str, name: &str| Project {
        id,
        slug: slug.into(),
        name: name.into(),
        platform: Some("node".into()),
        public_key: "abc".into(),
        dsn: format!("http://abc@localhost:8000/{id}"),
        created_at: now() - Duration::days(30),
    };
    vec![p(1, "web", "Web App"), p(2, "api", "API")]
}

fn issue(id: &str, title: &str, level: &str, count: u64, users: u64, mins_ago: i64) -> IssueSummary {
    let mut trend = vec![0u64; 24];
    trend[20] = count / 3;
    trend[23] = count - count / 3;
    IssueSummary {
        id: id.into(),
        project_id: 1,
        title: title.into(),
        culprit: Some("app/checkout.js in parseConfig".into()),
        level: level.into(),
        platform: "node".into(),
        exception_type: Some("TypeError".into()),
        status: IssueStatus::Unresolved,
        first_seen: now() - Duration::days(3),
        last_seen: now() - Duration::minutes(mins_ago),
        count,
        user_count: users,
        trend,
    }
}

pub(super) fn issues() -> Vec<IssueSummary> {
    vec![
        issue("a1", "TypeError: Cannot read properties of undefined (reading 'toUpperCase')", "error", 1234, 56, 3),
        issue("b2", "Error: inside span", "fatal", 42, 3, 90),
        issue("c3", "Payment 123 failed", "warning", 7, 1, 60 * 26),
    ]
}

pub(super) fn stats() -> ProjectStats {
    let buckets = (0..48u64)
        .map(|i| StatsBucket {
            start: now() - Duration::minutes(30 * (48 - i as i64)),
            events: (i % 7) * 3,
            errors: (i % 7) * 2,
            warnings: i % 7,
            other: 0,
        })
        .collect();
    ProjectStats {
        project_id: 1,
        window_start: now() - Duration::hours(24),
        window_end: now(),
        interval_seconds: 1800,
        total_events: 1283,
        unique_issues: 3,
        new_issues: 1,
        affected_users: 60,
        buckets,
    }
}

pub(super) fn issue_detail() -> IssueDetail {
    IssueDetail {
        summary: issues()[1].clone(),
        first_release: Some("web@1.2.0".into()),
        last_release: Some("web@1.2.3".into()),
        latest_event_id: Some("e1".into()),
        tags: vec![TagFacet {
            key: "browser".into(),
            unique: 2,
            top: vec![
                TagValueCount { value: "Chrome 141".into(), count: 30 },
                TagValueCount { value: "Firefox 140".into(), count: 12 },
            ],
        }],
        activity: vec![IssueActivity {
            at: now() - Duration::hours(2),
            status: IssueStatus::Resolved,
            reason: "user".into(),
        }],
        changed: None,
    }
}

pub(super) fn event_summary(id: &str, mins_ago: i64) -> EventSummary {
    EventSummary {
        event_id: id.into(),
        project_id: 1,
        issue_id: "b2".into(),
        timestamp: now() - Duration::minutes(mins_ago),
        received_at: now() - Duration::minutes(mins_ago),
        title: "Error: inside span".into(),
        level: "error".into(),
        platform: "node".into(),
        environment: Some("production".into()),
        release: Some("web@1.2.3".into()),
        user: Some("id:user-42".into()),
        message: None,
        trace_id: Some("b7644661850f40e69380d8c9ea253e84".into()),
        span_id: Some("b62d7c6abb676108".into()),
    }
}

pub(super) fn event_list() -> EventList {
    EventList { events: vec![event_summary("e1", 90), event_summary("e2", 200)], next_cursor: None }
}

pub(super) fn event_detail() -> EventDetail {
    EventDetail {
        summary: event_summary("e1", 90),
        culprit: Some("smoke".into()),
        tags: vec![("environment".into(), "production".into()), ("tenant".into(), "acme".into())],
        payload: crate::event_view::tests::node_payload(),
    }
}

pub(super) fn info() -> ServerInfo {
    ServerInfo {
        version: "0.1.0".into(),
        duckdb_version: "v2.0.0".into(),
        catalog: "c".into(),
        storage: "s".into(),
        queue_depth: 12,
        queue_capacity: 20000,
        public_url: None,
        retention_secs: None,
        max_window_secs: None,
        sql: None,
    }
}

/// App with projects, issues and stats loaded, as after startup.
pub(super) fn loaded_app() -> App {
    let mut app = App::new("http://127.0.0.1:8000", None);
    app.start();
    app.on_response(Resp::Projects(Ok(projects())));
    app.on_response(Resp::Info(Ok(ServerInfo {
        version: "0.1.0".into(),
        duckdb_version: "v2.0.0".into(),
        catalog: "c".into(),
        storage: "s".into(),
        queue_depth: 12,
        queue_capacity: 20000,
        public_url: None,
        retention_secs: None,
        max_window_secs: None,
        sql: None,
    })));
    let key = app.issues_key().unwrap();
    app.on_response(Resp::Issues(
        key,
        Ok(IssueList { issues: issues(), window_start: now(), window_end: now(), next_cursor: None }),
    ));
    app.on_response(Resp::Stats { project: "web".into(), window: app.window, result: Ok(stats()) });
    assert!(!app.loading(), "{:?}", app.pending);
    app.now = now();
    app
}

/// `loaded_app` with the detail view of issue `b2` open and event `e1` shown.
pub(super) fn detail_app() -> App {
    let mut app = loaded_app();
    app.issues_state.select(Some(1));
    let _ = app.on_key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter));
    app.on_response(Resp::Issue { id: "b2".into(), result: Ok(issue_detail()) });
    app.on_response(Resp::Events { issue: "b2".into(), before: None, result: Ok(event_list()) });
    app.on_response(Resp::Event { event_id: "e1".into(), result: Ok(event_detail()) });
    assert!(!app.loading(), "{:?}", app.pending);
    app.now = now();
    app
}
