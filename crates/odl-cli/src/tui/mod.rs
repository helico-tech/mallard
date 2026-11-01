//! Terminal UI for otel-ducklake.
//!
//! [`Embedded`] is the whole UI as a component: the host owns the terminal
//! (raw mode, alternate screen) and the event loop, and calls
//! [`Embedded::draw`], [`Embedded::handle_event`] and [`Embedded::tick`]
//! (~4x per second). Loading runs on the tokio runtime in the background and
//! never blocks input. [`run_standalone`] wraps it for `odl tui`.
//!
//! F1 and F2 are reserved for the host and always return [`Outcome::Ignored`],
//! and so do keys with Ctrl or Alt (except Ctrl-C, which quits), so host
//! shortcuts keep working.
//!
//! A tokio runtime must be current when `tick` is called (loads are spawned on
//! it), and the host's event loop must not block that runtime's only thread.

mod input;
mod loader;
mod query;
mod theme;
mod views;
pub(crate) mod waterfall;
mod widgets;

#[cfg(test)]
mod regress;
#[cfg(test)]
mod tests;

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use odl_client::{Client, api::Stats};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};

use input::{Input, InputResult};
use loader::{Data, Kind, Loader};

/// What the host should do after an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The UI used the event.
    Consumed,
    /// Not handled; the host may use it.
    Ignored,
    /// The user asked to quit.
    Quit,
    /// The user asked for errors related to this trace id (e.g. Sentry issues).
    OpenErrors(String),
}

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

