//! Traces: filterable trace list and a waterfall with span details.

use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, Utc};
use crossterm::event::{KeyCode, KeyEvent};
use odl_client::api::*;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span as TSpan},
    widgets::{Cell, List, ListItem, ListState, Paragraph, Row as TRow, Table, TableState, Wrap},
};

use crate::{
    fmt::{compact, ms},
    tui::{
        Embedded, Outcome, PromptTarget, View, due,
        loader::{Data, Kind},
        nav,
        query::{TraceFilter, parse_trace_filter},
        theme,
        waterfall::{self, Row},
        widgets,
    },
};

#[derive(Default)]
pub struct State {
    pub filter: TraceFilter,
    pub list: Vec<TraceSummary>,
    pub sel: TableState,
    pub error: Option<String>,
    pub refreshed: Option<Instant>,
    pub detail: Option<Detail>,
}

pub struct Detail {
    pub trace_id: String,
    /// Approximate start, used to narrow the lookup window.
    pub hint: Option<DateTime<Utc>>,
    pub trace: Option<Trace>,
    pub rows: Vec<Row>,
    pub total_ns: i64,
    pub sel: ListState,
    pub scroll: u16,
    pub logs: Vec<LogRecord>,
    pub requested: bool,
    pub error: Option<String>,
    /// Retry a failed lookup after this time.
    pub retry_at: Option<Instant>,
    /// Use a 24 h window: the server rejected 7 days (query.max_window).
    pub short_window: bool,
    /// Failed lookups in a row, for the retry back-off.
    pub failures: u32,
}

impl Detail {
    pub fn new(trace_id: String, hint: Option<DateTime<Utc>>) -> Self {
        Self {
            trace_id,
            hint,
            trace: None,
            rows: vec![],
            total_ns: 1,
            sel: ListState::default(),
            scroll: 0,
            logs: vec![],
            requested: false,
            error: None,
            retry_at: None,
            short_window: false,
            failures: 0,
        }
    }

    /// A lookup failed: show it and retry later (a trace may not be flushed
    /// yet), with a shorter window if the server refused the default one.
    /// Retries back off from 3 s to about a minute.
    pub fn on_error(&mut self, e: &odl_client::Error) {
        self.error = Some(crate::tui::loader::short_error(e));
        self.requested = false;
        if e.is_window_too_large() && !self.short_window {
            self.short_window = true;
            self.retry_at = Some(Instant::now());
        } else {
            self.retry_at = Some(Instant::now() + retry_delay(self.failures));
            self.failures = self.failures.saturating_add(1);
        }
    }

    pub fn selected_span(&self) -> Option<&Span> {
        let t = self.trace.as_ref()?;
        let r = self.rows.get(self.sel.selected()?)?;
        t.spans.get(r.span)
    }
}

/// Pause before retry number `failures + 1`: 3 s, 6 s, 12 s, … up to 48 s.
fn retry_delay(failures: u32) -> Duration {
    crate::tui::RETRY * 2u32.pow(failures.min(4))
}

impl State {
    pub fn set_list(&mut self, v: Vec<TraceSummary>) {
        let keep = self.sel.selected().and_then(|i| self.list.get(i)).map(|t| t.trace_id.clone());
        self.list = v;
        self.error = None;
        let i = keep.and_then(|id| self.list.iter().position(|t| t.trace_id == id)).unwrap_or(0);
        self.sel.select((!self.list.is_empty()).then_some(i.min(self.list.len().saturating_sub(1))));
    }

    pub fn set_trace(&mut self, t: Trace) {
        if let Some(d) = &mut self.detail
            && d.trace_id == t.trace_id
        {
            let (rows, total) = waterfall::layout(&t.spans);
            // Keep the selected span across refreshes.
            let keep = d.selected_span().map(|s| s.span_id.clone());
            let refresh = d.trace.is_some();
            d.rows = rows;
            d.total_ns = total;
            d.trace = Some(t);
            d.error = None;
            d.retry_at = None;
            d.failures = 0;
            let i = keep.and_then(|id| {
                let t = d.trace.as_ref()?;
                d.rows.iter().position(|r| t.spans.get(r.span).is_some_and(|s| s.span_id == id))
            });
            d.sel.select(Some(i.unwrap_or(0)));
            if !refresh {
                d.scroll = 0;
            }
        }
    }

