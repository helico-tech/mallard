//! Finished loads: apply them to the screens they belong to.

use super::*;
use crate::fmt::clean;

impl Resp {
    /// Every loaded value cleaned for display (see [`crate::fmt::sanitize`]).
    /// The event keeps its raw payload: its JSON view is built from it first.
    fn sanitized(self) -> Self {
        match self {
            Resp::Info(r) => Resp::Info(r.and_then(cleaned)),
            Resp::Projects(r) => Resp::Projects(r.and_then(cleaned)),
            Resp::Issues(k, r) => Resp::Issues(k, r.and_then(cleaned)),
            Resp::Stats { project, window, result } => {
                Resp::Stats { project, window, result: result.and_then(cleaned) }
            }
            Resp::Issue { id, result } => Resp::Issue { id, result: result.and_then(cleaned) },
            Resp::Events { issue, before, result } => Resp::Events { issue, before, result: result.and_then(cleaned) },
            Resp::SetStatus { id, status, result } => Resp::SetStatus { id, status, result: result.and_then(cleaned) },
            Resp::Trace { trace_id, result } => Resp::Trace { trace_id, result: result.and_then(cleaned) },
            r @ (Resp::Event { .. } | Resp::Panicked { .. }) => r,
        }
    }
}

/// A value cleaned for display; one that can't be cleaned is an error and
/// is never shown raw.
pub(super) fn cleaned<T: serde::Serialize + serde::de::DeserializeOwned>(v: T) -> Res<T> {
    crate::fmt::sanitize(v, false)
        .map_err(|e| FetchError { message: format!("cannot show the response: {e}"), offline: false })
}

impl Resp {
    /// The failed load, if this is one.
    fn error(&self) -> Option<&FetchError> {
        match self {
            Resp::Info(r) => r.as_ref().err(),
            Resp::Projects(r) => r.as_ref().err(),
            Resp::Issues(_, r) => r.as_ref().err(),
            Resp::Stats { result, .. } => result.as_ref().err(),
            Resp::Issue { result, .. } => result.as_ref().err(),
            Resp::Events { result, .. } => result.as_ref().err(),
            Resp::Event { result, .. } => result.as_ref().err(),
            Resp::SetStatus { result, .. } => result.as_ref().err(),
            Resp::Trace { result, .. } => result.as_ref().err(),
            Resp::Panicked { .. } => None,
        }
    }
}

impl App {
    /// Apply a finished load; returns the follow-up loads (already tracked).
    pub(crate) fn on_response(&mut self, resp: Resp) -> Vec<Req> {
        let kind = resp.kind();
        if let Some(n) = self.pending.get_mut(&kind) {
            *n -= 1;
            if *n == 0 {
                self.pending.remove(&kind);
            }
        }
        match resp.error() {
            Some(e) if e.offline => self.offline = Some(e.message.clone()),
            Some(_) => {}
            // Any answer from the server means it is reachable again.
            None if !matches!(resp, Resp::Panicked { .. }) => self.offline = None,
            None => {}
        }
        let out = match resp.sanitized() {
            Resp::Panicked { msg, .. } => self.on_panicked(&msg),
            Resp::Info(r) => self.on_info(r),
            Resp::Projects(r) => self.on_projects(r),
            Resp::Issues(key, r) => self.on_issues(&key, r),
            Resp::Stats { project, window, result } => self.on_stats(&project, window, result),
            Resp::Issue { id, result } => self.on_issue(&id, result),
            Resp::Events { issue, before, result } => self.on_events(&issue, before, result),
            Resp::Event { event_id, result } => self.on_event(&event_id, result),
            Resp::Trace { trace_id, result } => self.on_trace(&trace_id, result),
            Resp::SetStatus { id, status, result } => self.on_set_status(&id, status, result),
        };
        self.track(out)
    }

    fn on_panicked(&mut self, msg: &str) -> Vec<Req> {
        let msg = clean(msg);
        self.toast(format!("internal error: {msg}"), true);
        self.fatal = Some(msg);
        vec![]
    }

    fn on_info(&mut self, r: Res<ServerInfo>) -> Vec<Req> {
        match r {
            Ok(i) => {
                self.info = Some(i);
                self.global_error = None;
            }
            Err(e) if !e.offline => self.global_error = Some(e.message),
            Err(_) => {}
        }
        vec![]
    }

    fn on_projects(&mut self, r: Res<Vec<Project>>) -> Vec<Req> {
        let ps = match r {
            Ok(ps) => ps,
            Err(e) => {
                if !e.offline {
                    self.global_error = Some(e.message);
                }
                return vec![];
            }
        };
        self.projects = ps;
        self.projects_loaded = true;
        self.global_error = None;
        let before = self.project.clone();
        match self.current_project().map(|p| p.slug.clone()) {
            Some(slug) => self.project = Some(slug),
            None if before.is_some() => {
                let p = clean(before.as_deref().unwrap_or_default());
                self.issues_error = Some(format!("project {p:?} not found (press p to pick one)"));
            }
            None => self.project = self.projects.first().map(|p| p.slug.clone()),
        }
        // Also when an id (`-p 1`) became its slug: loads made with the old
        // string are dropped as stale, so start new ones.
        if self.project.is_some() && self.project != before { self.reload_list() } else { vec![] }
    }

