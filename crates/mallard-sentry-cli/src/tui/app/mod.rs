//! TUI state and input handling. No I/O here: key presses and API responses
//! go in, data requests ([`Req`]) come out. `mod.rs` runs the requests on
//! tokio tasks and feeds the responses back, so the UI never blocks.

use super::Outcome;
use crate::event_view::{self as ev, Breadcrumb, Exception, Frame};
use crate::{SortBy, StatusFilter};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use mallard_sentry_client::types::*;
use mallard_term::error::Hints;
use ratatui::widgets::TableState;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Time window of the issue list, the stats and the issue detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Window {
    Hour,
    Day,
    Week,
    TwoWeeks,
    Month,
}

impl Window {
    pub(super) const ALL: [Window; 5] = [Window::Hour, Window::Day, Window::Week, Window::TwoWeeks, Window::Month];

    /// The API's `since` value.
    pub(super) fn since(self) -> &'static str {
        match self {
            Window::Hour => "1h",
            Window::Day => "24h",
            Window::Week => "7d",
            Window::TwoWeeks => "14d",
            Window::Month => "30d",
        }
    }

    /// Stats bucket size that gives ~50 bars.
    pub(super) fn interval(self) -> &'static str {
        match self {
            Window::Hour => "1m",
            Window::Day => "30m",
            Window::Week => "3h",
            Window::TwoWeeks => "6h",
            Window::Month => "12h",
        }
    }
}

/// The item `step` places after `cur` in `all`, wrapping around.
fn cycle<T: Copy + PartialEq>(all: &[T], cur: T, step: isize) -> T {
    let i = all.iter().position(|x| *x == cur).unwrap_or(0) as isize;
    all[(i + step).rem_euclid(all.len() as isize) as usize]
}
pub(super) const REFRESH_EVERY: Duration = Duration::from_secs(5);
pub(super) const INFO_EVERY: Duration = Duration::from_secs(2);
const TOAST_FOR: Duration = Duration::from_secs(4);
const PAGE: usize = 10;
/// Screens kept behind the front one (oldest dropped first).
const MAX_BACK: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IssuesKey {
    pub project: String,
    pub status: StatusFilter,
    pub sort: SortBy,
    pub window: Window,
    pub query: String,
}