    pub fn set_trace_logs(&mut self, v: Vec<LogRecord>) {
        if let Some(d) = &mut self.detail {
            d.logs = v.into_iter().filter(|l| l.trace_id.as_deref() == Some(d.trace_id.as_str())).collect();
        }
    }
}

impl Embedded {
    pub(crate) fn refresh_traces(&mut self, force: bool) {
        if let Some(d) = &mut self.traces.detail {
            if d.requested || d.retry_at.is_some_and(|t| Instant::now() < t) {
                return;
            }
            let (from, to, since) = match d.hint {
                Some(h) => (Some(h - TimeDelta::hours(1)), Some(h + TimeDelta::hours(1)), None),
                None if d.short_window => (None, None, Some("24h".to_string())),
                None => (None, None, Some("7d".to_string())),
            };
            let id = d.trace_id.clone();
            let c = self.client.clone();
            let q = TraceGetQuery { from, to, since: since.clone() };
            let idc = id.clone();
            if !self.loader.spawn(Kind::Trace, async move { c.trace(&idc, &q).await.map(Box::new) }, Data::Trace) {
                return;
            }
            d.requested = true;
            let c = self.client.clone();
            let q = LogQuery {
                from,
                to,
                since,
                trace_id: Some(id),
                limit: Some(2000),
                order: Some("asc".into()),
                ..Default::default()
            };
            self.loader.spawn(Kind::TraceLogs, async move { c.logs(&q).await }, Data::Logs);
            return;
        }
        if !force && !due(self.traces.refreshed, Duration::from_secs(5)) {
            return;
        }
        let f = &self.traces.filter;
        let q = TraceQuery {
            since: Some(self.since().to_string()),
            service: f.service.clone(),
            operation: f.operation.clone(),
            errors_only: Some(f.errors_only),
            min_duration_ms: f.min_ms,
            max_duration_ms: f.max_ms,
            attributes: crate::attrs(&f.attrs),
            limit: Some(200),
            ..Default::default()
        };
        let c = self.client.clone();
        if self.loader.load(Kind::Traces, force, async move { c.traces(&q).await }, Data::Traces) {
            self.traces.refreshed = Some(Instant::now());
        }
    }

    pub(crate) fn open_trace_detail(&mut self, id: String) {
        let hint = self.traces.list.iter().find(|t| t.trace_id == id).map(|t| t.start_time);
        self.loader.cancel(Kind::Trace);
        self.loader.cancel(Kind::TraceLogs);
        self.traces.detail = Some(Detail::new(id, hint));
        self.refresh_traces(true);
    }

    pub(crate) fn set_trace_filter(&mut self, q: &str) {
        match parse_trace_filter(q) {
            Ok(f) => {
                self.traces.filter = f;
                self.traces.refreshed = None;
                self.traces.sel.select(Some(0));
                self.loader.cancel(Kind::Traces);
            }
            Err(e) => self.flash_error(e),
        }
    }

    fn logs_for_trace(&mut self, id: &str) {
        self.set_view_logs_with(&format!("trace={id}"));
    }

    pub(crate) fn set_view_logs_with(&mut self, query: &str) {
        self.view = View::Logs;
        self.set_log_filter(query);
    }