const WINDOWS: [&str; 6] = ["5m", "15m", "1h", "6h", "24h", "7d"];

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
    windows: Vec<String>,
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
    /// Kinds whose last load failed: only the first failure flashes.
    failing: HashSet<Kind>,
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
            windows: WINDOWS.iter().map(|s| s.to_string()).collect(),
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
            failing: HashSet::new(),
        }
    }

    /// Show the `e errors` key in hints and help. The key always returns
    /// [`Outcome::OpenErrors`]; enable this when the host can act on it
    /// (e.g. an error tracker is connected). Off by default.
    pub fn set_errors_enabled(&mut self, enabled: bool) {
        self.errors_enabled = enabled;
    }

    /// Start with a given time window (e.g. `15m`, `6h`, `2d`).
    pub fn with_since(client: Client, since: &str) -> Self {
        let mut e = Self::new(client);
        e.set_since(since);
        e
    }

    /// Change the time window (e.g. `15m`).
    pub fn set_since(&mut self, since: &str) {
        let since = since.trim();
        if since.is_empty() {
            return;
        }
        match self.windows.iter().position(|w| w == since) {
            Some(i) => self.win = i,
            None => {
                self.windows.push(since.to_string());
                self.win = self.windows.len() - 1;
            }
        }
        self.invalidate();
    }

    pub(crate) fn since(&self) -> String {
        self.windows[self.win].clone()
    }

    /// Window length in seconds (for rates); falls back to one hour.
    pub(crate) fn window_secs(&self) -> f64 {
        humantime_secs(&self.since()).unwrap_or(3600.0)
    }

    /// Jump to a trace (and its logs).
    pub fn open_trace(&mut self, trace_id: &str) {
        let id: String = trace_id.trim().chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_lowercase();
        if id.len() != 32 {
            self.flash_error(format!("not a trace id: {trace_id}"));
            return;
        }
        self.view = View::Traces;
        self.help = false;
        self.prompt = None;
        self.logs.detail = None;
        self.open_trace_detail(id);
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
            if self.loader.spawn(Kind::Stats, async move { c.stats().await }, Data::Stats) {
                self.stats_at = Some(Instant::now());
            }
        }
        match self.view {
            View::Overview => self.refresh_overview(false),
            View::Traces => self.refresh_traces(false),
            View::Logs => self.refresh_logs(false),
            View::Metrics => self.refresh_metrics(false),
            View::Status => {}
        }
        if self.loader.no_runtime {
            self.stats_error = Some("no tokio runtime: cannot load data".into());
        }
    }

    fn apply(&mut self, kind: Kind, data: Data) {
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
                self.stats = Some(s);
                self.stats_error = None;
            }
            Data::Services(v) => self.overview.set_services(v),
            Data::Operations(v) => self.overview.set_operations(v),
            Data::SpanSeries(s) => self.overview.span_series = Some(s),
            Data::LogSeries(s) => match kind {
                Kind::LogVolume => self.logs.series = Some(s),
                _ => self.overview.log_series = Some(s),
            },
            Data::Traces(v) => self.traces.set_list(v),
            Data::Trace(t) => self.traces.set_trace(*t),
            Data::Logs(v) => match kind {
                Kind::TraceLogs => self.traces.set_trace_logs(v),
                _ => self.logs_loaded(v),
            },
            Data::Tail((v, gap)) => self.logs_tail_loaded(v, gap),
            Data::MetricList(v) => self.metrics.set_list(v),
            Data::MetricQuery(r) => self.metrics.set_result(r),
        }
    }

    fn apply_error(&mut self, kind: Kind, err: &odl_client::Error) {
        let e = loader::short_error(err);
        match kind {
            Kind::Stats => self.stats_error = Some(e.clone()),
            Kind::Services | Kind::Operations | Kind::SpanSeries | Kind::LogSeries => {
                self.overview.error = Some(e.clone())
            }
            Kind::LogVolume | Kind::LogTail => self.logs.error = Some(e.clone()),
            Kind::Logs => {
                // Retry the initial load after a short pause.
                self.logs.error = Some(e.clone());
                self.logs.requested = false;
                self.logs.retry_at = Some(Instant::now() + RETRY);
            }
            Kind::Traces => self.traces.error = Some(e.clone()),
            Kind::Trace => {
                if let Some(d) = &mut self.traces.detail {
                    d.on_error(err);
                }
            }
            Kind::TraceLogs => {
                if let Some(d) = &mut self.traces.detail {
                    d.error = Some(e.clone());
                }
            }
            Kind::MetricList => self.metrics.error = Some(e.clone()),
            Kind::MetricQuery => {
                if let Some(c) = &mut self.metrics.chart {
                    c.error = Some(e.clone());
                }
            }
        }
        // Flash when a kind starts failing, not on every retry.
        let first = self.failing.insert(kind);
        if first && (kind != Kind::Stats || self.stats.is_none()) {
            self.flash_error(e);
        }
    }

    fn set_view(&mut self, v: View) {
        self.view = v;
        self.help = false;
    }

    fn cycle_window(&mut self, forward: bool) {
        let n = self.windows.len();
        self.win = if forward { (self.win + 1) % n } else { (self.win + n - 1) % n };
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
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            return Outcome::Quit;
        }
        if self.help {
            self.help = false;
            return Outcome::Consumed;
        }
        if self.prompt.is_some() {
            self.prompt_key(k);
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
            KeyCode::Char('?') => {
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
            KeyCode::Char('t' | 'w') => {
                self.cycle_window(true);
                Outcome::Consumed
            }
            KeyCode::Char('T' | 'W') => {
                self.cycle_window(false);
                Outcome::Consumed
            }
            KeyCode::Char('r') => {
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
    }

    fn prompt_key(&mut self, k: &KeyEvent) {
        let Some(p) = &mut self.prompt else { return };
        match p.input.handle(k) {
            InputResult::Edited => {
                if p.target == PromptTarget::MetricSearch {
                    self.metrics.search = p.input.value.clone();
                    self.metrics.clamp_selection();
                }
            }
            InputResult::Cancel => {
                if p.target == PromptTarget::MetricSearch {
                    // keep what was typed; Esc in the list clears it
                }
                self.prompt = None;
            }
            InputResult::Submit => {
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
        match self.view {
            View::Overview => self.draw_overview(f, body),
            View::Traces => self.draw_traces(f, body),
            View::Logs => self.draw_logs(f, body),
            View::Metrics => self.draw_metrics(f, body),
            View::Status => self.draw_status(f, body),
        }
        self.draw_footer(f, footer);
        if self.help {
            self.draw_help(f, body);
        }
    }

    fn live(&self) -> (bool, &'static str) {
        match self.last_ok {
            Some(t) if t.elapsed() < Duration::from_secs(20) => (true, "live"),
            Some(_) => (false, "stale"),
            None if self.stats_error.is_some() => (false, "offline"),
            None => (false, "connecting"),
        }
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let (ok, label) = self.live();
        let mut right = vec![];
        if self.loader.any_pending() {
            right.push(Span::styled(format!("{} ", SPIN[self.spin % SPIN.len()]), theme::dim()));
        }
        if let Some(r) = self.ingest_rate {
            right.push(Span::styled("↗ ", theme::muted()));
            right.push(Span::styled(format!("{}/s", crate::fmt::compact(r)), theme::text()));
            right.push(Span::styled(" ingest  ", theme::muted()));
        }
        right.push(Span::styled("◷ ", theme::muted()));
        right.push(Span::styled(format!("last {}", self.since()), theme::accent()));
        right.push(Span::raw("  "));
        right.push(Span::styled("● ", Style::new().fg(if ok { theme::OK } else { theme::ERR })));
        right.push(Span::styled(format!("{label} "), theme::dim()));
        let right = Line::from(right);
        let rw = right.width() as u16;
        let left = Line::from(vec![
            Span::styled(" ◆ otel-ducklake ", theme::accent()),
            Span::styled("│ ", theme::muted()),
            Span::styled(
                widgets::trunc(self.client.base_url(), area.width.saturating_sub(rw + 22) as usize),
                theme::dim(),
            ),
        ]);
        f.render_widget(Block::default().style(Style::new().bg(theme::SURFACE)), area);
        f.render_widget(left, area);
        if rw < area.width {
            f.render_widget(right, Rect { x: area.x + area.width - rw, width: rw, ..area });
        }
    }

    fn draw_tabs(&self, f: &mut Frame, area: Rect) {
        let mut spans = vec![Span::raw(" ")];
        for (i, (v, name)) in VIEWS.iter().enumerate() {
            if *v == self.view {
                spans.push(Span::styled(format!(" {} {name} ", i + 1), theme::badge(theme::ACCENT)));
            } else {
                spans.push(Span::styled(format!(" {}", i + 1), theme::muted()));
                spans.push(Span::styled(format!(" {name} "), theme::dim()));
            }
            spans.push(Span::raw(" "));
        }
        f.render_widget(Line::from(spans), area);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        if let Some(p) = &self.prompt {
            let label = format!(" {} ❯ ", p.label);
            let line = Line::from(vec![
                Span::styled(label.clone(), theme::badge(theme::ACCENT2)),
                Span::raw(" "),
                Span::styled(p.input.value.clone(), theme::bold()),
                Span::styled(format!("   {}", p.help), theme::muted()),
            ]);
            f.render_widget(line, area);
            let before: String = p.input.value.chars().take(p.input.cursor).collect();
            let x = area.x + label.chars().count() as u16 + 1 + Line::from(before).width() as u16;
            f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
            return;
        }
        let mut spans = widgets::hints(&self.hints());
        spans.extend(widgets::hints(&[("?", "help"), ("q", "quit")]));
        let line = Line::from(spans);
        let mut hints_area = area;
        if let Some((msg, is_err, _)) = &self.flash {
            let text = format!(" {} ", widgets::trunc(msg, (area.width / 2) as usize));
            let w = Line::from(text.as_str()).width() as u16;
            let style = if *is_err { theme::badge(theme::ERR) } else { theme::badge(theme::OK) };
            if w < area.width {
                f.render_widget(Span::styled(text, style), Rect { x: area.right() - w, width: w, ..area });
                // Hints get the rest, with a one-cell gap.
                hints_area.width = area.width.saturating_sub(w + 1);
            }
        }
        f.render_widget(line, hints_area);
    }

    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        let mut h = self.view_hints();
        if !self.errors_enabled {
            h.retain(|(k, _)| *k != "e");
        }
        h
    }

    fn view_hints(&self) -> Vec<(&'static str, &'static str)> {
        match self.view {
            View::Overview if self.overview.ops.is_some() => {
                vec![("↵", "traces"), ("esc", "services"), ("t", "window")]
            }
            View::Overview => vec![("↵", "operations"), ("t", "window"), ("r", "refresh")],
            View::Traces if self.traces.detail.is_some() => {
                vec![("j/k", "span"), ("pgup/dn", "scroll"), ("l", "logs"), ("e", "errors"), ("esc", "back")]
            }
            View::Traces => {
                vec![("/", "filter"), ("E", "errors only"), ("↵", "open"), ("e", "errors"), ("t", "window")]
            }
            View::Logs if self.logs.detail.is_some() => {
                vec![("o", "open trace"), ("j/k", "scroll"), ("esc", "close")]
            }
            View::Logs => vec![
                ("/", "filter"),
                ("s", "severity"),
                ("space", if self.logs.follow { "pause" } else { "follow" }),
                ("↵", "details"),
                ("o", "trace"),
            ],
            View::Metrics if self.metrics.chart.is_some() => {
                vec![("a", "aggregation"), ("b", "group by"), ("esc", "back"), ("t", "window")]
            }
            View::Metrics => vec![("/", "search"), ("↵", "chart"), ("t", "window")],
            View::Status => vec![("r", "refresh")],
        }
    }

    fn draw_help(&self, f: &mut Frame, area: Rect) {
        let rows: &[(&str, &str)] = &[
            ("1-5  tab", "switch view"),
            ("t / T", "next / previous time window"),
            ("r", "refresh now"),
            ("↑↓ j k  pgup pgdn  g G", "move"),
            ("↵", "open / drill down"),
            ("esc", "back / close"),
            ("q  ctrl-c", "quit"),
            ("", ""),
            ("Overview", "↵ service → operations → traces"),
            (
                "Traces",
                if self.errors_enabled {
                    "/ filter  E errors only  ↵ waterfall  l logs  e errors"
                } else {
                    "/ filter  E errors only  ↵ waterfall  l logs"
                },
            ),
            ("", "filter: service=checkout op=\"GET /\" errors min=250ms key=value"),
            ("Logs", "/ filter  s severity  space follow  ↵ details  o open trace"),
            ("", "filter: service=api level=warn trace=<id> words to find"),
            ("Metrics", "/ search  ↵ chart  a aggregation  b group by"),
        ];
        let w = 78.min(area.width);
        let h = (rows.len() as u16 + 4).min(area.height);
        let r = widgets::centered(area, w, h);
        f.render_widget(Clear, r);
        let lines: Vec<Line> = rows
            .iter()
            .map(|(k, d)| {
                Line::from(vec![
                    Span::styled(format!("{k:>24}  "), theme::accent()),
                    Span::styled(d.to_string(), theme::text()),
                ])
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(widgets::panel("Keys", true).title_bottom(Line::from(" any key to close ").right_aligned())),
            r,
        );
    }
}

pub(crate) fn humantime_secs(s: &str) -> Option<f64> {
    let s = s.trim();
    let idx = s.find(|c: char| !c.is_ascii_digit())?;
    let n: f64 = s[..idx].parse().ok()?;
    let mult = match &s[idx..] {
        "s" => 1.0,
        "m" => 60.0,
        "h" => 3600.0,
        "d" => 86400.0,
        "w" => 7.0 * 86400.0,
        _ => return None,
    };
    Some(n * mult)
}

/// Run the TUI in this terminal until the user quits.
pub async fn run_standalone(client: Client, since: String) -> Result<()> {
    let mut app = Embedded::with_since(client, &since);
    // try_init installs a panic hook that restores the terminal.
    let mut terminal = ratatui::try_init()?;
    let res = event_loop(&mut terminal, &mut app).await;
    ratatui::restore();
    res
}

/// Async event loop: terminal events arrive through crossterm's EventStream,
/// so the runtime keeps running background loads (also on a current-thread
/// runtime).
async fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut Embedded) -> Result<()> {
    use futures_util::StreamExt;
    let mut events = event::EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        terminal.draw(|f| app.draw(f, f.area()))?;
        tokio::select! {
            _ = ticker.tick() => app.tick(),
            ev = events.next() => match ev {
                Some(Ok(ev)) => match app.handle_event(&ev) {
                    Outcome::Quit => return Ok(()),
                    Outcome::OpenErrors(_) => app.flash_error("no error tracker connected"),
                    _ => {}
                },
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(()),
            },
        }
    }
}
