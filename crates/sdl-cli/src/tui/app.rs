//! TUI state and input handling. No I/O here: key presses and API responses
//! go in, data requests ([`Req`]) come out. `mod.rs` runs the requests on
//! tokio tasks and feeds the responses back, so the UI never blocks.

use crate::cli::Hints;
use crate::event_view::{self as ev, Breadcrumb, Exception, Frame};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;
use sdl_client::types::*;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const STATUSES: [&str; 4] = ["unresolved", "resolved", "ignored", "all"];
pub const SORTS: [(&str, &str); 4] =
    [("last_seen", "Last seen"), ("first_seen", "First seen"), ("count", "Events"), ("users", "Users")];
/// Window and the stats bucket size that gives ~50 bars.
pub const WINDOWS: [(&str, &str); 5] = [("1h", "1m"), ("24h", "30m"), ("7d", "3h"), ("14d", "6h"), ("30d", "12h")];
pub const REFRESH_EVERY: Duration = Duration::from_secs(5);
pub const INFO_EVERY: Duration = Duration::from_secs(2);
const TOAST_FOR: Duration = Duration::from_secs(4);
const PAGE: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuesKey {
    pub project: String,
    pub status: usize,
    pub sort: usize,
    pub window: usize,
    pub query: String,
}

impl IssuesKey {
    pub fn query(&self) -> IssueQuery {
        IssueQuery {
            status: Some(STATUSES[self.status].into()),
            query: (!self.query.is_empty()).then(|| self.query.clone()),
            since: Some(WINDOWS[self.window].0.into()),
            sort: Some(SORTS[self.sort].0.into()),
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
    Stats { project: String, window: usize },
    Issue { project: String, id: String, window: usize },
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
        window: usize,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    Projects { selected: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Issues,
    Detail,
    /// Error events of one trace (all projects).
    Trace,
}

/// Events of one trace, from [`App::show_trace`].
#[derive(Debug, Default)]
pub struct TraceView {
    pub trace_id: String,
    pub events: Vec<EventSummary>,
    pub loaded: bool,
    pub state: TableState,
    /// View to return to on Esc.
    pub back: Option<View>,
    pub error: Option<String>,
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
    /// Opened from the trace view: Esc goes back there.
    pub from_trace: bool,
    pub events_state: TableState,
    pub event: Option<EventDetail>,
    pub exceptions: Vec<Exception>,
    pub crumbs: Vec<Breadcrumb>,
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
        self.json = json;
        self.event = Some(e);
        self.event_error = None;
        self.frame = self.default_frame();
        self.crumbs_state.select(if self.crumbs.is_empty() { None } else { Some(0) });
        if self.tab() == Tab::Json {
            self.scroll = 0;
        }
    }

    pub fn current_event_id(&self) -> Option<&str> {
        self.event_id.as_deref()
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
    pub status: usize,
    pub sort: usize,
    pub window: usize,
    pub query: String,
    /// Search text while typing after `/`.
    pub input: Option<String>,
    pub issues: Vec<IssueSummary>,
    pub issues_loaded: bool,
    pub issues_state: TableState,
    pub stats: Option<ProjectStats>,
    pub view: View,
    pub detail: Option<Detail>,
    pub trace: Option<TraceView>,
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
    pub quit: bool,
    /// Set by `on_key` when the key was not used (the host may use it).
    pub ignored: bool,
    /// Set by `on_key` when the user asked to open the shown event's trace.
    pub open_trace: Option<String>,
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
            status: 0,
            sort: 0,
            window: 1,
            query: String::new(),
            input: None,
            issues: vec![],
            issues_loaded: false,
            issues_state: TableState::default(),
            stats: None,
            view: View::Issues,
            detail: None,
            trace: None,
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
            quit: false,
            ignored: false,
            open_trace: None,
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
        match (&self.detail, &self.project) {
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
        if let (Some(d), Some(project)) = (&self.detail, &self.project) {
            if !d.events_loaded || d.events_error.is_some() {
                r.push(Req::Events { project: project.clone(), issue: d.id.clone(), before: None });
            }
            if let Some(event_id) = d.event_id.clone().filter(|_| d.event.is_none() || d.event_error.is_some()) {
                r.push(Req::Event { project: project.clone(), issue: d.id.clone(), event_id });
            }
        }
        if let Some(t) = self.trace.as_ref().filter(|t| self.view == View::Trace && (!t.loaded || t.error.is_some())) {
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

    /// Handle a key press. Sets `ignored` when the key was not used and
    /// `quit` / `open_trace` when the user asked for that.
    pub fn on_key(&mut self, k: KeyEvent) -> Vec<Req> {
        let reqs = self.key(k);
        self.track(reqs)
    }

    fn key(&mut self, k: KeyEvent) -> Vec<Req> {
        self.ignored = false;
        if matches!(k.code, KeyCode::F(1) | KeyCode::F(2)) {
            // Reserved for an embedding host.
            self.ignored = true;
            return vec![];
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('q')) {
            self.quit = true;
            return vec![];
        }
        if k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            // Only plain keys run actions; ctrl-r must not resolve an issue.
            match self.input.as_mut() {
                Some(input) if k.modifiers == KeyModifiers::CONTROL && k.code == KeyCode::Char('u') => input.clear(),
                _ => self.ignored = true,
            }
            return vec![];
        }
        match self.overlay.clone() {
            Overlay::Help => {
                self.overlay = Overlay::None;
                return vec![];
            }
            Overlay::Projects { selected } => return self.on_key_projects(k, selected),
            Overlay::None => {}
        }
        if self.input.is_some() {
            return self.on_key_search(k);
        }
        match k.code {
            KeyCode::Char('?') => {
                self.overlay = Overlay::Help;
                return vec![];
            }
            KeyCode::Char('R') => return self.refresh_all(),
            _ => {}
        }
        match self.view {
            View::Issues => self.on_key_issues(k),
            View::Detail => self.on_key_detail(k),
            View::Trace => self.on_key_trace(k),
        }
    }

    /// Show the error events of a trace (searches all projects).
    pub fn show_trace(&mut self, trace_id: &str) -> Vec<Req> {
        let back = match self.view {
            View::Trace => self.trace.as_ref().and_then(|t| t.back),
            v => Some(v),
        };
        self.trace = Some(TraceView { trace_id: trace_id.to_string(), back, ..Default::default() });
        self.view = View::Trace;
        self.overlay = Overlay::None;
        self.input = None;
        self.track(vec![Req::Trace { trace_id: trace_id.to_string() }])
    }

    fn on_key_trace(&mut self, k: KeyEvent) -> Vec<Req> {
        let Some(t) = self.trace.as_mut() else {
            self.view = View::Issues;
            return vec![];
        };
        if Self::move_selection(&mut t.state, t.events.len(), k.code) {
            return vec![];
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => {
                let back = t.back.unwrap_or(View::Issues);
                self.view = if back == View::Detail && self.detail.is_none() { View::Issues } else { back };
                self.trace = None;
            }
            KeyCode::Enter => {
                if let Some(e) = t.state.selected().and_then(|i| t.events.get(i)).cloned() {
                    return self.open_event(&e);
                }
            }
            _ => self.ignored = true,
        }
        vec![]
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
            self.project = Some(slug.clone());
            self.issues.clear();
            self.issues_loaded = false;
            self.stats = None;
            reqs.extend(self.list_requests());
        }
        let project = self.project.clone().unwrap_or(slug);
        self.detail = Some(Detail {
            id: e.issue_id.clone(),
            event_id: Some(e.event_id.clone()),
            from_trace: true,
            tab: Some(Tab::Stack),
            ..Default::default()
        });
        self.view = View::Detail;
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
                        self.project = Some(slug);
                        self.issues.clear();
                        self.issues_loaded = false;
                        self.stats = None;
                        self.detail = None;
                        self.view = View::Issues;
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

    fn on_key_issues(&mut self, k: KeyEvent) -> Vec<Req> {
        if Self::move_selection(&mut self.issues_state, self.issues.len(), k.code) {
            return vec![];
        }
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                return self.reload_list();
            }
            KeyCode::Enter => return self.open_selected(),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.status = (self.status + 1) % STATUSES.len();
                return self.reload_list();
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.status = (self.status + STATUSES.len() - 1) % STATUSES.len();
                return self.reload_list();
            }
            KeyCode::Char(c @ '1'..='4') => {
                self.status = c as usize - '1' as usize;
                return self.reload_list();
            }
            KeyCode::Char('/') => self.input = Some(self.query.clone()),
            KeyCode::Char('s') => {
                self.sort = (self.sort + 1) % SORTS.len();
                return self.reload_list();
            }
            KeyCode::Char('t') => {
                self.window = (self.window + 1) % WINDOWS.len();
                return self.reload_list();
            }
            KeyCode::Char('T') => {
                self.window = (self.window + WINDOWS.len() - 1) % WINDOWS.len();
                return self.reload_list();
            }
            KeyCode::Char('p') => {
                let selected =
                    self.current_project().and_then(|c| self.projects.iter().position(|p| p.id == c.id)).unwrap_or(0);
                self.overlay = Overlay::Projects { selected };
                return vec![Req::Projects];
            }
            KeyCode::Char('r') => return self.set_selected_status(IssueStatus::Resolved),
            KeyCode::Char('i') => return self.set_selected_status(IssueStatus::Ignored),
            KeyCode::Char('u') => return self.set_selected_status(IssueStatus::Unresolved),
            _ => self.ignored = true,
        }
        vec![]
    }

    fn open_selected(&mut self) -> Vec<Req> {
        let (Some(issue), Some(project)) = (self.selected_issue().cloned(), self.project.clone()) else {
            return vec![];
        };
        self.detail = Some(Detail { id: issue.id.clone(), summary: Some(issue.clone()), ..Default::default() });
        self.view = View::Detail;
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
        if let Some(d) = self.detail.as_mut().filter(|d| d.id == id) {
            if let Some(s) = d.summary.as_mut() {
                s.status = status;
            }
            if let Some(dd) = d.detail.as_mut() {
                dd.summary.status = status;
            }
        }
        vec![Req::SetStatus { project, id, status }]
    }

    fn on_key_detail(&mut self, k: KeyEvent) -> Vec<Req> {
        let Some(d) = self.detail.as_mut() else {
            self.view = View::Issues;
            return vec![];
        };
        let tab = d.tab();
        let id = d.id.clone();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => {
                let from_trace = d.from_trace && self.trace.is_some();
                self.detail = None;
                self.view = if from_trace { View::Trace } else { View::Issues };
                return self.list_requests();
            }
            KeyCode::Char('t') => match d.trace_id() {
                Some(t) => self.open_trace = Some(t.to_string()),
                None => self.toast("this event has no trace id", false),
            },
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                d.tab = Some(Tab::ALL[(tab.index() + 1) % Tab::ALL.len()])
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                d.tab = Some(Tab::ALL[(tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()])
            }
            KeyCode::Char(c @ '1'..='5') => d.tab = Some(Tab::ALL[c as usize - '1' as usize]),
            KeyCode::Char('[') => return self.step_event(1),
            KeyCode::Char(']') => return self.step_event(-1),
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
            KeyCode::Char('r') => return self.set_status(id, IssueStatus::Resolved),
            KeyCode::Char('i') => return self.set_status(id, IssueStatus::Ignored),
            KeyCode::Char('u') => return self.set_status(id, IssueStatus::Unresolved),
            KeyCode::Enter if tab == Tab::Events => {
                if let Some(i) = d.events_state.selected() {
                    d.tab = Some(Tab::Stack);
                    return self.show_event(i);
                }
            }
            code if is_nav(code) => return self.move_in_tab(code),
            _ => self.ignored = true,
        }
        vec![]
    }