    pub(crate) fn key_traces(&mut self, k: &KeyEvent) -> Option<Outcome> {
        if let Some(d) = &mut self.traces.detail {
            match k.code {
                KeyCode::PageDown | KeyCode::Char(']') => {
                    d.scroll = d.scroll.saturating_add(10);
                    return Some(Outcome::Consumed);
                }
                KeyCode::PageUp | KeyCode::Char('[') => {
                    d.scroll = d.scroll.saturating_sub(10);
                    return Some(Outcome::Consumed);
                }
                KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => {
                    self.traces.detail = None;
                    self.traces.refreshed = None;
                    return Some(Outcome::Consumed);
                }
                KeyCode::Char('e') => return Some(Outcome::OpenErrors(d.trace_id.clone())),
                KeyCode::Char('l') => {
                    let id = d.trace_id.clone();
                    self.logs_for_trace(&id);
                    return Some(Outcome::Consumed);
                }
                KeyCode::Enter => return Some(Outcome::Consumed),
                _ => {}
            }
            if let Some(i) = nav(d.sel.selected().unwrap_or(0), d.rows.len(), k, 10) {
                d.sel.select(Some(i));
                d.scroll = 0;
                return Some(Outcome::Consumed);
            }
            return None;
        }
        let t = &mut self.traces;
        if let Some(i) = nav(t.sel.selected().unwrap_or(0), t.list.len(), k, 10) {
            t.sel.select(Some(i));
            return Some(Outcome::Consumed);
        }
        let selected = t.sel.selected().and_then(|i| t.list.get(i)).map(|x| x.trace_id.clone());
        match k.code {
            KeyCode::Char('/' | 'f') => {
                let v = t.filter.to_query();
                self.start_prompt(
                    PromptTarget::TraceFilter,
                    "filter traces",
                    "service= op= errors min=250ms key=value · ↵ apply · esc cancel",
                    &v,
                );
                Some(Outcome::Consumed)
            }
            KeyCode::Char('E') => {
                t.filter.errors_only = !t.filter.errors_only;
                t.refreshed = None;
                self.loader.cancel(Kind::Traces);
                let msg = if self.traces.filter.errors_only { "only traces with errors" } else { "all traces" };
                self.flash(msg);
                Some(Outcome::Consumed)
            }
            KeyCode::Char('c') => {
                t.filter = TraceFilter::default();
                t.refreshed = None;
                self.loader.cancel(Kind::Traces);
                Some(Outcome::Consumed)
            }
            KeyCode::Enter | KeyCode::Right => {
                self.open_trace_detail(selected?);
                Some(Outcome::Consumed)
            }
            KeyCode::Char('e') => selected.map(Outcome::OpenErrors),
            KeyCode::Char('l') => {
                self.logs_for_trace(&selected?);
                Some(Outcome::Consumed)
            }
            _ => None,
        }
    }

    pub(crate) fn draw_traces(&mut self, f: &mut Frame, area: Rect) {
        if self.traces.detail.is_some() {
            self.draw_trace_detail(f, area);
        } else {
            self.draw_trace_list(f, area);
        }
    }