impl IssuesKey {
    pub(super) fn query(&self) -> IssueQuery {
        IssueQuery {
            status: Some(self.status.as_str().into()),
            query: (!self.query.is_empty()).then(|| self.query.clone()),
            since: Some(self.window.since().into()),
            sort: Some(self.sort.as_str().into()),
            limit: Some(200),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Req {
    Info,
    Projects,
    Issues(IssuesKey),
    Stats { project: String, window: Window },
    Issue { project: String, id: String, window: Window },
    Events { project: String, issue: String, before: Option<String> },
    Event { project: String, issue: String, event_id: String },
    SetStatus { project: String, id: String, status: IssueStatus },
    Trace { trace_id: String },
}

/// What a request loads; used to count the loads in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Kind {
    Info,
    Projects,
    Issues,
    Stats,
    Issue,
    Events,
    Event,
    Status,
    Trace,
}

impl Req {
    pub(super) fn kind(&self) -> Kind {
        match self {
            Req::Info => Kind::Info,
            Req::Projects => Kind::Projects,
            Req::Issues(_) => Kind::Issues,
            Req::Stats { .. } => Kind::Stats,
            Req::Issue { .. } => Kind::Issue,
            Req::Events { .. } => Kind::Events,
            Req::Event { .. } => Kind::Event,
            Req::SetStatus { .. } => Kind::Status,
            Req::Trace { .. } => Kind::Trace,
        }
    }
}

/// A failed load: a printable one-line message, and whether it was a
/// network-level failure (server unreachable) rather than an API error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FetchError {
    pub message: String,
    pub offline: bool,
}

#[cfg(test)]
impl FetchError {
    pub(super) fn api(message: impl Into<String>) -> Self {
        FetchError { message: message.into(), offline: false }
    }
    pub(super) fn offline(message: impl Into<String>) -> Self {
        FetchError { message: message.into(), offline: true }
    }
}

pub(super) type Res<T> = Result<T, FetchError>;

#[derive(Debug)]
pub(super) enum Resp {
    Info(Res<ServerInfo>),
    Projects(Res<Vec<Project>>),
    Issues(IssuesKey, Res<IssueList>),
    Stats {
        project: String,
        window: Window,
        result: Res<ProjectStats>,
    },
    Issue {
        id: String,
        result: Res<IssueDetail>,
    },
    Events {
        issue: String,
        /// The cursor of the request: `None` is the first page.
        before: Option<String>,
        result: Res<EventList>,
    },
    Event {
        event_id: String,
        result: Res<EventDetail>,
    },
    SetStatus {
        id: String,
        status: IssueStatus,
        result: Res<IssueDetail>,
    },
    Trace {
        trace_id: String,
        result: Res<EventList>,
    },
    /// A background load of this kind panicked.
    Panicked {
        kind: Kind,
        msg: String,
    },
}

impl Resp {
    fn kind(&self) -> Kind {
        match self {
            Resp::Info(_) => Kind::Info,
            Resp::Projects(_) => Kind::Projects,
            Resp::Issues(..) => Kind::Issues,
            Resp::Stats { .. } => Kind::Stats,
            Resp::Issue { .. } => Kind::Issue,
            Resp::Events { .. } => Kind::Events,
            Resp::Event { .. } => Kind::Event,
            Resp::SetStatus { .. } => Kind::Status,
            Resp::Trace { .. } => Kind::Trace,
            Resp::Panicked { kind, .. } => *kind,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Overview,
    Stack,
    Breadcrumbs,
    Events,
    Json,
}

impl Tab {
    pub(super) const ALL: [Tab; 5] = [Tab::Overview, Tab::Stack, Tab::Breadcrumbs, Tab::Events, Tab::Json];
    pub(super) fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Stack => "Stack trace",
            Tab::Breadcrumbs => "Breadcrumbs",
            Tab::Events => "Events",
            Tab::Json => "JSON",
        }
    }
    fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Overlay {
    None,
    Help,
    Projects { selected: usize },
}

/// The screen in front, with its data. Esc returns to the screen behind
/// it (`App::back`); the issue list is always at the bottom.
#[derive(Debug)]
pub(super) enum Screen {
    Issues,
    Detail(Box<Detail>),
    /// Error events of one trace (all projects).
    Trace(TraceView),
}

/// Which kind of screen is in front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum View {
    Issues,
    Detail,
    Trace,
}

/// Events of one trace, from [`App::show_trace`].
#[derive(Debug, Default)]
pub(super) struct TraceView {
    pub trace_id: String,
    pub events: Vec<EventSummary>,
    pub loaded: bool,
    pub state: TableState,
    pub error: Option<String>,
    /// Open this event once the list has loaded ([`App::show_trace_event`]).
    pub open_event: Option<String>,
}

/// One frame of the stack trace view, flattened over the exception chain.
pub(super) struct StackFrame<'a> {
    pub exception: usize,
    pub frame: &'a Frame,
}

#[derive(Debug, Default)]
pub(super) struct Detail {
    pub id: String,
    pub summary: Option<IssueSummary>,
    pub detail: Option<IssueDetail>,
    pub events: Vec<EventSummary>,
    pub next_cursor: Option<String>,
    pub events_loaded: bool,
    /// Index of the shown event in `events`.
    pub event_idx: usize,
    /// Id of the shown (or requested) event.
    pub event_id: Option<String>,
    pub events_state: TableState,
    pub event: Option<EventDetail>,
    pub exceptions: Vec<Exception>,
    pub crumbs: Vec<Breadcrumb>,
    /// Parsed once per shown event, not per frame.
    pub contexts: Vec<(String, String)>,
    pub request: Option<ev::Request>,
    pub message: Option<String>,
    pub json: Vec<String>,
    /// Waiting for an older page to step to the next older event.
    pub want_older: bool,
    /// Last failed load of the issue itself, of the event list, of the shown event.
    pub error: Option<String>,
    pub events_error: Option<String>,
    pub event_error: Option<String>,
    pub tab: Option<Tab>,
    pub scroll: usize,
    pub max_scroll: usize,
    pub frame: usize,
    /// Scroll offset of the stack trace view (kept so the cursor is visible).
    pub stack_scroll: usize,
    pub in_app_only: bool,
    pub raw: bool,
    pub crumbs_state: TableState,
}

