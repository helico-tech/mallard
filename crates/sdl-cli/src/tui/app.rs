//! TUI state and input handling. No I/O here: key presses and API responses
//! go in, data requests ([`Req`]) come out. `mod.rs` runs the requests on
//! tokio tasks and feeds the responses back, so the UI never blocks.

use super::Outcome;
use crate::cli::Hints;
use crate::event_view::{self as ev, Breadcrumb, Exception, Frame};
use crate::{SortBy, StatusFilter};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;
use sdl_client::types::*;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Time window of the issue list, the stats and the issue detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Hour,
    Day,
    Week,
    TwoWeeks,
    Month,
}

impl Window {
    pub const ALL: [Window; 5] = [Window::Hour, Window::Day, Window::Week, Window::TwoWeeks, Window::Month];

    /// The API's `since` value.
    pub fn since(self) -> &'static str {
        match self {
            Window::Hour => "1h",
            Window::Day => "24h",
            Window::Week => "7d",
            Window::TwoWeeks => "14d",
            Window::Month => "30d",
        }
    }

    /// Stats bucket size that gives ~50 bars.
    pub fn interval(self) -> &'static str {
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
pub const REFRESH_EVERY: Duration = Duration::from_secs(5);
pub const INFO_EVERY: Duration = Duration::from_secs(2);
const TOAST_FOR: Duration = Duration::from_secs(4);
const PAGE: usize = 10;
/// Screens kept behind the front one (oldest dropped first).
const MAX_BACK: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuesKey {
    pub project: String,
    pub status: StatusFilter,
    pub sort: SortBy,
    pub window: Window,
    pub query: String,
}