    fn draw_trace_list(&mut self, f: &mut Frame, area: Rect) {
        let long = self.window_secs() > 86_400.0;
        let t = &mut self.traces;
        let q = t.filter.to_query();
        let filter_line = Line::from(vec![
            TSpan::styled(" ⌕ ", theme::accent()),
            if q.is_empty() {
                TSpan::styled("all traces · press / to filter", theme::muted())
            } else {
                TSpan::styled(q, theme::bold())
            },
        ]);
        let block =
            widgets::panel_with("Traces", Line::from(format!(" {} newest ", t.list.len())).style(theme::muted()), true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let [fl, table_area] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
        f.render_widget(filter_line, fl);
        let max_dur = t.list.iter().map(|x| x.duration_ms).fold(0.0, f64::max).max(1e-9);
        let rows: Vec<TRow> = t
            .list
            .iter()
            .map(|tr| {
                let (svc, name) = match (&tr.root_service, &tr.root_name) {
                    (Some(s), Some(n)) => (s.clone(), n.clone()),
                    _ => (tr.services.first().cloned().unwrap_or_default(), "(root not received)".into()),
                };
                let frac = tr.duration_ms / max_dur;
                let filled = ((frac * 10.0).round() as usize).clamp(1, 10);
                let bar_color = if tr.error_count > 0 { theme::ERR } else { theme::ACCENT };
                TRow::new(vec![
                    Cell::from(widgets::local(tr.start_time, if long { "%m-%d %H:%M:%S" } else { "%H:%M:%S%.3f" }))
                        .style(theme::dim()),
                    Cell::from(Line::from(vec![
                        TSpan::styled("● ", Style::new().fg(theme::service_color(&svc))),
                        TSpan::styled(format!("{svc} "), theme::dim()),
                        TSpan::styled(name, theme::text()),
                    ])),
                    Cell::from(Line::from(ms(tr.duration_ms)).right_aligned()).style(theme::text()),
                    Cell::from(Line::from(vec![
                        TSpan::styled("▬".repeat(filled), Style::new().fg(bar_color)),
                        TSpan::styled("─".repeat(10 - filled), theme::muted()),
                    ])),
                    Cell::from(Line::from(compact(tr.span_count as f64)).right_aligned()).style(theme::dim()),
                    if tr.error_count > 0 {
                        Cell::from(Line::from(format!("✗ {}", tr.error_count)).right_aligned()).style(theme::error())
                    } else {
                        Cell::from(Line::from("·").right_aligned()).style(theme::muted())
                    },
                    Cell::from(tr.services.join(" ")).style(theme::muted()),
                ])
            })
            .collect();
        let empty = rows.is_empty();
        let header = TRow::new(vec![
            Cell::from("start"),
            Cell::from("root"),
            Cell::from(Line::from("duration").right_aligned()),
            Cell::from(""),
            Cell::from(Line::from("spans").right_aligned()),
            Cell::from(Line::from("errors").right_aligned()),
            Cell::from("services"),
        ])
        .style(theme::header_row());
        let widths = [
            Constraint::Length(if long { 14 } else { 12 }),
            Constraint::Fill(3),
            Constraint::Length(9),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Fill(2),
        ];
        let sel = t.sel.selected();
        widgets::fit_offset(t.sel.offset_mut(), sel, t.list.len(), table_area.height.saturating_sub(1));
        f.render_stateful_widget(
            Table::new(rows, widths).header(header).row_highlight_style(theme::selected()).highlight_symbol("▌"),
            table_area,
            &mut t.sel,
        );
        if empty {
            let (msg, style) = match &t.error {
                Some(e) => (format!("⚠ {e}"), theme::error()),
                None if t.refreshed.is_some() => ("no matching traces in this window".into(), theme::muted()),
                None => ("loading…".into(), theme::muted()),
            };
            widgets::placeholder(f, table_area, &msg, style);
        }
    }

    fn draw_trace_detail(&mut self, f: &mut Frame, area: Rect) {
        let Some(d) = &mut self.traces.detail else {
            return;
        };
        let wide = area.width >= 110;
        let [wf_area, info_area] = if wide {
            Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(area)
        } else {
            Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area)
        };
        let Some(trace) = &d.trace else {
            let block = widgets::panel(&format!("Trace {}", d.trace_id), true);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let (msg, style) = match &d.error {
                Some(e) => (format!("⚠ {e}"), theme::error()),
                None => ("loading trace…".to_string(), theme::muted()),
            };
            widgets::placeholder(f, inner, &msg, style);
            return;
        };
        let errors = trace.spans.iter().filter(|s| s.status_code == "ERROR").count();
        let services: std::collections::BTreeSet<&str> = trace.spans.iter().map(|s| s.service_name.as_str()).collect();
        let mut right = vec![
            TSpan::styled(format!(" {} ", ms(d.total_ns as f64 / 1e6)), theme::bold()),
            TSpan::styled(format!("· {} spans · {} services ", trace.spans.len(), services.len()), theme::dim()),
        ];
        if errors > 0 {
            right.push(TSpan::styled(format!("· ✗ {errors} "), theme::error()));
        }
        if !d.logs.is_empty() {
            right.push(TSpan::styled(format!("· {} logs ", d.logs.len()), theme::dim()));
        }
        let block = widgets::panel_with(&format!("Trace {}", d.trace_id), Line::from(right), true);
        let inner = block.inner(wf_area);
        f.render_widget(block, wf_area);
        if inner.height < 2 || inner.width < 20 {
            return;
        }
        let [ruler, list_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
        let w = list_area.width.saturating_sub(1) as usize; // highlight symbol
        let dur_w = 9usize;
        let label_w = (w * 45 / 100).clamp(18.min(w), 70);
        let bar_w = w.saturating_sub(label_w + dur_w + 1);
        // ruler: 0 .. mid .. total
        let total_ms = d.total_ns as f64 / 1e6;
        let mut r = format!("{:<label_w$} ", "service · operation");
        let left = "0";
        let mid = ms(total_ms / 2.0);
        let end = ms(total_ms);
        if bar_w > mid.len() + end.len() + 4 {
            let mut bar = vec![' '; bar_w];
            for (i, c) in left.chars().enumerate() {
                bar[i] = c;
            }
            let ms_ = bar_w / 2 - mid.len() / 2;
            for (i, c) in mid.chars().enumerate() {
                bar[ms_ + i] = c;
            }
            let es = bar_w - end.len();
            for (i, c) in end.chars().enumerate() {
                bar[es + i] = c;
            }
            r.push_str(&bar.into_iter().collect::<String>());
        }
        f.render_widget(TSpan::styled(format!(" {r}"), theme::muted()), ruler);
        // Build items only for what fits: traces can have thousands of spans.
        let sel = d.sel.selected();
        let (shown, sel) = widgets::visible(d.sel.offset_mut(), sel, d.rows.len(), list_area.height);
        let items: Vec<ListItem> = d.rows[shown]
            .iter()
            .map(|row| {
                let s = &trace.spans[row.span];
                let is_err = s.status_code == "ERROR";
                let svc_color = theme::service_color(&s.service_name);
                let mut label: Vec<TSpan> = vec![TSpan::styled(row.prefix.clone(), theme::muted())];
                let used = widgets::width(&row.prefix);
                let svc = widgets::trunc(&s.service_name, 14.min(label_w.saturating_sub(used + 2)).max(1));
                let rest = label_w.saturating_sub(used + widgets::width(&svc) + 1);
                label.push(TSpan::styled(svc.clone(), Style::new().fg(svc_color)));
                label.push(TSpan::raw(" "));
                let name = widgets::trunc(&s.name, rest);
                let pad = rest.saturating_sub(widgets::width(&name));
                label.push(TSpan::styled(
                    name,
                    if is_err { theme::error().add_modifier(Modifier::BOLD) } else { theme::text() },
                ));
                label.push(TSpan::raw(" ".repeat(pad + 1)));
                let (start, len) = waterfall::bar(row.offset_ns, row.duration_ns, d.total_ns, bar_w as u16);
                let bar_color = if is_err { theme::ERR } else { svc_color };
                label.push(TSpan::raw(" ".repeat(start as usize)));
                label.push(TSpan::styled("█".repeat(len as usize), Style::new().fg(bar_color)));
                label.push(TSpan::raw(" ".repeat(bar_w.saturating_sub((start + len) as usize))));
                label.push(TSpan::styled(format!("{:>dur_w$}", ms(row.duration_ns as f64 / 1e6)), theme::dim()));
                ListItem::new(Line::from(label))
            })
            .collect();
        f.render_stateful_widget(
            List::new(items).highlight_style(theme::selected()).highlight_symbol("▌"),
            list_area,
            &mut ListState::default().with_selected(sel),
        );
        // Span details
        let lines = span_details(d);
        let title = d.selected_span().map(|s| s.name.clone()).unwrap_or_else(|| "Span".into());
        let block = widgets::panel(&widgets::trunc(&title, info_area.width.saturating_sub(6) as usize), false);
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((d.scroll, 0)).block(block), info_area);
    }
}