    fn on_issues(&mut self, key: &IssuesKey, r: Res<IssueList>) -> Vec<Req> {
        if Some(key) != self.issues_key().as_ref() {
            return vec![];
        }
        match r {
            Ok(list) => {
                let keep = self.selected_issue().map(|i| i.id.clone());
                self.issues = list.issues;
                self.issues_loaded = true;
                self.issues_error = None;
                let idx = keep
                    .and_then(|id| self.issues.iter().position(|i| i.id == id))
                    .or_else(|| self.issues_state.selected().map(|s| s.min(self.issues.len().saturating_sub(1))))
                    .or(Some(0));
                self.issues_state.select(if self.issues.is_empty() { None } else { idx });
            }
            Err(e) => self.issues_error = Some(e.message),
        }
        vec![]
    }

    fn on_stats(&mut self, project: &str, window: Window, r: Res<ProjectStats>) -> Vec<Req> {
        if self.project.as_deref() != Some(project) || window != self.window {
            return vec![];
        }
        match r {
            Ok(s) => {
                self.stats = Some(s);
                self.stats_error = None;
            }
            Err(e) => self.stats_error = Some(e.message),
        }
        vec![]
    }

    fn on_issue(&mut self, id: &str, r: Res<IssueDetail>) -> Vec<Req> {
        if let Some(d) = self.details_mut().find(|d| d.id == id) {
            match r {
                Ok(detail) => {
                    d.detail = Some(detail);
                    d.error = None;
                }
                Err(e) => d.error = Some(e.message),
            }
        }
        vec![]
    }

    /// A page of the event list. `cursor` is the request's: `None` for the
    /// first page.
    fn on_events(&mut self, issue: &str, cursor: Option<String>, r: Res<EventList>) -> Vec<Req> {
        let project = self.project.clone();
        // A first page always fits; an older page only when it is the page
        // this detail asked for (not one of an earlier detail of the same issue).
        let Some(d) = self.details_mut().find(|d| d.id == issue && (cursor.is_none() || cursor == d.next_cursor))
        else {
            return vec![];
        };
        let list = match r {
            Ok(list) => list,
            Err(e) => {
                d.want_older = false;
                d.events_error = Some(e.message);
                return vec![];
            }
        };
        d.events_error = None;
        let before = d.events.len();
        if cursor.is_some() {
            d.events.extend(list.events);
        } else {
            d.events = list.events;
        }
        d.next_cursor = list.next_cursor;
        d.events_loaded = true;
        if d.events_state.selected().is_none() && !d.events.is_empty() {
            d.events_state.select(Some(0));
        }
        let Some(project) = project else {
            return vec![];
        };
        if d.want_older {
            // `[` past the last event: show the first one of the new page.
            d.want_older = false;
            return if d.events.len() > before { d.show(before, project).into_iter().collect() } else { vec![] };
        }
        match d.event_id.clone() {
            Some(id) => {
                if let Some(i) = d.events.iter().position(|e| e.event_id == id) {
                    d.event_idx = i;
                    d.events_state.select(Some(i));
                }
                vec![]
            }
            None => d.show(0, project).into_iter().collect(),
        }
    }

    fn on_event(&mut self, event_id: &str, r: Res<EventDetail>) -> Vec<Req> {
        if let Some(d) = self.details_mut().find(|d| d.event_id.as_deref() == Some(event_id)) {
            // JSON view from the raw payload (serde escapes control
            // characters in strings), cleaned per line for C1 codes.
            let r = r.and_then(|e| {
                let json = serde_json::to_string_pretty(&e.payload).unwrap_or_default().lines().map(clean).collect();
                Ok((cleaned(e)?, json))
            });
            match r {
                Ok((e, json)) => d.set_event(e, json),
                Err(e) => d.event_error = Some(e.message),
            }
        }
        vec![]
    }

    fn on_trace(&mut self, trace_id: &str, r: Res<EventList>) -> Vec<Req> {
        let trace = self.screens_mut().find_map(|s| match s {
            Screen::Trace(t) if t.trace_id == trace_id => Some(t),
            _ => None,
        });
        if let Some(t) = trace {
            match r {
                Ok(list) => {
                    t.events = list.events;
                    t.loaded = true;
                    t.error = None;
                    t.state.select(if t.events.is_empty() { None } else { Some(0) });
                }
                Err(e) => t.error = Some(e.message),
            }
        }
        vec![]
    }

    fn on_set_status(&mut self, id: &str, status: IssueStatus, r: Res<IssueDetail>) -> Vec<Req> {
        match r {
            Ok(d) => {
                let verb = status.as_str();
                self.toast(format!("✓ {verb}: {}", crate::fmt::truncate(&d.summary.title, 60)), false);
                if let Some(det) = self.details_mut().find(|x| x.id == id) {
                    det.detail = Some(d);
                }
            }
            Err(e) => self.toast(format!("could not update {}: {}", clean(id), e.message), true),
        }
        self.list_requests()
    }
}