impl Detail {
    pub(super) fn tab(&self) -> Tab {
        self.tab.unwrap_or(Tab::Overview)
    }

    pub(super) fn title(&self) -> &str {
        self.detail
            .as_ref()
            .map(|d| d.summary.title.as_str())
            .or(self.summary.as_ref().map(|s| s.title.as_str()))
            .unwrap_or("")
    }

    pub(super) fn current_summary(&self) -> Option<&IssueSummary> {
        self.detail.as_ref().map(|d| &d.summary).or(self.summary.as_ref())
    }

    pub(super) fn has_raw(&self) -> bool {
        self.exceptions.iter().any(|e| e.raw_frames.is_some())
    }

    pub(super) fn stack_frames(&self) -> Vec<StackFrame<'_>> {
        let mut out = Vec::new();
        for (i, e) in self.exceptions.iter().enumerate() {
            for f in e.frames_newest_first(self.raw) {
                if !self.in_app_only || f.in_app {
                    out.push(StackFrame { exception: i, frame: f });
                }
            }
        }
        out
    }

    fn default_frame(&self) -> usize {
        self.stack_frames().iter().position(|f| f.frame.in_app).unwrap_or(0)
    }

    /// `e` must already be sanitized; `json` is the pretty payload, cleaned per line.
    fn set_event(&mut self, e: EventDetail, json: Vec<String>) {
        self.exceptions = ev::exceptions(&e.payload);
        self.crumbs = ev::breadcrumbs(&e.payload);
        self.contexts = ev::contexts(&e.payload);
        self.request = ev::request(&e.payload);
        self.message = ev::message(&e.payload);
        self.json = json;
        self.event = Some(e);
        self.event_error = None;
        self.frame = self.default_frame();
        self.crumbs_state.select(if self.crumbs.is_empty() { None } else { Some(0) });
        if self.tab() == Tab::Json {
            self.scroll = 0;
        }
    }

    /// Show event `idx` of the list: the request that loads it.
    fn show(&mut self, idx: usize, project: String) -> Option<Req> {
        let e = self.events.get(idx)?;
        self.event_idx = idx;
        self.event_id = Some(e.event_id.clone());
        self.events_state.select(Some(idx));
        Some(Req::Event { project, issue: self.id.clone(), event_id: e.event_id.clone() })
    }

    /// The request for the next older page of events.
    fn older_page(&self, project: String) -> Option<Req> {
        let cursor = self.next_cursor.clone()?;
        Some(Req::Events { project, issue: self.id.clone(), before: Some(cursor) })
    }

    /// Trace id of the shown event.
    pub(super) fn trace_id(&self) -> Option<&str> {
        self.event.as_ref().and_then(|e| e.summary.trace_id.as_deref())
    }
}