fn section(title: &str) -> Line<'static> {
    Line::from(vec![
        TSpan::styled("── ", theme::muted()),
        TSpan::styled(title.to_string(), theme::dim().add_modifier(Modifier::BOLD)),
        TSpan::styled(" ──────────────────────────", theme::muted()),
    ])
}

fn kv_line(k: &str, v: &str) -> Line<'static> {
    Line::from(vec![TSpan::styled(format!("  {k} "), theme::dim()), TSpan::styled(v.to_string(), theme::text())])
}

fn span_details(d: &Detail) -> Vec<Line<'static>> {
    let Some(s) = d.selected_span() else {
        return vec![Line::from("no span selected")];
    };
    let trace_start = d.trace.as_ref().and_then(|t| t.spans.iter().map(|s| s.start_time).min()).unwrap_or(s.start_time);
    let offset = (s.start_time - trace_start).num_nanoseconds().unwrap_or(0) as f64 / 1e6;
    let (status, sc) = match s.status_code.as_str() {
        "ERROR" => ("ERROR", theme::ERR),
        "OK" => ("OK", theme::OK),
        _ => ("UNSET", theme::MUTED),
    };
    let mut out = vec![
        Line::from(vec![
            TSpan::styled(format!(" {status} "), theme::badge(sc)),
            TSpan::raw(" "),
            TSpan::styled(format!("● {}", s.service_name), Style::new().fg(theme::service_color(&s.service_name))),
            TSpan::styled(format!("  {}", s.kind.to_lowercase()), theme::muted()),
        ]),
        Line::from(vec![
            TSpan::styled("  duration ", theme::dim()),
            TSpan::styled(ms(s.duration_ns as f64 / 1e6), theme::bold()),
            TSpan::styled(format!("   at +{}", ms(offset)), theme::dim()),
        ]),
    ];
    if !s.status_message.is_empty() {
        out.push(Line::from(TSpan::styled(format!("  {}", s.status_message), theme::error())));
    }
    out.push(kv_line("span  ", &s.span_id));
    if let Some(p) = &s.parent_span_id {
        out.push(kv_line("parent", p));
    }
    out.push(kv_line("start ", &widgets::local(s.start_time, "%Y-%m-%d %H:%M:%S%.6f")));
    if !s.scope_name.is_empty() {
        out.push(kv_line("scope ", &format!("{} {}", s.scope_name, s.scope_version)));
    }
    if !s.attributes.is_empty() {
        out.push(Line::raw(""));
        out.push(section(&format!("Attributes ({})", s.attributes.len())));
        for (k, v) in &s.attributes {
            out.push(kv_line(k, v));
        }
    }
    if !s.events.is_empty() {
        out.push(Line::raw(""));
        out.push(section(&format!("Events ({})", s.events.len())));
        for e in &s.events {
            let at = (e.time - trace_start).num_nanoseconds().unwrap_or(0) as f64 / 1e6;
            let style =
                if e.name == "exception" { theme::error().add_modifier(Modifier::BOLD) } else { theme::accent() };
            out.push(Line::from(vec![
                TSpan::styled(format!("  +{} ", ms(at)), theme::dim()),
                TSpan::styled(e.name.clone(), style),
            ]));
            for (k, v) in &e.attributes {
                if k == "exception.stacktrace" {
                    out.push(Line::from(TSpan::styled(format!("    {k}"), theme::dim())));
                    for l in v.lines() {
                        out.push(Line::from(TSpan::styled(format!("      {l}"), theme::error())));
                    }
                } else {
                    out.push(Line::from(vec![
                        TSpan::styled(format!("    {k} "), theme::dim()),
                        TSpan::styled(v.clone(), theme::text()),
                    ]));
                }
            }
        }
    }
    if !s.links.is_empty() {
        out.push(Line::raw(""));
        out.push(section(&format!("Links ({})", s.links.len())));
        for l in &s.links {
            out.push(kv_line("→", &format!("{} / {}", l.trace_id, l.span_id)));
        }
    }
    let span_logs: Vec<&LogRecord> =
        d.logs.iter().filter(|l| l.span_id.as_deref() == Some(s.span_id.as_str())).collect();
    if !span_logs.is_empty() {
        out.push(Line::raw(""));
        out.push(section(&format!("Logs ({})", span_logs.len())));
        for l in span_logs {
            let (lbl, c) = theme::severity(l.severity_number, &l.severity_text);
            out.push(Line::from(vec![
                TSpan::styled(format!("  {} ", widgets::local(l.time, "%H:%M:%S%.3f")), theme::dim()),
                TSpan::styled(format!("{lbl:<5} "), Style::new().fg(c)),
                TSpan::styled(l.body.clone(), theme::text()),
            ]));
        }
    }
    if !s.resource_attributes.is_empty() {
        out.push(Line::raw(""));
        out.push(section(&format!("Resource ({})", s.resource_attributes.len())));
        for (k, v) in &s.resource_attributes {
            out.push(kv_line(k, v));
        }
    }
    out
}
