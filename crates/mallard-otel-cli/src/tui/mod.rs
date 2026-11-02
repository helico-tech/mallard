//! Terminal UI for the telemetry side.
//!
//! [`Embedded`] is the whole UI as a component: the host owns the terminal
//! (raw mode, alternate screen) and the event loop, and calls
//! [`Embedded::draw`], [`Embedded::handle_event`] and [`Embedded::tick`]
//! (~4x per second). Loading runs on the tokio runtime in the background and
//! never blocks input.
//!
//! F1 and F2 are reserved for the host and always return [`Outcome::Ignored`],
//! and so do keys with Ctrl or Alt (except Ctrl-C, which quits, or cancels
//! an open prompt), so host shortcuts keep working.
//!
//! A tokio runtime must be current when `tick` is called (loads are spawned on
//! it), and the host's event loop must not block that runtime's only thread.

mod chrome;
mod input;
mod loader;
mod query;
mod suggestions;
mod theme;
mod views;
mod widgets;

#[cfg(test)]
mod regress;
#[cfg(test)]
mod tests;

use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::{DateTime, Utc};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use mallard_otel_client::{Client, api::Stats};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    widgets::Paragraph,
};

use input::{Input, InputResult};
use loader::{Data, Kind, Loader};

/// What the host should do after an event; `x` on a trace is
/// [`Outcome::Jump`] (errors of this trace).
pub use mallard_term::tui::Outcome;
use mallard_term::{
    text::clean,
    tui::{Scope, keys, suggest::Suggest},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum View {
    Overview,
    Traces,
    Logs,
    Metrics,
    Status,
}

const VIEWS: [(View, &str); 5] = [
    (View::Overview, "Overview"),
    (View::Traces, "Traces"),
    (View::Logs, "Logs"),
    (View::Metrics, "Metrics"),
    (View::Status, "Status"),
];

/// Time windows `t` cycles through; `set_since` may add one more.
const WINDOWS: [(&str, u64); 6] =
    [("5m", 300), ("15m", 900), ("1h", 3600), ("6h", 6 * 3600), ("24h", 86_400), ("7d", 7 * 86_400)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptTarget {
    TraceFilter,
    LogFilter,
    MetricSearch,
    MetricGroupBy,
}

pub(crate) struct Prompt {
    pub target: PromptTarget,
    pub label: &'static str,
    pub help: &'static str,
    pub input: Input,
}

/// The TUI as an embeddable component.
pub struct Embedded {
    client: Client,
    loader: Loader,
    view: View,
    /// Time windows as given (sent to the server) and parsed.
    windows: Vec<(String, Duration)>,
    win: usize,
    stats: Option<Stats>,
    stats_error: Option<String>,
    stats_at: Option<Instant>,
    prev_accepted: Option<(Instant, u64)>,
    ingest_rate: Option<f64>,
    last_ok: Option<Instant>,
    overview: views::overview::State,
    traces: views::traces::State,
    logs: views::logs::State,
    metrics: views::metrics::State,
    help: bool,
    prompt: Option<Prompt>,
    flash: Option<(String, bool, Instant)>,
    spin: usize,
    errors_enabled: bool,
    /// The host's error tracker: marks traces and spans with error events.
    errors: Option<Arc<dyn crate::ErrorLookup>>,
    /// Kinds whose last load failed: only the first failure flashes.
    failing: HashSet<Kind>,
    /// In a host with one window and one app picker for its tabs: `t`, `T` and `p` are
    /// the host's (they return [`Outcome::Ignored`]).
    hosted: bool,
    /// The host's app; everything but a trace opened by id is limited to its services.
    scope: Scope,
    trace_suggest: Suggest,
    log_suggest: Suggest,
}

impl std::fmt::Debug for Embedded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Embedded")
            .field("client", &self.client)
            .field("view", &self.view)
            .field("since", &self.since())
            .finish_non_exhaustive()
    }
}