pub(super) struct Toast {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

pub(super) struct App {
    pub server_url: String,
    pub info: Option<ServerInfo>,
    pub projects: Vec<Project>,
    pub project: Option<String>,
    pub projects_loaded: bool,
    pub status: StatusFilter,
    pub sort: SortBy,
    pub window: Window,
    pub query: String,
    /// Search text while typing after `/`.
    pub input: Option<String>,
    pub issues: Vec<IssueSummary>,
    pub issues_loaded: bool,
    pub issues_state: TableState,
    pub stats: Option<ProjectStats>,
    pub screen: Screen,
    /// Screens behind `screen`, the last one is shown on Esc.
    pub back: Vec<Screen>,
    pub overlay: Overlay,
    pub toast: Option<Toast>,
    /// Server unreachable (network error of any load); cleared by any success.
    pub offline: Option<String>,
    /// API error of the info / projects loads (e.g. invalid token).
    pub global_error: Option<String>,
    /// Last failed load of the issue list / the stats panel.
    pub issues_error: Option<String>,
    pub stats_error: Option<String>,
    /// A background load panicked; the standalone TUI quits on this.
    pub fatal: Option<String>,
    /// Loads in flight, per kind. Every request gets exactly one response
    /// (a panic becomes `Resp::Panicked`), so the counts never leak.
    pub pending: HashMap<Kind, usize>,
    pub spinner: usize,
    pub now: DateTime<Utc>,
    pub last_refresh: Instant,
    pub last_info: Instant,
    pub started_at: Instant,
    /// The host's command name, for hints.
    pub hints: Hints,
}

impl App {
    pub(super) fn new(server_url: &str, project: Option<String>) -> Self {
        App {
            server_url: server_url.to_string(),
            info: None,
            projects: vec![],
            project,
            projects_loaded: false,
            status: StatusFilter::Unresolved,
            sort: SortBy::LastSeen,
            window: Window::Day,
            query: String::new(),
            input: None,
            issues: vec![],
            issues_loaded: false,
            issues_state: TableState::default(),
            stats: None,
            screen: Screen::Issues,
            back: vec![],
            overlay: Overlay::None,
            toast: None,
            offline: None,
            global_error: None,
            issues_error: None,
            stats_error: None,
            fatal: None,
            pending: HashMap::new(),
            spinner: 0,
            now: Utc::now(),
            last_refresh: Instant::now(),
            last_info: Instant::now(),
            started_at: Instant::now(),
            hints: crate::HINTS,
        }
    }

    /// Requests to send at startup.
    pub(super) fn start(&mut self) -> Vec<Req> {
        let mut r = vec![Req::Info, Req::Projects];
        r.extend(self.list_requests());
        self.track(r)
    }

    fn track(&mut self, reqs: Vec<Req>) -> Vec<Req> {
        for r in &reqs {
            *self.pending.entry(r.kind()).or_default() += 1;
        }
        reqs
    }

    pub(super) fn in_flight(&self, kind: Kind) -> bool {
        self.pending.contains_key(&kind)
    }

    pub(super) fn loading(&self) -> bool {
        !self.pending.is_empty()
    }

    pub(super) fn view(&self) -> View {
        match self.screen {
            Screen::Issues => View::Issues,
            Screen::Detail(_) => View::Detail,
            Screen::Trace(_) => View::Trace,
        }
    }

    /// The issue detail, when it is in front.
    pub(super) fn detail(&self) -> Option<&Detail> {
        match &self.screen {
            Screen::Detail(d) => Some(d),
            _ => None,
        }
    }

    pub(super) fn detail_mut(&mut self) -> Option<&mut Detail> {
        match &mut self.screen {
            Screen::Detail(d) => Some(d),
            _ => None,
        }
    }

    /// The trace view, when it is in front.
    pub(super) fn trace(&self) -> Option<&TraceView> {
        match &self.screen {
            Screen::Trace(t) => Some(t),
            _ => None,
        }
    }

    /// Every open screen, front first. Loads apply to the screens behind
    /// the front one too, so going back shows their data.
    fn screens_mut(&mut self) -> impl Iterator<Item = &mut Screen> {
        std::iter::once(&mut self.screen).chain(self.back.iter_mut().rev())
    }

    fn details_mut(&mut self) -> impl Iterator<Item = &mut Detail> {
        self.screens_mut().filter_map(|s| match s {
            Screen::Detail(d) => Some(&mut **d),
            _ => None,
        })
    }

    /// Bring `s` to the front; the current screen goes behind it.
    fn push(&mut self, s: Screen) {
        let old = std::mem::replace(&mut self.screen, s);
        if !matches!(old, Screen::Issues) {
            if self.back.len() == MAX_BACK {
                self.back.remove(0);
            }
            self.back.push(old);
        }
    }

