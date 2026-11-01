//! `mallard find <id>`: what is this id? 32 hex characters can be a trace
//! or a Sentry event (both are tried), 16 hex characters an issue (every
//! project is tried) or a span. Each match says how to see more (`next`).
//! No match is exit code 4. Free values (request ids, user ids) are not
//! searched yet.

use std::time::Duration;

use anyhow::Result;
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

/// What kind of id this can be.
#[derive(Debug, PartialEq, Eq)]
enum IdKind {
    /// Trace id or Sentry event id.
    Hex32(String),
    /// Issue id or span id.
    Hex16(String),
}

fn kind_of(id: &str) -> Option<IdKind> {
    let hex: String = id.trim().chars().filter(|c| *c != '-').collect::<String>().to_ascii_lowercase();
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        32 => Some(IdKind::Hex32(hex)),
        16 => Some(IdKind::Hex16(hex)),
        _ => None,
    }
}

pub(crate) async fn run(r: &Run, id: &str, window: &WindowArgs, wait: &WaitArgs) -> Result<()> {
    let kind = kind_of(id).ok_or_else(|| {
        usage(format!(
            "{:?} is not an id mallard can look up: use a trace or event id (32 hex) or an issue or span id (16 hex)",
            clean(id)
        ))
    })?;
    // The window is resolved on every try: with --wait it moves along.
    let fetch = || async {
        let w = window.resolve(WINDOW)?;
        let matches = match &kind {
            IdKind::Hex32(id) => hex32(r, id, w.from, w.to).await?,
            IdKind::Hex16(id) => hex16(r, id, w.from, w.to).await?,
        };
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

async fn hex32(
    r: &Run,
    id: &str,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<Match>> {
    let c = &r.client;
    let (trace, event) = tokio::join!(c.trace_overview_in(id, from, to), c.sentry.find_event(id));
    let mut out = Vec::new();
    let mut errors = Vec::new();
    match trace {
        Ok(t) if !(t.spans.is_empty() && t.errors.is_empty() && t.logs.is_empty()) => out.push(Match::Trace {
            trace_id: t.trace_id.to_string(),
            spans: t.spans.len(),
            errors: t.errors.len(),
            logs: t.logs.len(),
            next: format!("mallard trace {}", t.trace_id),
        }),
        Ok(_) => {}
        Err(e) => errors.push(crate::described(e, c.base_url())),
    }
    match event {
        Ok(e) => out.push(Match::Event {
            next: format!(
                "mallard events show {} --issue {} -p {}",
                e.summary.event_id, e.summary.issue_id, e.summary.project_id
            ),
            event_id: e.summary.event_id,
            issue_id: e.summary.issue_id,
            project_id: e.summary.project_id,
            title: e.summary.title,
            trace_id: e.summary.trace_id,
        }),
        Err(e) if e.is_not_found() => {}
        Err(e) => errors.push(crate::described(mallard_client::Error::Sentry(e), c.base_url())),
    }
    // Report a failure only when it may have hidden the answer.
    match errors.into_iter().next() {
        Some(e) if out.is_empty() => Err(e),
        _ => Ok(out),
    }
}

/// An issue id (in every project, or the one of `-p`) and a span id, at
/// the same time.
async fn hex16(
    r: &Run,
    id: &str,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<Match>> {
    let q = mallard_client::mallard_otel_client::api::TraceGetQuery { from: Some(from), to: Some(to), since: None };
    let (issues, span) = tokio::join!(issues(r, id), r.client.otel.span(id, &q));
    let mut out = Vec::new();
    let mut errors = Vec::new();
    match issues {
        Ok(m) => out.extend(m),
        Err(e) => errors.push(e),
    }
    match span {
        Ok(s) => out.push(Match::Span {
            next: format!("mallard trace {}", s.trace_id),
            span_id: s.span_id,
            trace_id: s.trace_id,
            service: s.service_name,
            name: s.name,
        }),
        Err(e) if e.is_not_found() => {}
        Err(e) => errors.push(crate::described(mallard_client::Error::Otel(e), r.client.base_url())),
    }
    // Report a failure only when it may have hidden the answer.
    match errors.into_iter().next() {
        Some(e) if out.is_empty() => Err(e),
        _ => Ok(out),
    }
}

async fn issues(r: &Run, id: &str) -> Result<Vec<Match>> {
    let c = &r.client.sentry;
    let err = |e| crate::described(mallard_client::Error::Sentry(e), r.client.base_url());
    let projects = match &r.project {
        Some(p) => vec![c.project(p).await.map_err(err)?],
        None => c.projects().await.map_err(err)?,
    };
    let lookups = projects.iter().map(|p| async move {
        let q = mallard_client::mallard_sentry_client::types::WindowQuery::default();
        (p, c.issue(&p.slug, id, &q).await)
    });
    let mut out = Vec::new();
    for (p, res) in futures::future::join_all(lookups).await {
        match res {
            Ok(d) => out.push(Match::Issue {
                next: format!("mallard issues show {} -p {}", d.summary.id, p.slug),
                issue_id: d.summary.id,
                project: p.slug.clone(),
                title: d.summary.title,
                status: d.summary.status.to_string(),
                count: d.summary.count,
            }),
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(err(e)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_by_length() {
        let t = "0af7651916cd43dd8448eb211c80319c";
        assert_eq!(kind_of(t), Some(IdKind::Hex32(t.into())));
        assert_eq!(kind_of("0AF76519-16CD-43DD-8448-EB211C80319C"), Some(IdKind::Hex32(t.into())));
        assert_eq!(kind_of("85fede14250ec6be"), Some(IdKind::Hex16("85fede14250ec6be".into())));
        assert_eq!(kind_of("req-7f3a"), None);
        assert_eq!(kind_of("85fede14250ec6bz"), None);
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