/// Pause before retrying a failed load.
pub(crate) const RETRY: Duration = Duration::from_secs(3);

pub(crate) fn due(t: Option<Instant>, every: Duration) -> bool {
    t.is_none_or(|t| t.elapsed() >= every)
}

/// Move a selection index for common navigation keys.
pub(crate) fn nav(cur: usize, len: usize, k: &KeyEvent, page: usize) -> Option<usize> {
    if len == 0 {
        return match k.code {
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Char('j' | 'k' | 'g' | 'G')
            | KeyCode::PageUp
            | KeyCode::PageDown => Some(0),
            _ => None,
        };
    }
    let last = len - 1;
    Some(match k.code {
        KeyCode::Down | KeyCode::Char('j') => (cur + 1).min(last),
        KeyCode::Up | KeyCode::Char('k') => cur.saturating_sub(1),
        KeyCode::PageDown => (cur + page.max(1)).min(last),
        KeyCode::PageUp => cur.saturating_sub(page.max(1)),
        KeyCode::Home | KeyCode::Char('g') => 0,
        KeyCode::End | KeyCode::Char('G') => last,
        _ => return None,
    })
}

impl Embedded {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            loader: Loader::default(),
            view: View::Overview,
            windows: WINDOWS.iter().map(|(s, secs)| (s.to_string(), Duration::from_secs(*secs))).collect(),
            win: 2,
            stats: None,
            stats_error: None,
            stats_at: None,
            prev_accepted: None,
            ingest_rate: None,
            last_ok: None,
            overview: Default::default(),
            traces: Default::default(),
            logs: Default::default(),
            metrics: Default::default(),
            help: false,
            prompt: None,
            flash: None,
            spin: 0,
            errors_enabled: false,
            errors: None,
            failing: HashSet::new(),
            hosted: false,
            scope: Scope::All,
            trace_suggest: Suggest::new(suggestions::TRACE_KEYS),
            log_suggest: Suggest::new(suggestions::LOG_KEYS),
        }
    }

    /// The host owns the time window and the app picker (see the field).
    pub fn set_hosted(&mut self, hosted: bool) {
        self.hosted = hosted;
    }

    /// The server's longest window, once it has said so.
    pub fn max_window_secs(&self) -> Option<u64> {
        self.stats.as_ref()?.capabilities.as_ref().map(|c| c.max_window_secs)
    }

    /// Limit the views to an app's services. A filter on another service goes, because a
    /// filter must not disagree with the picker (ADR 0028).
    pub fn set_scope(&mut self, scope: &Scope) {
        if self.scope == *scope {
            return;
        }
        self.scope = scope.clone();
        self.traces.pages = 0;
        if !self.traces.filter.service.as_ref().is_none_or(|s| self.in_scope(s)) {
            self.traces.filter.service = None;
        }
        if !self.logs.filter.service.as_ref().is_none_or(|s| self.in_scope(s)) {
            self.logs.filter.service = None;
            self.logs.query = views::logs::without_service(&self.logs.query);
        }
        if !self.overview.ops.as_ref().is_none_or(|(s, _)| self.in_scope(s)) {
            self.overview.ops = None;
        }
        self.overview.services.clear();
        self.overview.span_series = None;
        self.overview.log_series = None;
        self.traces.list.clear();
        self.traces.sel.select(None);
        self.metrics.list.clear();
        self.invalidate();
    }

    fn in_scope(&self, service: &str) -> bool {
        match &self.scope {
            Scope::All => true,
            Scope::App { services, .. } => services.iter().any(|s| s == service),
        }
    }

    /// What to send as `service`: a filter's value (always inside the scope), or the scope.
    pub(crate) fn scoped(&self, filter: Option<&str>) -> Vec<String> {
        match (filter, &self.scope) {
            (Some(s), _) => vec![s.to_string()],
            (None, Scope::All) => vec![],
            (None, Scope::App { services, .. }) => services.clone(),
        }
    }

    /// An app without telemetry in the window: nothing to ask the server for. Sending no
    /// `service` would show every service instead. A trace opened by id is not scoped.
    fn no_telemetry(&self) -> Option<&str> {
        let Scope::App { slug, services, .. } = &self.scope else { return None };
        let by_id = match self.view {
            View::Status => true,
            View::Traces => self.traces.detail.is_some(),
            View::Logs => self.logs.filter.trace.is_some(),
            _ => false,
        };
        (services.is_empty() && !by_id).then_some(slug.as_str())
    }

    /// Show the `x errors` key in hints and help. The key always returns
    /// [`Outcome::Jump`]; enable this when the host can act on it (e.g. an
    /// error tracker is connected). Off by default.
    pub fn set_errors_enabled(&mut self, enabled: bool) {
        self.errors_enabled = enabled;
    }

    /// Mark traces and spans that have error events in the host's error
    /// tracker: a `sentry` column in the trace list, `⚠` on spans, and `x` on
    /// such a span returns [`Outcome::JumpEvent`].
    pub fn set_error_lookup(&mut self, lookup: Arc<dyn crate::ErrorLookup>) {
        self.errors = Some(lookup);
    }

    /// Start with a given time window (e.g. `15m`, `6h`, `2d`, `1h30m`).
    pub fn with_since(client: Client, since: &str) -> Result<Self> {
        let mut e = Self::new(client);
        e.set_since(since)?;
        Ok(e)
    }

    /// Change the time window (e.g. `15m`). An invalid window is an error
    /// and changes nothing.
    pub fn set_since(&mut self, since: &str) -> Result<()> {
        let since = since.trim();
        let d = crate::parse_window(since)?;
        match self.windows.iter().position(|(w, _)| w == since) {
            Some(i) => self.win = i,
            None => {
                // At most one window besides the presets.
                self.windows.truncate(WINDOWS.len());
                self.windows.push((since.to_string(), d));
                self.win = self.windows.len() - 1;
            }
        }
        self.traces.pages = 0;
        self.invalidate();
        Ok(())
    }

    pub(crate) fn since(&self) -> &str {
        &self.windows[self.win].0
    }

    /// Window length in seconds (for rates).
    pub(crate) fn window_secs(&self) -> f64 {
        self.windows[self.win].1.as_secs_f64()
    }

    /// Jump to a trace (and its logs). Returns false, and shows why, when
    /// `trace_id` is not 32 hex characters (dashes are ignored).
    pub fn open_trace(&mut self, trace_id: &str) -> bool {
        self.open_trace_at(trace_id, None, None)
    }

    /// [`Embedded::open_trace`] of a trace known to be near `time`, on `span` if given.
    pub fn open_trace_at(&mut self, trace_id: &str, time: Option<DateTime<Utc>>, span: Option<&str>) -> bool {
        let id: String = trace_id.trim().chars().filter(|c| *c != '-').collect::<String>().to_lowercase();
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            self.flash_error(format!("not a trace id: {trace_id}"));
            return false;
        }
        self.view = View::Traces;
        self.help = false;
        self.prompt = None;
        self.logs.detail = None;
        match time {
            Some(_) => self.show_trace_detail(id, time, span.map(str::to_string)),
            None => self.open_trace_detail(id),
        }
        true
    }

    pub(crate) fn flash(&mut self, msg: impl Into<String>) {
        self.flash = Some((msg.into(), false, Instant::now()));
    }

    pub(crate) fn flash_error(&mut self, msg: impl Into<String>) {
        self.flash = Some((msg.into(), true, Instant::now()));
    }

    /// Force all views to reload on the next tick. In-flight requests are
    /// aborted so results for the old window are never applied.
    fn invalidate(&mut self) {
        self.loader.cancel_all_except(Kind::Stats);
        self.overview.refreshed = None;
        self.traces.refreshed = None;
        if let Some(d) = &mut self.traces.detail {
            d.requested = false;
            d.retry_at = None;
        }
        self.logs.reset();
        self.metrics.refreshed = None;
        if let Some(c) = &mut self.metrics.chart {
            c.refreshed = None;
        }
    }

    /// Apply finished loads and start due refreshes. Never blocks.
    pub fn tick(&mut self) {
        self.spin = self.spin.wrapping_add(1);
        for l in self.loader.drain() {
            match l.result {
                Ok(Data::Suggest(a)) => self.suggest_answer(*a),
                Ok(data) => {
                    self.last_ok = Some(Instant::now());
                    self.apply(l.kind, data);
                }
                Err(e) => self.apply_error(l.kind, &e),
            }
        }
        if let Some((_, _, at)) = &self.flash
            && at.elapsed() > Duration::from_secs(6)
        {
            self.flash = None;
        }
        if due(self.stats_at, Duration::from_secs(5)) && !self.loader.is_pending(Kind::Stats) {
            let c = self.client.clone();
            if self.loader.spawn(Kind::Stats, async move { c.stats().await }, |s| Data::Stats(Box::new(s))) {
                self.stats_at = Some(Instant::now());
            }
        }
        self.poll_suggest();
        if self.no_telemetry().is_none() {
            match self.view {
                View::Overview => self.refresh_overview(false),
                View::Traces => self.refresh_traces(false),
                View::Logs => self.refresh_logs(),
                View::Metrics => self.refresh_metrics(false),
                View::Status => {}
            }
        }
        if self.loader.no_runtime {
            self.stats_error = Some("no tokio runtime: cannot load data".into());
        }
    }

    fn apply(&mut self, kind: Kind, data: Data) {
        if let Data::Errors(Err(e)) = &data {
            // Flash when the lookup starts failing; keep the last markers.
            if self.failing.insert(kind) {
                self.flash_error(format!("sentry errors: {e}"));
            }
            return;
        }
        self.failing.remove(&kind);
        match data {
            Data::Stats(s) => {
                let accepted: u64 = s.ingest.signals.iter().map(|x| x.accepted).sum();
                if let Some((t, prev)) = self.prev_accepted {
                    let dt = t.elapsed().as_secs_f64();
                    if dt > 0.5 {
                        self.ingest_rate = Some(accepted.saturating_sub(prev) as f64 / dt);
                    }
                }
                self.prev_accepted = Some((Instant::now(), accepted));
                self.stats = Some(*s);
                self.stats_error = None;
            }
            Data::Services(mut v) => {
                // The endpoint has no `service` filter.
                v.retain(|s| self.in_scope(&s.raw));
                self.overview.set_services(v);
            }
            Data::Operations(v) => self.overview.set_operations(v),
            Data::SpanSeries(s) => self.overview.span_series = Some(s),
            Data::LogSeries(s) => match kind {
                Kind::LogVolume => self.logs.series = Some(s),
                _ => self.overview.log_series = Some(s),
            },
            Data::Traces(v) => {
                self.traces.set_list(v);
                self.load_trace_errors();
            }
            Data::Trace(t) => {
                self.traces.set_trace(*t);
                self.load_span_errors();
            }
            Data::Errors(r) => self.traces.set_errors(kind, r.unwrap_or_default()),
            Data::Logs(v) => self.traces.set_trace_logs(v),
            Data::LogPage { records, tail } => self.logs_loaded(records, *tail),
            Data::Tail { records, truncated } => self.logs_tail_loaded(records, truncated),
            Data::MetricList(v) => self.metrics.set_list(v),
            Data::MetricQuery(r) => self.metrics.set_result(r),
            Data::Suggest(a) => self.suggest_answer(*a),
        }
    }

    fn apply_error(&mut self, kind: Kind, err: &mallard_otel_client::Error) {
        let e = loader::short_error(err);
        // Flash when a kind starts failing, not on every retry.
        if self.failing.insert(kind) && (kind != Kind::Stats || self.stats.is_none()) {
            self.flash_error(e.clone());
        }
        match kind {
            Kind::Stats => self.stats_error = Some(e),
            Kind::Services | Kind::Operations | Kind::SpanSeries | Kind::LogSeries => self.overview.error = Some(e),
            Kind::LogVolume | Kind::LogTail => self.logs.error = Some(e),
            Kind::Logs => {
                // Retry the initial load after a short pause.
                self.logs.error = Some(e);
                self.logs.requested = false;
                self.logs.retry_at = Some(Instant::now() + RETRY);
            }
            Kind::Traces => self.traces.error = Some(e),
            Kind::Trace => {
                if let Some(d) = &mut self.traces.detail {
                    d.on_error(err);
                }
            }
            Kind::TraceLogs => {
                if let Some(d) = &mut self.traces.detail {
                    d.error = Some(e);
                }
            }
            Kind::MetricList => self.metrics.error = Some(e),
            // These report their failures as data.
            Kind::TraceErrors | Kind::SpanErrors | Kind::Suggest => {}
            Kind::MetricQuery => {
                if let Some(c) = &mut self.metrics.chart {
                    c.error = Some(e);
                }
            }
        }
    }

    fn set_view(&mut self, v: View) {
        self.view = v;
        self.help = false;
    }

    fn cycle_window(&mut self, forward: bool) {
        let n = self.windows.len();
        self.win = if forward { (self.win + 1) % n } else { (self.win + n - 1) % n };
        self.traces.pages = 0;
        self.invalidate();
        self.flash(format!("time window: last {}", self.since()));
    }

    /// Handle a terminal event.
    pub fn handle_event(&mut self, ev: &Event) -> Outcome {
        let Event::Key(k) = ev else {
            return Outcome::Ignored;
        };
        if k.kind == KeyEventKind::Release {
            return Outcome::Ignored;
        }
        if matches!(k.code, KeyCode::F(1) | KeyCode::F(2)) {
            return Outcome::Ignored;
        }
        if self.prompt.is_some() {
            return self.prompt_key(k);
        }
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            return Outcome::Quit;
        }
        if self.help {
            self.help = false;
            return Outcome::Consumed;
        }
        // Ctrl/Alt combinations belong to the host.
        if k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return Outcome::Ignored;
        }
        let handled = match self.view {
            View::Overview => self.key_overview(k),
            View::Traces => self.key_traces(k),
            View::Logs => self.key_logs(k),
            View::Metrics => self.key_metrics(k),
            View::Status => None,
        };
        if let Some(o) = handled {
            return o;
        }
        match k.code {
            KeyCode::Char('q') => Outcome::Quit,
            KeyCode::Char(keys::HELP) => {
                self.help = true;
                Outcome::Consumed
            }
            KeyCode::Char(c @ '1'..='5') => {
                self.set_view(VIEWS[(c as u8 - b'1') as usize].0);
                Outcome::Consumed
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let i = VIEWS.iter().position(|(v, _)| *v == self.view).unwrap_or(0);
                let n = VIEWS.len();
                let j = if k.code == KeyCode::Tab { (i + 1) % n } else { (i + n - 1) % n };
                self.set_view(VIEWS[j].0);
                Outcome::Consumed
            }
            KeyCode::Char(keys::WINDOW_NEXT) if !self.hosted => {
                self.cycle_window(true);
                Outcome::Consumed
            }
            KeyCode::Char(keys::WINDOW_PREV) if !self.hosted => {
                self.cycle_window(false);
                Outcome::Consumed
            }
            KeyCode::Char(keys::REFRESH) => {
                self.invalidate();
                self.stats_at = None;
                self.flash("refreshing…");
                Outcome::Consumed
            }
            _ => Outcome::Ignored,
        }
    }

    pub(crate) fn start_prompt(&mut self, target: PromptTarget, label: &'static str, help: &'static str, value: &str) {
        self.prompt = Some(Prompt { target, label, help, input: Input::new(value) });
        if let Some(s) = self.suggest_of(target) {
            s.open(value, value.chars().count(), Instant::now());
        }
    }

    /// A key while a prompt is open. Ctrl-C and Esc cancel it.
    fn prompt_key(&mut self, k: &KeyEvent) -> Outcome {
        let Some(target) = self.prompt.as_ref().map(|p| p.target) else { return Outcome::Ignored };
        let now = Instant::now();
        // Tab and the arrows are the list's while it shows; every other key is the input's.
        if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && let Some(s) = self.suggest_of(target)
            && s.has_items()
        {
            match k.code {
                KeyCode::Tab => {
                    if let (Some((value, cursor)), Some(p)) = (s.accept(now), &mut self.prompt) {
                        p.input = Input { value, cursor };
                    }
                    return Outcome::Consumed;
                }
                KeyCode::Down | KeyCode::Up => {
                    s.select(if k.code == KeyCode::Down { 1 } else { -1 });
                    return Outcome::Consumed;
                }
                _ => {}
            }
        }
        let Some(p) = &mut self.prompt else { return Outcome::Ignored };
        match p.input.handle(k) {
            InputResult::Ignored => return Outcome::Ignored,
            InputResult::Edited => {
                let (value, cursor) = (p.input.value.clone(), p.input.cursor);
                if p.target == PromptTarget::MetricSearch {
                    self.metrics.search = value.clone();
                    self.metrics.clamp_selection();
                }
                if let Some(s) = self.suggest_of(target) {
                    s.edited(&value, cursor, now);
                }
            }
            // A metric search keeps what was typed; Esc in the list clears it.
            InputResult::Cancel => {
                self.prompt = None;
                self.close_suggest(target);
            }
            InputResult::Submit => {
                self.close_suggest(target);
                let p = self.prompt.take().unwrap();
                let v = p.input.value.trim().to_string();
                match p.target {
                    PromptTarget::TraceFilter => self.set_trace_filter(&v),
                    PromptTarget::LogFilter => self.set_log_filter(&v),
                    PromptTarget::MetricSearch => {
                        self.metrics.search = v;
                        self.metrics.clamp_selection();
                    }
                    PromptTarget::MetricGroupBy => self.set_metric_group_by(&v),
                }
            }
        }
        Outcome::Consumed
    }

    fn close_suggest(&mut self, target: PromptTarget) {
        self.loader.cancel(Kind::Suggest);
        if let Some(s) = self.suggest_of(target) {
            s.close();
        }
    }

    /// Render the whole UI into `area`.
    pub fn draw(&mut self, f: &mut Frame, area: Rect) {
        if area.width < 20 || area.height < 6 {
            f.render_widget(Paragraph::new("window too small"), area);
            return;
        }
        let [header, tabs, body, footer] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)])
                .areas(area);
        self.draw_header(f, header);
        self.draw_tabs(f, tabs);
        if let Some(slug) = self.no_telemetry() {
            let (slug, since) = (clean(slug).into_owned(), self.since().to_string());
            self.draw_no_telemetry(f, body, &slug, &since);
        } else {
            match self.view {
                View::Overview => self.draw_overview(f, body),
                View::Traces => self.draw_traces(f, body),
                View::Logs => self.draw_logs(f, body),
                View::Metrics => self.draw_metrics(f, body),
                View::Status => self.draw_status(f, body),
            }
        }
        self.draw_footer(f, footer);
        if let Some(target) = self.prompt.as_ref().map(|p| p.target)
            && let Some(s) = self.suggest_of(target)
        {
            s.render(f, body, footer);
        }
        if self.help {
            self.draw_help(f, body);
        }
    }
}