impl IssuesKey {
    pub fn query(&self) -> IssueQuery {
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
pub enum Req {
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
pub enum Kind {
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
    pub fn kind(&self) -> Kind {
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
pub struct FetchError {
    pub message: String,
    pub offline: bool,
}

#[cfg(test)]
impl FetchError {
    pub fn api(message: impl Into<String>) -> Self {
        FetchError { message: message.into(), offline: false }
    }
    pub fn offline(message: impl Into<String>) -> Self {
        FetchError { message: message.into(), offline: true }
    }
}

pub type Res<T> = Result<T, FetchError>;

fn is_nav(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Char('j' | 'k' | 'g' | 'G' | ' ')
    )
}

#[derive(Debug)]
pub enum Resp {
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
fn cleaned<T: serde::Serialize + serde::de::DeserializeOwned>(v: T) -> Res<T> {
    crate::fmt::sanitize(v, false)
        .map_err(|e| FetchError { message: format!("cannot show the response: {e}"), offline: false })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Stack,
    Breadcrumbs,
    Events,
    Json,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Overview, Tab::Stack, Tab::Breadcrumbs, Tab::Events, Tab::Json];
    pub fn title(self) -> &'static str {
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
pub enum Overlay {
    None,
    Help,
    Projects { selected: usize },
}

/// The screen in front, with its data. Esc returns to the screen behind
/// it (`App::back`); the issue list is always at the bottom.
#[derive(Debug)]
pub enum Screen {
    Issues,
    Detail(Box<Detail>),
    /// Error events of one trace (all projects).
    Trace(TraceView),
}

/// Which kind of screen is in front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Issues,
    Detail,
    Trace,
}

/// Events of one trace, from [`App::show_trace`].
#[derive(Debug, Default)]
pub struct TraceView {
    pub trace_id: String,
    pub events: Vec<EventSummary>,
    pub loaded: bool,
    pub state: TableState,
    pub error: Option<String>,
}

/// What a key press did: requests to send, and what the host should do.
type Step = (Vec<Req>, Outcome);

fn done(reqs: Vec<Req>) -> Step {
    (reqs, Outcome::Consumed)
}

fn ignored() -> Step {
    (vec![], Outcome::Ignored)
}

/// One frame of the stack trace view, flattened over the exception chain.
pub struct StackFrame<'a> {
    pub exception: usize,
    pub frame: &'a Frame,
}

#[derive(Debug, Default)]
pub struct Detail {
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
    pub fn tab(&self) -> Tab {
        self.tab.unwrap_or(Tab::Overview)
    }

    pub fn title(&self) -> &str {
        self.detail
            .as_ref()
            .map(|d| d.summary.title.as_str())
            .or(self.summary.as_ref().map(|s| s.title.as_str()))
            .unwrap_or("")
    }

    pub fn current_summary(&self) -> Option<&IssueSummary> {
        self.detail.as_ref().map(|d| &d.summary).or(self.summary.as_ref())
    }

    pub fn has_raw(&self) -> bool {
        self.exceptions.iter().any(|e| e.raw_frames.is_some())
    }

    pub fn stack_frames(&self) -> Vec<StackFrame<'_>> {
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
    pub fn trace_id(&self) -> Option<&str> {
        self.event.as_ref().and_then(|e| e.summary.trace_id.as_deref())
    }
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

pub struct App {
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
    pub fn new(server_url: &str, project: Option<String>) -> Self {
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
            hints: Hints::SDL,
        }
    }

    /// Requests to send at startup.
    pub fn start(&mut self) -> Vec<Req> {
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

    pub fn in_flight(&self, kind: Kind) -> bool {
        self.pending.contains_key(&kind)
    }

    pub fn loading(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn view(&self) -> View {
        match self.screen {
            Screen::Issues => View::Issues,
            Screen::Detail(_) => View::Detail,
            Screen::Trace(_) => View::Trace,
        }
    }

    /// The issue detail, when it is in front.
    pub fn detail(&self) -> Option<&Detail> {
        match &self.screen {
            Screen::Detail(d) => Some(d),
            _ => None,
        }
    }

    pub fn detail_mut(&mut self) -> Option<&mut Detail> {
        match &mut self.screen {
            Screen::Detail(d) => Some(d),
            _ => None,
        }
    }

    /// The trace view, when it is in front.
    pub fn trace(&self) -> Option<&TraceView> {
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

    pub fn current_project(&self) -> Option<&Project> {
        let p = self.project.as_deref()?;
        self.projects.iter().find(|x| x.slug == p || x.id.to_string() == p)
    }

    pub fn issues_key(&self) -> Option<IssuesKey> {
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

    pub fn selected_issue(&self) -> Option<&IssueSummary> {
        self.issues_state.selected().and_then(|i| self.issues.get(i))
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast { text: text.into(), error, at: Instant::now() });
    }

    /// Periodic tick: spinner, clock, toast expiry, auto refresh.
    pub fn on_tick(&mut self, now: Instant) -> Vec<Req> {
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

    /// Handle a key press: the requests it starts (already tracked) and
    /// what the host should do.
    pub fn on_key(&mut self, k: KeyEvent) -> Step {
        let (reqs, outcome) = self.key(k);
        (self.track(reqs), outcome)
    }

    fn key(&mut self, k: KeyEvent) -> Step {
        if matches!(k.code, KeyCode::F(1) | KeyCode::F(2)) {
            // Reserved for an embedding host.
            return ignored();
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('q')) {
            return (vec![], Outcome::Quit);
        }
        // AltGr arrives as ctrl+alt on Windows: let it type `@`, `{`, `\` in the search box.
        let altgr = k.modifiers == KeyModifiers::CONTROL | KeyModifiers::ALT && matches!(k.code, KeyCode::Char(_));
        if k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) && !(altgr && self.input.is_some()) {
            // Only plain keys run actions; ctrl-r must not resolve an issue.
            return match self.input.as_mut() {
                Some(input) if k.modifiers == KeyModifiers::CONTROL && k.code == KeyCode::Char('u') => {
                    input.clear();
                    done(vec![])
                }
                _ => ignored(),
            };
        }
        match self.overlay {
            Overlay::Help => {
                self.overlay = Overlay::None;
                return done(vec![]);
            }
            Overlay::Projects { selected } => return done(self.on_key_projects(k, selected)),
            Overlay::None => {}
        }
        if self.input.is_some() {
            return done(self.on_key_search(k));
        }
        match k.code {
            KeyCode::Char('?') => {
                self.overlay = Overlay::Help;
                return done(vec![]);
            }
            KeyCode::Char('R') => return done(self.refresh_all()),
            _ => {}
        }
        match self.view() {
            View::Issues => self.on_key_issues(k),
            View::Detail => self.on_key_detail(k),
            View::Trace => self.on_key_trace(k),
        }
    }

    /// Show the error events of a trace (searches all projects). Replaces a
    /// trace view in front; Esc returns to the screen before it.
    pub fn show_trace(&mut self, trace_id: &str) -> Vec<Req> {
        let t = Screen::Trace(TraceView { trace_id: trace_id.to_string(), ..Default::default() });
        if self.view() == View::Trace {
            self.screen = t;
        } else {
            self.push(t);
        }
        self.overlay = Overlay::None;
        self.input = None;
        self.track(vec![Req::Trace { trace_id: trace_id.to_string() }])
    }

    fn on_key_trace(&mut self, k: KeyEvent) -> Step {
        let Screen::Trace(t) = &mut self.screen else {
            return ignored();
        };
        if Self::move_selection(&mut t.state, t.events.len(), k.code) {
            return done(vec![]);
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => self.pop(),
            KeyCode::Enter => {
                if let Some(e) = t.state.selected().and_then(|i| t.events.get(i)).cloned() {
                    return done(self.open_event(&e));
                }
            }
            _ => return ignored(),
        }
        done(vec![])
    }

    /// Open the issue detail of `e` (switching project if needed) showing `e`.
    fn open_event(&mut self, e: &EventSummary) -> Vec<Req> {
        let mut reqs = vec![];
        let slug = self
            .projects
            .iter()
            .find(|p| p.id == e.project_id)
            .map(|p| p.slug.clone())
            .unwrap_or_else(|| e.project_id.to_string());
        if self.project.as_deref() != Some(slug.as_str()) && self.project.as_deref() != Some(&e.project_id.to_string())
        {
            self.switch_project(slug.clone());
            reqs.extend(self.list_requests());
        }
        let project = self.project.clone().unwrap_or(slug);
        self.push(Screen::Detail(Box::new(Detail {
            id: e.issue_id.clone(),
            event_id: Some(e.event_id.clone()),
            tab: Some(Tab::Stack),
            ..Default::default()
        })));
        reqs.extend([
            Req::Issue { project: project.clone(), id: e.issue_id.clone(), window: self.window },
            Req::Events { project: project.clone(), issue: e.issue_id.clone(), before: None },
            Req::Event { project, issue: e.issue_id.clone(), event_id: e.event_id.clone() },
        ]);
        reqs
    }

    fn on_key_projects(&mut self, k: KeyEvent, selected: usize) -> Vec<Req> {
        let n = self.projects.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('p') => self.overlay = Overlay::None,
            KeyCode::Down | KeyCode::Char('j') if n > 0 => {
                self.overlay = Overlay::Projects { selected: (selected + 1).min(n - 1) }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = Overlay::Projects { selected: selected.saturating_sub(1) }
            }
            KeyCode::Enter => {
                self.overlay = Overlay::None;
                if let Some(p) = self.projects.get(selected) {
                    let slug = p.slug.clone();
                    if self.project.as_deref() != Some(slug.as_str()) {
                        self.toast(format!("switched to {}", p.name), false);
                        self.switch_project(slug);
                        self.screen = Screen::Issues;
                        self.back.clear();
                        return self.reload_list();
                    }
                }
            }
            _ => {}
        }
        vec![]
    }

    fn on_key_search(&mut self, k: KeyEvent) -> Vec<Req> {
        let Some(input) = self.input.as_mut() else {
            return vec![];
        };
        match k.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Enter => {
                self.query = self.input.take().unwrap_or_default().trim().to_string();
                return self.reload_list();
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => input.push(c),
            _ => {}
        }
        vec![]
    }

    fn move_selection(state: &mut TableState, len: usize, code: KeyCode) -> bool {
        if len == 0 {
            state.select(None);
            return false;
        }
        let cur = state.selected().unwrap_or(0).min(len - 1);
        let next = match code {
            KeyCode::Down | KeyCode::Char('j') => (cur + 1).min(len - 1),
            KeyCode::Up | KeyCode::Char('k') => cur.saturating_sub(1),
            KeyCode::PageDown => (cur + PAGE).min(len - 1),
            KeyCode::PageUp => cur.saturating_sub(PAGE),
            KeyCode::Char('g') | KeyCode::Home => 0,
            KeyCode::Char('G') | KeyCode::End => len - 1,
            _ => return false,
        };
        state.select(Some(next));
        true
    }

    fn on_key_issues(&mut self, k: KeyEvent) -> Step {
        if Self::move_selection(&mut self.issues_state, self.issues.len(), k.code) {
            return done(vec![]);
        }
        done(match k.code {
            KeyCode::Char('q') => return (vec![], Outcome::Quit),
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.reload_list()
            }
            KeyCode::Enter => self.open_selected(),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.status = cycle(&StatusFilter::ALL, self.status, 1);
                self.reload_list()
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.status = cycle(&StatusFilter::ALL, self.status, -1);
                self.reload_list()
            }
            KeyCode::Char(c @ '1'..='4') => {
                self.status = StatusFilter::ALL[c as usize - '1' as usize];
                self.reload_list()
            }
            KeyCode::Char('/') => {
                self.input = Some(self.query.clone());
                vec![]
            }
            KeyCode::Char('s') => {
                self.sort = cycle(&SortBy::ALL, self.sort, 1);
                self.reload_list()
            }
            KeyCode::Char('t') => {
                self.window = cycle(&Window::ALL, self.window, 1);
                self.reload_list()
            }
            KeyCode::Char('T') => {
                self.window = cycle(&Window::ALL, self.window, -1);
                self.reload_list()
            }
            KeyCode::Char('p') => {
                let selected =
                    self.current_project().and_then(|c| self.projects.iter().position(|p| p.id == c.id)).unwrap_or(0);
                self.overlay = Overlay::Projects { selected };
                vec![Req::Projects]
            }
            KeyCode::Char('r') => self.set_selected_status(IssueStatus::Resolved),
            KeyCode::Char('i') => self.set_selected_status(IssueStatus::Ignored),
            KeyCode::Char('u') => self.set_selected_status(IssueStatus::Unresolved),
            _ => return ignored(),
        })
    }

    fn open_selected(&mut self) -> Vec<Req> {
        let (Some(issue), Some(project)) = (self.selected_issue().cloned(), self.project.clone()) else {
            return vec![];
        };
        self.push(Screen::Detail(Box::new(Detail {
            id: issue.id.clone(),
            summary: Some(issue.clone()),
            ..Default::default()
        })));
        vec![
            Req::Issue { project: project.clone(), id: issue.id.clone(), window: self.window },
            Req::Events { project, issue: issue.id, before: None },
        ]
    }

    fn set_selected_status(&mut self, status: IssueStatus) -> Vec<Req> {
        let Some(id) = self.selected_issue().map(|i| i.id.clone()) else {
            return vec![];
        };
        self.set_status(id, status)
    }

    fn set_status(&mut self, id: String, status: IssueStatus) -> Vec<Req> {
        let Some(project) = self.project.clone() else {
            return vec![];
        };
        // Optimistic: show the new status right away.
        for i in self.issues.iter_mut().filter(|i| i.id == id) {
            i.status = status;
        }
        for d in self.details_mut().filter(|d| d.id == id) {
            if let Some(s) = d.summary.as_mut() {
                s.status = status;
            }
            if let Some(dd) = d.detail.as_mut() {
                dd.summary.status = status;
            }
        }
        vec![Req::SetStatus { project, id, status }]
    }

    fn on_key_detail(&mut self, k: KeyEvent) -> Step {
        let Screen::Detail(d) = &mut self.screen else {
            return ignored();
        };
        let tab = d.tab();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => {
                self.pop();
                return done(self.list_requests());
            }
            KeyCode::Char('t') => match d.trace_id() {
                Some(t) => return (vec![], Outcome::OpenTrace(t.to_string())),
                None => self.toast("this event has no trace id", false),
            },
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                d.tab = Some(Tab::ALL[(tab.index() + 1) % Tab::ALL.len()])
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                d.tab = Some(Tab::ALL[(tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()])
            }
            KeyCode::Char(c @ '1'..='5') => d.tab = Some(Tab::ALL[c as usize - '1' as usize]),
            KeyCode::Char('[') => return done(self.step_event(1)),
            KeyCode::Char(']') => return done(self.step_event(-1)),
            KeyCode::Char('a') => {
                d.in_app_only = !d.in_app_only;
                d.frame = d.default_frame();
            }
            KeyCode::Char('w') => {
                if d.has_raw() {
                    d.raw = !d.raw;
                    d.frame = d.default_frame();
                } else {
                    self.toast("this event has no raw (unsymbolicated) frames", false);
                }
            }
            KeyCode::Char(c @ ('r' | 'i' | 'u')) => {
                let status = match c {
                    'r' => IssueStatus::Resolved,
                    'i' => IssueStatus::Ignored,
                    _ => IssueStatus::Unresolved,
                };
                let id = d.id.clone();
                return done(self.set_status(id, status));
            }
            KeyCode::Enter if tab == Tab::Events => {
                if let (Some(i), Some(project)) = (d.events_state.selected(), self.project.clone()) {
                    d.tab = Some(Tab::Stack);
                    return done(d.show(i, project).into_iter().collect());
                }
            }
            code if is_nav(code) => return done(self.move_in_tab(code)),
            _ => return ignored(),
        }
        done(vec![])
    }

    fn move_in_tab(&mut self, code: KeyCode) -> Vec<Req> {
        let project = self.project.clone();
        let events_busy = self.in_flight(Kind::Events);
        let Some(d) = self.detail_mut() else {
            return vec![];
        };
        match d.tab() {
            Tab::Overview | Tab::Json => {
                let max = d.max_scroll;
                d.scroll = match code {
                    KeyCode::Down | KeyCode::Char('j') => (d.scroll + 1).min(max),
                    KeyCode::Up | KeyCode::Char('k') => d.scroll.saturating_sub(1),
                    KeyCode::PageDown | KeyCode::Char(' ') => (d.scroll + PAGE * 2).min(max),
                    KeyCode::PageUp => d.scroll.saturating_sub(PAGE * 2),
                    KeyCode::Char('g') | KeyCode::Home => 0,
                    KeyCode::Char('G') | KeyCode::End => max,
                    _ => d.scroll,
                };
            }
            Tab::Stack => {
                let n = d.stack_frames().len();
                if n > 0 {
                    d.frame = match code {
                        KeyCode::Down | KeyCode::Char('j') => (d.frame + 1).min(n - 1),
                        KeyCode::Up | KeyCode::Char('k') => d.frame.saturating_sub(1),
                        KeyCode::PageDown => (d.frame + PAGE).min(n - 1),
                        KeyCode::PageUp => d.frame.saturating_sub(PAGE),
                        KeyCode::Char('g') | KeyCode::Home => 0,
                        KeyCode::Char('G') | KeyCode::End => n - 1,
                        _ => d.frame,
                    };
                }
            }
            Tab::Breadcrumbs => {
                Self::move_selection(&mut d.crumbs_state, d.crumbs.len(), code);
            }
            Tab::Events => {
                let n = d.events.len();
                Self::move_selection(&mut d.events_state, n, code);
                // Reached the end: fetch the next (older) page.
                if n > 0
                    && d.events_state.selected() == Some(n - 1)
                    && !events_busy
                    && let Some(project) = project
                {
                    return d.older_page(project).into_iter().collect();
                }
            }
        }
        vec![]
    }

    /// `delta` 1 = older, -1 = newer.
    fn step_event(&mut self, delta: isize) -> Vec<Req> {
        let Some(project) = self.project.clone() else {
            return vec![];
        };
        let events_busy = self.in_flight(Kind::Events);
        let Some(d) = self.detail_mut() else {
            return vec![];
        };
        let target = d.event_idx as isize + delta;
        if target < 0 {
            self.toast("already at the newest event", false);
            return vec![];
        }
        let target = target as usize;
        if target >= d.events.len() {
            if d.want_older || events_busy {
                // The older page is already on its way.
                return vec![];
            }
            if let Some(r) = d.older_page(project) {
                d.want_older = true;
                return vec![r];
            }
            self.toast("already at the oldest event", false);
            return vec![];
        }
        d.show(target, project).into_iter().collect()
    }

    /// Remember a network failure (shown as "offline").
    fn note(&mut self, e: &FetchError) {
        if e.offline {
            self.offline = Some(e.message.clone());
        }
    }

    pub fn on_response(&mut self, resp: Resp) -> Vec<Req> {
        use crate::fmt::clean;
        if let Some(n) = self.pending.get_mut(&resp.kind()) {
            *n -= 1;
            if *n == 0 {
                self.pending.remove(&resp.kind());
            }
        }
        let ok = !matches!(
            &resp,
            Resp::Info(Err(_))
                | Resp::Projects(Err(_))
                | Resp::Issues(_, Err(_))
                | Resp::Stats { result: Err(_), .. }
                | Resp::Issue { result: Err(_), .. }
                | Resp::Events { result: Err(_), .. }
                | Resp::Event { result: Err(_), .. }
                | Resp::SetStatus { result: Err(_), .. }
                | Resp::Trace { result: Err(_), .. }
                | Resp::Panicked { .. }
        );
        if ok {
            // Any answer from the server means it is reachable again.
            self.offline = None;
        }
        let mut out = vec![];
        match resp.sanitized() {
            Resp::Panicked { msg, .. } => {
                let msg = clean(&msg);
                self.toast(format!("internal error: {msg}"), true);
                self.fatal = Some(msg);
            }
            Resp::Info(r) => match r {
                Ok(i) => {
                    self.info = Some(i);
                    self.global_error = None;
                }
                Err(e) => {
                    self.note(&e);
                    if !e.offline {
                        self.global_error = Some(e.message);
                    }
                }
            },
            Resp::Projects(r) => match r {
                Ok(ps) => {
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
                    // Also when an id (`-p 1`) became its slug: loads made with
                    // the old string are dropped as stale, so start new ones.
                    if self.project.is_some() && self.project != before {
                        out.extend(self.reload_list());
                    }
                }
                Err(e) => {
                    self.note(&e);
                    if !e.offline {
                        self.global_error = Some(e.message);
                    }
                }
            },
            Resp::Issues(key, r) => {
                if Some(&key) != self.issues_key().as_ref() {
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
                            .or_else(|| {
                                self.issues_state.selected().map(|s| s.min(self.issues.len().saturating_sub(1)))
                            })
                            .or(Some(0));
                        self.issues_state.select(if self.issues.is_empty() { None } else { idx });
                    }
                    Err(e) => {
                        self.note(&e);
                        self.issues_error = Some(e.message);
                    }
                }
            }
            Resp::Stats { project, window, result } => {
                if self.project.as_deref() != Some(project.as_str()) || window != self.window {
                    return vec![];
                }
                match result {
                    Ok(s) => {
                        self.stats = Some(s);
                        self.stats_error = None;
                    }
                    Err(e) => {
                        self.note(&e);
                        self.stats_error = Some(e.message);
                    }
                }
            }
            Resp::Issue { id, result } => {
                if let Err(e) = &result {
                    self.note(e);
                }
                if let Some(d) = self.details_mut().find(|d| d.id == id) {
                    match result {
                        Ok(detail) => {
                            d.detail = Some(detail);
                            d.error = None;
                        }
                        Err(e) => d.error = Some(e.message),
                    }
                }
            }
            Resp::Events { issue, before: cursor, result } => {
                if let Err(e) = &result {
                    self.note(e);
                }
                let project = self.project.clone();
                // A first page always fits; an older page only when it is
                // the page this detail asked for (not one of an earlier
                // detail of the same issue).
                let Some(d) =
                    self.details_mut().find(|d| d.id == issue && (cursor.is_none() || cursor == d.next_cursor))
                else {
                    return vec![];
                };
                let append = cursor.is_some();
                match result {
                    Ok(list) => {
                        d.events_error = None;
                        let before = d.events.len();
                        if append {
                            d.events.extend(list.events);
                        } else {
                            d.events = list.events;
                        }
                        d.next_cursor = list.next_cursor;
                        d.events_loaded = true;
                        if d.events_state.selected().is_none() && !d.events.is_empty() {
                            d.events_state.select(Some(0));
                        }
                        if d.want_older {
                            d.want_older = false;
                            if let Some(project) = project.filter(|_| d.events.len() > before) {
                                out.extend(d.show(before, project));
                            }
                        } else if let Some(id) = d.event_id.clone() {
                            if let Some(i) = d.events.iter().position(|e| e.event_id == id) {
                                d.event_idx = i;
                                d.events_state.select(Some(i));
                            }
                        } else if let Some(project) = project {
                            out.extend(d.show(0, project));
                        }
                    }
                    Err(e) => {
                        d.want_older = false;
                        d.events_error = Some(e.message);
                    }
                }
            }
            Resp::Event { event_id, result } => {
                if let Err(e) = &result {
                    self.note(e);
                }
                if let Some(d) = self.details_mut().find(|d| d.event_id.as_deref() == Some(event_id.as_str())) {
                    // JSON view from the raw payload (serde escapes control
                    // characters in strings), cleaned per line for C1 codes.
                    let result = result.and_then(|e| {
                        let json =
                            serde_json::to_string_pretty(&e.payload).unwrap_or_default().lines().map(clean).collect();
                        Ok((cleaned(e)?, json))
                    });
                    match result {
                        Ok((e, json)) => d.set_event(e, json),
                        Err(e) => d.event_error = Some(e.message),
                    }
                }
            }
            Resp::Trace { trace_id, result } => {
                if let Err(e) = &result {
                    self.note(e);
                }
                let trace = self.screens_mut().find_map(|s| match s {
                    Screen::Trace(t) if t.trace_id == trace_id => Some(t),
                    _ => None,
                });
                if let Some(t) = trace {
                    match result {
                        Ok(list) => {
                            t.events = list.events;
                            t.loaded = true;
                            t.error = None;
                            t.state.select(if t.events.is_empty() { None } else { Some(0) });
                        }
                        Err(e) => t.error = Some(e.message),
                    }
                }
            }
            Resp::SetStatus { id, status, result } => {
                match result {
                    Ok(d) => {
                        let verb = status.as_str();
                        self.toast(format!("✓ {verb}: {}", crate::fmt::truncate(&d.summary.title, 60)), false);
                        if let Some(det) = self.details_mut().find(|x| x.id == id) {
                            det.detail = Some(d);
                        }
                    }
                    Err(e) => {
                        self.note(&e);
                        self.toast(format!("could not update {}: {}", clean(&id), e.message), true);
                    }
                }
                out.extend(self.list_requests());
            }
        }
        self.track(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::fixtures::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(c))
    }

    /// The requests of a key press.
    fn press(app: &mut App, k: KeyEvent) -> Vec<Req> {
        app.on_key(k).0
    }

    #[test]
    fn startup_picks_first_project_and_loads_list() {
        let mut app = App::new("http://x", None);
        let r = app.start();
        assert_eq!(r, vec![Req::Info, Req::Projects]);
        let r = app.on_response(Resp::Projects(Ok(projects())));
        assert_eq!(app.project.as_deref(), Some("web"));
        assert!(matches!(r.as_slice(), [Req::Stats { .. }, Req::Issues(_)]));
    }

    #[test]
    fn stale_issue_lists_are_dropped() {
        let mut app = loaded_app();
        let old = app.issues_key().unwrap();
        press(&mut app, key('s'));
        app.on_response(Resp::Issues(old, Ok(IssueList { issues: vec![], window_start: now(), window_end: now() })));
        assert_eq!(app.issues.len(), 3, "stale response must not replace the list");
    }

    #[test]
    fn filters_and_search_produce_requests() {
        let mut app = loaded_app();
        let r = press(&mut app, KeyEvent::from(KeyCode::Tab));
        assert!(matches!(&r[1], Req::Issues(k) if k.status == StatusFilter::Resolved));
        press(&mut app, key('/'));
        for c in "boom".chars() {
            assert!(press(&mut app, key(c)).is_empty());
        }
        let r = press(&mut app, KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.query, "boom");
        assert!(matches!(&r[1], Req::Issues(k) if k.query == "boom" && k.query().query.as_deref() == Some("boom")));
        let r = press(&mut app, key('t'));
        assert!(matches!(&r[0], Req::Stats { window: Window::Week, .. }));
        let r = press(&mut app, key('T'));
        assert!(matches!(&r[0], Req::Stats { window: Window::Day, .. }));
        press(&mut app, KeyEvent::from(KeyCode::BackTab));
        assert_eq!(app.status, StatusFilter::Unresolved);
        press(&mut app, KeyEvent::from(KeyCode::BackTab));
        assert_eq!(app.status, StatusFilter::All, "wraps around");
        press(&mut app, key('3'));
        assert_eq!(app.status, StatusFilter::Ignored);
    }

    #[test]
    fn enter_opens_detail_and_loads_latest_event() {
        let mut app = loaded_app();
        press(&mut app, key('j'));
        let r = press(&mut app, KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.view(), View::Detail);
        assert!(matches!(&r[0], Req::Issue { id, .. } if id == "b2"));
        let r = app.on_response(Resp::Events { issue: "b2".into(), before: None, result: Ok(event_list()) });
        assert!(matches!(&r[0], Req::Event { event_id, .. } if event_id == "e1"));
        app.on_response(Resp::Event { event_id: "e1".into(), result: Ok(event_detail()) });
        let d = app.detail().unwrap();
        assert_eq!(d.exceptions.len(), 2);
        // Cursor starts on the first in-app frame.
        assert!(d.stack_frames()[d.frame].frame.in_app);
        // Older event.
        let r = press(&mut app, key('['));
        assert!(matches!(&r[0], Req::Event { event_id, .. } if event_id == "e2"));
        let r = press(&mut app, key(']'));
        assert!(matches!(&r[0], Req::Event { event_id, .. } if event_id == "e1"));
        assert!(press(&mut app, key(']')).is_empty());
        press(&mut app, KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.view(), View::Issues);
    }

    #[test]
    fn resolve_is_optimistic_and_refreshes() {
        let mut app = loaded_app();
        let r = press(&mut app, key('r'));
        assert!(matches!(&r[0], Req::SetStatus { status: IssueStatus::Resolved, .. }));
        assert_eq!(app.issues[0].status, IssueStatus::Resolved);
        let mut d = issue_detail();
        d.summary.status = IssueStatus::Resolved;
        let r = app.on_response(Resp::SetStatus { id: "a1".into(), status: IssueStatus::Resolved, result: Ok(d) });
        assert!(app.toast.as_ref().unwrap().text.contains("resolved"));
        assert!(r.iter().any(|r| matches!(r, Req::Issues(_))));
    }

    #[test]
    fn unknown_and_reserved_keys_are_ignored() {
        let mut app = loaded_app();
        assert_eq!(app.on_key(KeyEvent::from(KeyCode::F(1))).1, Outcome::Ignored);
        assert_eq!(app.on_key(key('z')).1, Outcome::Ignored);
        assert_eq!(app.on_key(key('j')).1, Outcome::Consumed);
        assert_eq!(app.on_key(key('q')).1, Outcome::Quit);
        let mut app = detail_app();
        assert_eq!(app.on_key(key('z')).1, Outcome::Ignored);
        app.show_trace("t1");
        assert_eq!(app.on_key(key('z')).1, Outcome::Ignored);
    }

    #[test]
    fn trace_view_opens_event_detail() {
        let mut app = detail_app();
        let Outcome::OpenTrace(t) = app.on_key(key('t')).1 else { panic!("t opens the trace") };
        assert_eq!(t, "b7644661850f40e69380d8c9ea253e84");
        let r = app.show_trace(&t);
        assert_eq!(r, vec![Req::Trace { trace_id: t.clone() }]);
        assert_eq!(app.view(), View::Trace);
        app.on_response(Resp::Trace { trace_id: t.clone(), result: Ok(event_list()) });
        press(&mut app, key('j'));
        let r = press(&mut app, KeyEvent::from(KeyCode::Enter));
        assert!(r.iter().any(|r| matches!(r, Req::Event { event_id, .. } if event_id == "e2")));
        assert_eq!(app.view(), View::Detail);
        // Esc walks back: trace, the detail it was opened from, the list.
        press(&mut app, KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.view(), View::Trace);
        press(&mut app, KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.detail().map(|d| d.id.as_str()), Some("b2"));
        assert_eq!(app.detail().unwrap().event_id.as_deref(), Some("e1"));
        press(&mut app, KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.view(), View::Issues);
    }

    #[test]
    fn loads_reach_screens_behind_the_front_one() {
        let mut app = loaded_app();
        app.issues_state.select(Some(1));
        press(&mut app, KeyEvent::from(KeyCode::Enter));
        let t = "b7644661850f40e69380d8c9ea253e84";
        app.show_trace(t);
        // The detail's loads arrive while the trace view is in front.
        app.on_response(Resp::Issue { id: "b2".into(), result: Ok(issue_detail()) });
        app.on_response(Resp::Events { issue: "b2".into(), before: None, result: Ok(event_list()) });
        app.on_response(Resp::Event { event_id: "e1".into(), result: Ok(event_detail()) });
        press(&mut app, KeyEvent::from(KeyCode::Esc));
        let d = app.detail().expect("back on the detail");
        assert!(d.detail.is_some() && d.events_loaded && d.event.is_some());
    }

    #[test]
    fn the_back_stack_is_bounded() {
        let mut app = detail_app();
        for i in 0..3 * MAX_BACK {
            app.show_trace(&format!("t{i}"));
            app.on_response(Resp::Trace { trace_id: format!("t{i}"), result: Ok(event_list()) });
            press(&mut app, KeyEvent::from(KeyCode::Enter));
        }
        assert_eq!(app.back.len(), MAX_BACK);
    }

    #[test]
    fn ctrl_and_alt_keys_do_not_run_actions() {
        let mut app = loaded_app();
        let r = app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(r, (vec![], Outcome::Ignored));
        assert_eq!(app.issues[0].status, IssueStatus::Unresolved);
        assert_eq!(app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT)).1, Outcome::Ignored);
        assert_eq!(app.sort, SortBy::LastSeen);
        press(&mut app, key('/'));
        press(&mut app, key('x'));
        press(&mut app, KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.input.as_deref(), Some(""));
        // AltGr (ctrl+alt on Windows) types in the search box, and only there.
        press(&mut app, KeyEvent::new(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT));
        assert_eq!(app.input.as_deref(), Some("@"));
        press(&mut app, KeyEvent::from(KeyCode::Esc));
        let r = app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL | KeyModifiers::ALT));
        assert_eq!(r, (vec![], Outcome::Ignored));
        assert_eq!(app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)).1, Outcome::Quit);
    }

    #[test]
    fn project_id_is_replaced_by_slug_and_list_reloaded() {
        let mut app = App::new("http://x", Some("1".into()));
        let r = app.start();
        let old = match &r[3] {
            Req::Issues(k) => k.clone(),
            other => panic!("{other:?}"),
        };
        let r = app.on_response(Resp::Projects(Ok(projects())));
        assert_eq!(app.project.as_deref(), Some("web"));
        assert!(r.iter().any(|r| matches!(r, Req::Issues(k) if k.project == "web")), "{r:?}");
        // The response to the old request is stale and dropped.
        app.on_response(Resp::Issues(old, Ok(IssueList { issues: issues(), window_start: now(), window_end: now() })));
        assert!(!app.issues_loaded);
        // With the slug already given, no extra reload.
        let mut app = App::new("http://x", Some("web".into()));
        app.start();
        assert!(app.on_response(Resp::Projects(Ok(projects()))).is_empty());
    }

    #[test]
    fn older_page_is_requested_once() {
        let mut app = detail_app();
        let d = app.detail_mut().unwrap();
        d.next_cursor = Some("cursor-1".into());
        d.event_idx = 1;
        let r = press(&mut app, key('['));
        assert!(matches!(&r[..], [Req::Events { before: Some(c), .. }] if c == "cursor-1"));
        assert!(press(&mut app, key('[')).is_empty(), "second press must not load the same page again");
    }

    #[test]
    fn view_errors_are_kept_per_view() {
        let mut app = detail_app();
        app.on_response(Resp::Event { event_id: "e1".into(), result: Err(FetchError::api("boom")) });
        assert_eq!(app.detail().unwrap().event_error.as_deref(), Some("boom"));
        app.on_response(Resp::Info(Ok(info())));
        assert_eq!(app.detail().unwrap().event_error.as_deref(), Some("boom"));
        assert!(app.offline.is_none());
    }

    #[test]
    fn a_panicked_load_is_no_longer_pending() {
        let mut app = App::new("http://x", None);
        app.start();
        assert!(app.in_flight(Kind::Info));
        app.on_response(Resp::Panicked { kind: Kind::Info, msg: "boom".into() });
        assert!(!app.in_flight(Kind::Info), "the kind that panicked must be cleared");
        app.on_response(Resp::Projects(Ok(vec![])));
        assert!(!app.loading());
        // The periodic refresh may load info again.
        app.last_info = Instant::now() - INFO_EVERY;
        assert!(app.on_tick(Instant::now()).contains(&Req::Info));
    }

    #[test]
    fn a_stale_response_does_not_end_a_newer_load() {
        let mut app = loaded_app();
        assert!(!app.loading());
        let first = press(&mut app, key('s'));
        press(&mut app, key('s'));
        let Req::Issues(old) = &first[1] else { panic!("{first:?}") };
        let list = IssueList { issues: issues(), window_start: now(), window_end: now() };
        app.on_response(Resp::Issues(old.clone(), Ok(list.clone())));
        app.on_response(Resp::Stats { project: "web".into(), window: app.window, result: Ok(stats()) });
        assert!(app.in_flight(Kind::Issues) && app.in_flight(Kind::Stats), "the newer loads are still running");
        assert!(app.loading());
        app.on_response(Resp::Issues(app.issues_key().unwrap(), Ok(list)));
        app.on_response(Resp::Stats { project: "web".into(), window: app.window, result: Ok(stats()) });
        assert!(!app.loading());
    }

    #[test]
    fn r_retries_failed_trace_events_and_event_loads() {
        let mut app = detail_app();
        app.on_response(Resp::Event { event_id: "e1".into(), result: Err(FetchError::api("boom")) });
        let r = press(&mut app, key('R'));
        assert!(r.iter().any(|r| matches!(r, Req::Event { event_id, .. } if event_id == "e1")), "{r:?}");
        assert!(!r.iter().any(|r| matches!(r, Req::Events { .. })), "events are loaded: {r:?}");

        let mut app = loaded_app();
        press(&mut app, KeyEvent::from(KeyCode::Enter));
        app.on_response(Resp::Events { issue: "a1".into(), before: None, result: Err(FetchError::api("boom")) });
        let r = press(&mut app, key('R'));
        assert!(r.iter().any(|r| matches!(r, Req::Events { issue, before: None, .. } if issue == "a1")), "{r:?}");

        let mut app = loaded_app();
        let t = "b7644661850f40e69380d8c9ea253e84";
        app.show_trace(t);
        app.on_response(Resp::Trace { trace_id: t.into(), result: Err(FetchError::offline("down")) });
        let r = press(&mut app, key('R'));
        assert!(r.contains(&Req::Trace { trace_id: t.into() }), "{r:?}");
    }

    #[test]
    fn an_older_page_of_an_earlier_detail_is_dropped() {
        let mut app = detail_app();
        app.detail_mut().unwrap().next_cursor = Some("c1".into());
        press(&mut app, key('4'));
        // Moving to the last event loads the next (older) page.
        let r = press(&mut app, key('G'));
        assert!(matches!(&r[..], [Req::Events { before: Some(c), .. }] if c == "c1"), "{r:?}");
        // Esc and open the same issue again while the older page is on its way.
        press(&mut app, KeyEvent::from(KeyCode::Esc));
        press(&mut app, KeyEvent::from(KeyCode::Enter));
        let older = EventList { events: event_list().events[1..].to_vec(), next_cursor: None };
        app.on_response(Resp::Events { issue: "b2".into(), before: Some("c1".into()), result: Ok(older) });
        let d = app.detail().unwrap();
        assert!(!d.events_loaded && d.events.is_empty(), "stale page appended: {:?}", d.events);
        // The first page arrives and is shown from its first event.
        let r = app.on_response(Resp::Events { issue: "b2".into(), before: None, result: Ok(event_list()) });
        assert!(matches!(&r[..], [Req::Event { event_id, .. }] if event_id == "e1"), "{r:?}");
        let d = app.detail().unwrap();
        assert_eq!((d.events.len(), d.event_idx), (2, 0));
    }

    #[test]
    fn show_trace_closes_the_search_box() {
        let mut app = loaded_app();
        press(&mut app, key('/'));
        press(&mut app, key('x'));
        let t = "b7644661850f40e69380d8c9ea253e84";
        app.show_trace(t);
        assert_eq!(app.input, None);
        app.on_response(Resp::Trace { trace_id: t.into(), result: Ok(event_list()) });
        let r = press(&mut app, KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.view(), View::Detail, "Enter opens the event, not a search");
        assert!(!r.iter().any(|r| matches!(r, Req::Issues(_))), "{r:?}");
    }

    /// `sanitize` fails closed, so every API type the TUI shows must
    /// survive its JSON round trip.
    #[test]
    fn every_api_type_survives_sanitize() {
        cleaned(info()).unwrap();
        cleaned(projects()).unwrap();
        cleaned(IssueList { issues: issues(), window_start: now(), window_end: now() }).unwrap();
        cleaned(stats()).unwrap();
        cleaned(issue_detail()).unwrap();
        cleaned(event_list()).unwrap();
        cleaned(event_detail()).unwrap();
    }

    #[test]
    fn project_switcher() {
        let mut app = loaded_app();
        press(&mut app, key('p'));
        assert_eq!(app.overlay, Overlay::Projects { selected: 0 });
        press(&mut app, key('j'));
        let r = press(&mut app, KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.project.as_deref(), Some("api"));
        assert!(matches!(&r[1], Req::Issues(k) if k.project == "api"));
    }
}
