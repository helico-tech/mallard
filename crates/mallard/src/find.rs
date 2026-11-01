//! `mallard find <id>`: what is this id? `GET /api/v1/find` knows: 32 hex
//! characters can be a trace or a Sentry event, 16 hex characters an issue (every
//! project is tried) or a span. Each match says how to see more (`next`).
//! No match is exit code 4. Free values (request ids, user ids) are not
//! searched yet.

use std::time::Duration;

use anyhow::Result;
use mallard_client::mallard_api::{FindId, FindMatch, FindQuery};
use mallard_term::error::{Described, Kind, usage};
use mallard_term::output::List;
use mallard_term::text::clean;
use mallard_term::wait::{WaitArgs, until};
use mallard_term::window::WindowArgs;
use serde::Serialize;

use crate::Run;

/// Default window for traces and their logs.
const WINDOW: Duration = Duration::from_secs(7 * 86_400);

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Match {
    Trace { trace_id: String, spans: usize, errors: usize, logs: usize, next: String },
    Event { event_id: String, issue_id: String, project_id: u32, title: String, trace_id: Option<String>, next: String },
    Issue { issue_id: String, project: String, title: String, status: String, count: u64, next: String },
    Span { span_id: String, trace_id: String, service: String, name: String, next: String },
}

/// What the server found, with the command to see more.
impl From<FindMatch> for Match {
    fn from(m: FindMatch) -> Match {
        match m {
            FindMatch::Trace { trace_id, spans, errors, logs, .. } => Match::Trace {
                next: format!("mallard trace {trace_id}"),
                trace_id,
                spans: spans as usize,
                errors: errors as usize,
                logs: logs as usize,
            },
            FindMatch::Event { event_id, issue_id, project_id, title, trace_id, .. } => Match::Event {
                next: format!("mallard events show {event_id} --issue {issue_id} -p {project_id}"),
                event_id,
                issue_id,
                project_id,
                title,
                trace_id,
            },
            FindMatch::Issue { issue_id, project, title, status, count, .. } => Match::Issue {
                next: format!("mallard issues show {issue_id} -p {project}"),
                issue_id,
                project,
                title,
                status,
                count,
            },
            FindMatch::Span { span_id, trace_id, service, name, .. } => {
                Match::Span { next: format!("mallard trace {trace_id}"), span_id, trace_id, service, name }
            }
        }
    }
}

impl Match {
    fn line(&self) -> String {
        match self {
            Match::Trace { trace_id, spans, errors, logs, next } => {
                format!("trace  {trace_id}  {spans} spans, {errors} errors, {logs} logs  → {next}")
            }
            Match::Event { event_id, title, next, .. } => format!("event  {event_id}  {}  → {next}", clean(title)),
            Match::Issue { issue_id, title, status, count, next, .. } => {
                format!("issue  {issue_id}  {}  ({status}, {count} events)  → {next}", clean(title))
            }
            Match::Span { span_id, service, name, next, .. } => {
                format!("span   {span_id}  {}  {}  → {next}", clean(service), clean(name))
            }
        }
    }
}

pub(crate) async fn run(r: &Run, id: &str, window: &WindowArgs, wait: &WaitArgs) -> Result<()> {
    let hex = FindId::parse(id).ok_or_else(|| {
        usage(format!(
            "{:?} is not an id mallard can look up: use a trace or event id (32 hex) or an issue or span id (16 hex)",
            clean(id)
        ))
    })?;
    let (FindId::Hex32(hex) | FindId::Hex16(hex)) = hex;
    // The window is resolved on every try: with --wait it moves along.
    let fetch = || async {
        let w = window.resolve(WINDOW)?;
        let q =
            FindQuery { id: hex.clone(), project: r.project.clone(), from: Some(w.from), to: Some(w.to), since: None };
        let found = r.client.find(&q).await.map_err(|e| crate::described(e, r.client.base_url()))?;
        let matches: Vec<Match> = found.map_or_else(Vec::new, |f| f.matches.into_iter().map(Match::from).collect());
        anyhow::Ok((w, matches))
    };
    let ((w, matches), waited) = until(&r.output, wait, fetch, |(_, m)| !m.is_empty()).await?.split();
    let list = List::new(matches).window(w);
    if r.output.text() {
        for m in &list.items {
            r.output.out.line(&m.line())?;
        }
    } else {
        r.output.list(&list)?;
    }
    waited?;
    if list.items.is_empty() {
        return Err(anyhow::Error::new(
            Described::new(Kind::NotFound, format!("nothing found for {id}"))
                .hint("data can take a few seconds to arrive; widen the window with --since"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_next_command_of_each_match() {
        use chrono::DateTime;
        let time = DateTime::UNIX_EPOCH;
        let next = |m: FindMatch| match Match::from(m) {
            Match::Trace { next, .. }
            | Match::Event { next, .. }
            | Match::Issue { next, .. }
            | Match::Span { next, .. } => next,
        };
        let t = FindMatch::Trace { trace_id: "t".into(), time, spans: 1, errors: 0, logs: 0 };
        assert_eq!(next(t), "mallard trace t");
        let e = FindMatch::Event {
            event_id: "e".into(),
            issue_id: "i".into(),
            project: "web".into(),
            project_id: 3,
            title: "x".into(),
            trace_id: None,
        };
        assert_eq!(next(e), "mallard events show e --issue i -p 3");
        let i = FindMatch::Issue {
            issue_id: "i".into(),
            project: "web".into(),
            project_id: 3,
            title: "x".into(),
            status: "unresolved".into(),
            count: 1,
        };
        assert_eq!(next(i), "mallard issues show i -p web");
        let s =
            FindMatch::Span { span_id: "s".into(), trace_id: "t".into(), service: "a".into(), name: "b".into(), time };
        assert_eq!(next(s), "mallard trace t");
    }

    #[test]
    fn matches_say_what_they_are() {
        let m = Match::Trace { trace_id: "t".into(), spans: 1, errors: 2, logs: 3, next: "mallard trace t".into() };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"kind": "trace", "trace_id": "t", "spans": 1, "errors": 2, "logs": 3, "next": "mallard trace t"})
        );
        assert!(m.line().contains("→ mallard trace t"));
        let m = Match::Span {
            span_id: "s".into(),
            trace_id: "t".into(),
            service: "checkout".into(),
            name: "POST /pay".into(),
            next: "mallard trace t".into(),
        };
        assert_eq!(serde_json::to_value(&m).unwrap()["kind"], "span");
        assert!(m.line().starts_with("span   s  checkout  POST /pay"), "{}", m.line());
    }
}