    /// Esc: back to the screen behind the front one.
    fn pop(&mut self) {
        self.screen = self.back.pop().unwrap_or(Screen::Issues);
    }

    /// Another project: forget its list, stats and open screens.
    fn switch_project(&mut self, slug: String) {
        self.project = Some(slug);
        self.issues.clear();
        self.issues_loaded = false;
        self.stats = None;
    }

    pub(super) fn current_project(&self) -> Option<&Project> {
        let p = self.project.as_deref()?;
        self.projects.iter().find(|x| x.slug == p || x.id.to_string() == p)
    }

    pub(super) fn issues_key(&self) -> Option<IssuesKey> {
        Some(IssuesKey {
            project: self.project.clone()?,
            status: self.status,
            sort: self.sort,
            window: self.window,
            query: self.query.clone(),
        })
    }

    fn list_requests(&self) -> Vec<Req> {
        let Some(key) = self.issues_key() else {
            return vec![];
        };
        vec![Req::Stats { project: key.project.clone(), window: self.window }, Req::Issues(key)]
    }

    fn detail_requests(&self) -> Vec<Req> {
        match (self.detail(), &self.project) {
            (Some(d), Some(p)) => vec![Req::Issue { project: p.clone(), id: d.id.clone(), window: self.window }],
            _ => vec![],
        }
    }

    pub(super) fn selected_issue(&self) -> Option<&IssueSummary> {
        self.issues_state.selected().and_then(|i| self.issues.get(i))
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast { text: text.into(), error, at: Instant::now() });
    }

    /// Periodic tick: spinner, clock, toast expiry, auto refresh.
    pub(super) fn on_tick(&mut self, now: Instant) -> Vec<Req> {
        self.spinner = (now.duration_since(self.started_at).as_millis() / 100) as usize;
        self.now = Utc::now();
        if self.toast.as_ref().is_some_and(|t| now.duration_since(t.at) > TOAST_FOR) {
            self.toast = None;
        }
        let mut reqs = vec![];
        if now.duration_since(self.last_info) >= INFO_EVERY && !self.in_flight(Kind::Info) {
            self.last_info = now;
            reqs.push(Req::Info);
        }
        if now.duration_since(self.last_refresh) >= REFRESH_EVERY {
            self.last_refresh = now;
            if !self.projects_loaded && !self.in_flight(Kind::Projects) {
                reqs.push(Req::Projects);
            }
            reqs.extend(self.list_requests().into_iter().filter(|r| !self.in_flight(r.kind())));
            reqs.extend(self.detail_requests().into_iter().filter(|r| !self.in_flight(r.kind())));
        }
        self.track(reqs)
    }

    /// `R`: reload everything, including the loads of the open view that
    /// failed (they say "R retry").
    fn refresh_all(&mut self) -> Vec<Req> {
        self.last_refresh = Instant::now();
        let mut r = vec![Req::Info, Req::Projects];
        r.extend(self.list_requests());
        r.extend(self.detail_requests());
        if let (Some(d), Some(project)) = (self.detail(), &self.project) {
            if !d.events_loaded || d.events_error.is_some() {
                r.push(Req::Events { project: project.clone(), issue: d.id.clone(), before: None });
            }
            if let Some(event_id) = d.event_id.clone().filter(|_| d.event.is_none() || d.event_error.is_some()) {
                r.push(Req::Event { project: project.clone(), issue: d.id.clone(), event_id });
            }
        }
        if let Some(t) = self.trace().filter(|t| !t.loaded || t.error.is_some()) {
            r.push(Req::Trace { trace_id: t.trace_id.clone() });
        }
        r
    }

    /// Filters changed: reload the list from the top.
    fn reload_list(&mut self) -> Vec<Req> {
        self.issues_state.select(if self.issues.is_empty() { None } else { Some(0) });
        self.last_refresh = Instant::now();
        self.list_requests()
    }
}

mod keys;
mod responses;
#[cfg(test)]
mod tests;