    fn move_in_tab(&mut self, code: KeyCode) -> Vec<Req> {
        let project = self.project.clone();
        let events_busy = self.in_flight(Kind::Events);
        let Some(d) = self.detail.as_mut() else {
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
                    && let (Some(cursor), Some(project)) = (d.next_cursor.clone(), project)
                {
                    return vec![Req::Events { project, issue: d.id.clone(), before: Some(cursor) }];
                }
            }
        }
        vec![]
    }

    /// `delta` 1 = older, -1 = newer.
    fn step_event(&mut self, delta: isize) -> Vec<Req> {
        let project = self.project.clone();
        let events_busy = self.in_flight(Kind::Events);
        let Some(d) = self.detail.as_mut() else {
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
            if let (Some(cursor), Some(project)) = (d.next_cursor.clone(), project) {
                d.want_older = true;
                return vec![Req::Events { project, issue: d.id.clone(), before: Some(cursor) }];
            }
            self.toast("already at the oldest event", false);
            return vec![];
        }
        self.show_event(target)
    }

    fn show_event(&mut self, idx: usize) -> Vec<Req> {
        let Some(project) = self.project.clone() else {
            return vec![];
        };
        let Some(d) = self.detail.as_mut() else {
            return vec![];
        };
        let Some(e) = d.events.get(idx) else {
            return vec![];
        };
        d.event_idx = idx;
        d.event_id = Some(e.event_id.clone());
        d.events_state.select(Some(idx));
        vec![Req::Event { project, issue: d.id.clone(), event_id: e.event_id.clone() }]
    }

    /// Remember a network failure (shown as "offline").
    fn note(&mut self, e: &FetchError) {
        if e.offline {
            self.offline = Some(e.message.clone());
        }
    }

    pub fn on_response(&mut self, resp: Resp) -> Vec<Req> {
        use crate::fmt::{clean, sanitize};
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
        match resp {
            Resp::Panicked { msg, .. } => {
                let msg = clean(&msg);
                self.toast(format!("internal error: {msg}"), true);
                self.fatal = Some(msg);
            }
            Resp::Info(r) => match r {
                Ok(i) => {
                    self.info = Some(sanitize(i, false));
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
                    self.projects = sanitize(ps, false);
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
                        self.issues = sanitize(list, false).issues;
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
                        self.stats = Some(sanitize(s, false));
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
                if let Some(d) = self.detail.as_mut().filter(|d| d.id == id) {
                    match result {
                        Ok(detail) => {
                            d.detail = Some(sanitize(detail, false));
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
                    self.detail.as_mut().filter(|d| d.id == issue && (cursor.is_none() || cursor == d.next_cursor))
                else {
                    return vec![];
                };
                let append = cursor.is_some();
                match result {
                    Ok(list) => {
                        let list = sanitize(list, false);
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
                            if d.events.len() > before {
                                out.extend(self.show_event(before));
                                return self.track(out);
                            }
                        } else if let Some(id) = d.event_id.clone() {
                            if let Some(i) = d.events.iter().position(|e| e.event_id == id) {
                                d.event_idx = i;
                                d.events_state.select(Some(i));
                            }
                        } else if let (Some(e), Some(project)) = (d.events.first(), project) {
                            d.event_idx = 0;
                            d.event_id = Some(e.event_id.clone());
                            out.push(Req::Event { project, issue: d.id.clone(), event_id: e.event_id.clone() });
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
                if let Some(d) = self.detail.as_mut().filter(|d| d.current_event_id() == Some(event_id.as_str())) {
                    match result {
                        Ok(e) => {
                            // JSON view from the raw payload (serde escapes control
                            // characters in strings), cleaned per line for C1 codes.
                            let json = serde_json::to_string_pretty(&e.payload)
                                .unwrap_or_default()
                                .lines()
                                .map(clean)
                                .collect();
                            d.set_event(sanitize(e, false), json);
                        }
                        Err(e) => d.event_error = Some(e.message),
                    }
                }
            }
            Resp::Trace { trace_id, result } => {
                if let Err(e) = &result {
                    self.note(e);
                }
                if let Some(t) = self.trace.as_mut().filter(|t| t.trace_id == trace_id) {
                    match result {
                        Ok(list) => {
                            t.events = sanitize(list, false).events;
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
                        let d = sanitize(d, false);
                        let verb = match status {
                            IssueStatus::Resolved => "resolved",
                            IssueStatus::Ignored => "ignored",
                            IssueStatus::Unresolved => "unresolved",
                        };
                        self.toast(format!("✓ {verb}: {}", crate::fmt::truncate(&d.summary.title, 60)), false);
                        if let Some(det) = self.detail.as_mut().filter(|x| x.id == id) {
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
        app.on_key(key('s'));
        app.on_response(Resp::Issues(old, Ok(IssueList { issues: vec![], window_start: now(), window_end: now() })));
        assert_eq!(app.issues.len(), 3, "stale response must not replace the list");
    }

    #[test]
    fn filters_and_search_produce_requests() {
        let mut app = loaded_app();
        let r = app.on_key(KeyEvent::from(KeyCode::Tab));
        assert!(matches!(&r[1], Req::Issues(k) if k.status == 1));
        app.on_key(key('/'));
        for c in "boom".chars() {
            assert!(app.on_key(key(c)).is_empty());
        }
        let r = app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.query, "boom");
        assert!(matches!(&r[1], Req::Issues(k) if k.query == "boom" && k.query().query.as_deref() == Some("boom")));
        let r = app.on_key(key('t'));
        assert!(matches!(&r[0], Req::Stats { window: 2, .. }));
    }

    #[test]
    fn enter_opens_detail_and_loads_latest_event() {
        let mut app = loaded_app();
        app.on_key(key('j'));
        let r = app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.view, View::Detail);
        assert!(matches!(&r[0], Req::Issue { id, .. } if id == "b2"));
        let r = app.on_response(Resp::Events { issue: "b2".into(), before: None, result: Ok(event_list()) });
        assert!(matches!(&r[0], Req::Event { event_id, .. } if event_id == "e1"));
        app.on_response(Resp::Event { event_id: "e1".into(), result: Ok(event_detail()) });
        let d = app.detail.as_ref().unwrap();
        assert_eq!(d.exceptions.len(), 2);
        // Cursor starts on the first in-app frame.
        assert!(d.stack_frames()[d.frame].frame.in_app);
        // Older event.
        let r = app.on_key(key('['));
        assert!(matches!(&r[0], Req::Event { event_id, .. } if event_id == "e2"));
        let r = app.on_key(key(']'));
        assert!(matches!(&r[0], Req::Event { event_id, .. } if event_id == "e1"));
        assert!(app.on_key(key(']')).is_empty());
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.view, View::Issues);
    }

    #[test]
    fn resolve_is_optimistic_and_refreshes() {
        let mut app = loaded_app();
        let r = app.on_key(key('r'));
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
        app.on_key(KeyEvent::from(KeyCode::F(1)));
        assert!(app.ignored);
        app.on_key(key('z'));
        assert!(app.ignored);
        app.on_key(key('j'));
        assert!(!app.ignored);
    }

    #[test]
    fn trace_view_opens_event_detail() {
        let mut app = detail_app();
        app.on_key(key('t'));
        let t = app.open_trace.take().unwrap();
        assert_eq!(t, "b7644661850f40e69380d8c9ea253e84");
        let r = app.show_trace(&t);
        assert_eq!(r, vec![Req::Trace { trace_id: t.clone() }]);
        assert_eq!(app.view, View::Trace);
        app.on_response(Resp::Trace { trace_id: t.clone(), result: Ok(event_list()) });
        app.on_key(key('j'));
        let r = app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(r.iter().any(|r| matches!(r, Req::Event { event_id, .. } if event_id == "e2")));
        assert_eq!(app.view, View::Detail);
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.view, View::Trace);
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.view, View::Issues);
    }

    #[test]
    fn ctrl_and_alt_keys_do_not_run_actions() {
        let mut app = loaded_app();
        let r = app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(r.is_empty() && app.ignored);
        assert_eq!(app.issues[0].status, IssueStatus::Unresolved);
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT));
        assert!(app.ignored && app.sort == 0);
        app.on_key(key('/'));
        app.on_key(key('x'));
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.input.as_deref(), Some(""));
        app.on_key(KeyEvent::from(KeyCode::Esc));
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
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
        let d = app.detail.as_mut().unwrap();
        d.next_cursor = Some("cursor-1".into());
        d.event_idx = 1;
        let r = app.on_key(key('['));
        assert!(matches!(&r[..], [Req::Events { before: Some(c), .. }] if c == "cursor-1"));
        assert!(app.on_key(key('[')).is_empty(), "second press must not load the same page again");
    }

    #[test]
    fn view_errors_are_kept_per_view() {
        let mut app = detail_app();
        app.on_response(Resp::Event { event_id: "e1".into(), result: Err(FetchError::api("boom")) });
        assert_eq!(app.detail.as_ref().unwrap().event_error.as_deref(), Some("boom"));
        app.on_response(Resp::Info(Ok(info())));
        assert_eq!(app.detail.as_ref().unwrap().event_error.as_deref(), Some("boom"));
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
        let first = app.on_key(key('s'));
        app.on_key(key('s'));
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
        let r = app.on_key(key('R'));
        assert!(r.iter().any(|r| matches!(r, Req::Event { event_id, .. } if event_id == "e1")), "{r:?}");
        assert!(!r.iter().any(|r| matches!(r, Req::Events { .. })), "events are loaded: {r:?}");

        let mut app = loaded_app();
        app.on_key(KeyEvent::from(KeyCode::Enter));
        app.on_response(Resp::Events { issue: "a1".into(), before: None, result: Err(FetchError::api("boom")) });
        let r = app.on_key(key('R'));
        assert!(r.iter().any(|r| matches!(r, Req::Events { issue, before: None, .. } if issue == "a1")), "{r:?}");

        let mut app = loaded_app();
        let t = "b7644661850f40e69380d8c9ea253e84";
        app.show_trace(t);
        app.on_response(Resp::Trace { trace_id: t.into(), result: Err(FetchError::offline("down")) });
        let r = app.on_key(key('R'));
        assert!(r.contains(&Req::Trace { trace_id: t.into() }), "{r:?}");
    }

    #[test]
    fn an_older_page_of_an_earlier_detail_is_dropped() {
        let mut app = detail_app();
        app.detail.as_mut().unwrap().next_cursor = Some("c1".into());
        app.on_key(key('4'));
        // Moving to the last event loads the next (older) page.
        let r = app.on_key(key('G'));
        assert!(matches!(&r[..], [Req::Events { before: Some(c), .. }] if c == "c1"), "{r:?}");
        // Esc and open the same issue again while the older page is on its way.
        app.on_key(KeyEvent::from(KeyCode::Esc));
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let older = EventList { events: event_list().events[1..].to_vec(), next_cursor: None };
        app.on_response(Resp::Events { issue: "b2".into(), before: Some("c1".into()), result: Ok(older) });
        let d = app.detail.as_ref().unwrap();
        assert!(!d.events_loaded && d.events.is_empty(), "stale page appended: {:?}", d.events);
        // The first page arrives and is shown from its first event.
        let r = app.on_response(Resp::Events { issue: "b2".into(), before: None, result: Ok(event_list()) });
        assert!(matches!(&r[..], [Req::Event { event_id, .. }] if event_id == "e1"), "{r:?}");
        let d = app.detail.as_ref().unwrap();
        assert_eq!((d.events.len(), d.event_idx), (2, 0));
    }

    #[test]
    fn show_trace_closes_the_search_box() {
        let mut app = loaded_app();
        app.on_key(key('/'));
        app.on_key(key('x'));
        let t = "b7644661850f40e69380d8c9ea253e84";
        app.show_trace(t);
        assert_eq!(app.input, None);
        app.on_response(Resp::Trace { trace_id: t.into(), result: Ok(event_list()) });
        let r = app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.view, View::Detail, "Enter opens the event, not a search");
        assert!(!r.iter().any(|r| matches!(r, Req::Issues(_))), "{r:?}");
    }

    #[test]
    fn project_switcher() {
        let mut app = loaded_app();
        app.on_key(key('p'));
        assert_eq!(app.overlay, Overlay::Projects { selected: 0 });
        app.on_key(key('j'));
        let r = app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.project.as_deref(), Some("api"));
        assert!(matches!(&r[1], Req::Issues(k) if k.project == "api"));
    }
}
