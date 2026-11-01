//! Logs: live tail with severity histogram, filter and record details.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{TimeDelta, Utc};
use crossterm::event::{KeyCode, KeyEvent};
use mallard_term::tui::keys;
use odl_client::{LogTail, api::*};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Cell, Clear, Paragraph, Row, Table, TableState, Wrap},
};

use crate::{
    fmt::compact,
    tui::{
        Embedded, Outcome, PromptTarget, due,
        loader::{Data, Kind},
        nav,
        query::{LogFilter, parse_log_filter, tokenize},
        theme, widgets,
    },
};

const MAX_RECORDS: usize = 5000;

pub struct State {
    pub query: String,
    pub filter: LogFilter,
    pub records: Vec<LogRow>,
    pub sel: TableState,
    pub follow: bool,
    pub series: Option<LogSeries>,
    pub error: Option<String>,
    pub refreshed: Option<Instant>,
    pub loaded: bool,
    pub requested: bool,
    pub tail: Option<Arc<tokio::sync::Mutex<LogTail>>>,
    pub tail_at: Option<Instant>,
    /// Retry a failed initial load after this time.
    pub retry_at: Option<Instant>,
    /// Detail popup scroll offset when open.
    pub detail: Option<u16>,
}

/// A row of the log list.
#[allow(clippy::large_enum_variant, reason = "gaps are rare; boxing every record would cost more")]
pub enum LogRow {
    Record(LogRecord),
    /// The tail may have skipped records before this time (pause, burst).
    Gap(chrono::DateTime<Utc>),
}

impl LogRow {
    pub fn record(&self) -> Option<&LogRecord> {
        match self {
            LogRow::Record(r) => Some(r),
            LogRow::Gap(_) => None,
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            query: String::new(),
            filter: LogFilter::default(),
            records: Vec::new(),
            sel: TableState::default(),
            follow: true,
            series: None,
            error: None,
            refreshed: None,
            loaded: false,
            requested: false,
            tail: None,
            tail_at: None,
            retry_at: None,
            detail: None,
        }
    }
}

/// Next severity filter in the cycle all → info → warn → error → all,
/// rewriting the `level=` token of the query.
pub fn cycle_level(query: &str) -> (String, Option<u8>) {
    let toks = tokenize(query);
    let is_level = |t: &str| t.split_once('=').is_some_and(|(k, _)| matches!(k, "level" | "lvl" | "severity" | "sev"));
    let cur = toks.iter().find(|t| is_level(t)).and_then(|t| crate::severity::parse(t.split_once('=').unwrap().1));
    let next = match cur {
        None => Some(("info", 9)),
        Some(n) if n < 13 => Some(("warn", 13)),
        Some(n) if n < 17 => Some(("error", 17)),
        Some(_) => None,
    };
    let mut rest: Vec<String> =
        toks.into_iter().filter(|t| !is_level(t)).map(|t| if t.contains(' ') { quote(&t) } else { t }).collect();
    if let Some((name, _)) = next {
        rest.insert(0, format!("level={name}"));
    }
    (rest.join(" "), next.map(|(_, n)| n))
}

fn quote(t: &str) -> String {
    match t.split_once('=') {
        Some((k, v)) => format!("{k}=\"{v}\""),
        None => format!("\"{t}\""),
    }
}

impl State {
    pub fn reset(&mut self) {
        self.records.clear();
        self.loaded = false;
        self.requested = false;
        self.retry_at = None;
        self.tail = None;
        self.tail_at = None;
        self.refreshed = None;
        self.sel.select(None);
        self.detail = None;
        self.error = None;
    }

    fn base_query(&self, since: String) -> LogQuery {
        LogQuery {
            since: Some(since),
            service: self.filter.service.clone(),
            min_severity: self.filter.level,
            q: self.filter.text.clone(),
            trace_id: self.filter.trace.clone(),
            attributes: crate::attrs(&self.filter.attrs),
            ..Default::default()
        }
    }

    fn at_end(&self) -> bool {
        self.sel.selected().is_none_or(|i| i + 1 >= self.records.len())
    }

    /// Append new records (the tail already removed duplicates). `gap`:
    /// records may have been skipped before these.
    fn append(&mut self, v: Vec<LogRecord>, gap: bool) {
        let stick = self.follow && self.at_end();
        if gap
            && let Some(first) = v.first()
            && !self.records.is_empty()
        {
            self.records.push(LogRow::Gap(first.time));
        }
        self.records.extend(v.into_iter().map(LogRow::Record));
        if self.records.len() > MAX_RECORDS {
            let drop = self.records.len() - MAX_RECORDS;
            self.records.drain(..drop);
            if let Some(i) = self.sel.selected() {
                self.sel.select(Some(i.saturating_sub(drop)));
            }
        }
        if stick && !self.records.is_empty() {
            self.sel.select(Some(self.records.len() - 1));
        }
    }
}

impl Embedded {
    pub(crate) fn refresh_logs(&mut self) {
        let since = self.since().to_string();
        if !self.logs.requested && self.logs.retry_at.is_none_or(|t| Instant::now() >= t) {
            let mut q = self.logs.base_query(since.clone());
            q.limit = Some(1000);
            q.order = Some(LogOrder::Desc);
            let c = self.client.clone();
            self.loader.cancel(Kind::LogTail);
            if self.loader.spawn(Kind::Logs, async move { c.logs(&q).await }, Data::Logs) {
                self.logs.requested = true;
            }
        }
        if due(self.logs.refreshed, Duration::from_secs(5)) {
            let q = LogSeriesQuery {
                since: Some(since),
                service: self.logs.filter.service.clone(),
                q: self.logs.filter.text.clone(),
                ..Default::default()
            };
            let c = self.client.clone();
            if self.loader.load(Kind::LogVolume, false, async move { c.log_series(&q).await }, Data::LogSeries) {
                self.logs.refreshed = Some(Instant::now());
            }
        }
        if self.logs.loaded
            && self.logs.follow
            && due(self.logs.tail_at, Duration::from_secs(2))
            && !self.loader.is_pending(Kind::LogTail)
            && let Some(tail) = self.logs.tail.clone()
            && self.loader.spawn(
                Kind::LogTail,
                async move {
                    let mut t = tail.lock().await;
                    let records = t.poll().await?;
                    Ok((records, t.last_poll_truncated()))
                },
                |(records, truncated)| Data::Tail { records, truncated },
            )
        {
            self.logs.tail_at = Some(Instant::now());
        }
    }

    pub(crate) fn logs_loaded(&mut self, mut v: Vec<LogRecord>) {
        v.reverse();
        let l = &mut self.logs;
        l.records.clear();
        l.loaded = true;
        l.error = None;
        l.retry_at = None;
        let start = v.last().map(|r| r.time).unwrap_or_else(|| Utc::now() - TimeDelta::seconds(5));
        l.follow = true;
        let q = l.base_query(String::new());
        let q = LogQuery { since: None, ..q };
        let mut tail = LogTail::new(self.client.clone(), q, start);
        // The tail must not return what the initial load already shows.
        tail.mark_seen(&v);
        l.append(v, false);
        l.sel.select(l.records.len().checked_sub(1));
        l.tail = Some(Arc::new(tokio::sync::Mutex::new(tail)));
        l.tail_at = Some(Instant::now());
    }

    pub(crate) fn logs_tail_loaded(&mut self, v: Vec<LogRecord>, gap: bool) {
        self.logs.error = None;
        if self.logs.loaded {
            self.logs.append(v, gap);
        }
    }

    pub(crate) fn set_log_filter(&mut self, q: &str) {
        match parse_log_filter(q) {
            Ok(f) => {
                self.logs.query = q.to_string();
                self.logs.filter = f;
                self.logs.reset();
                self.loader.cancel(Kind::Logs);
                self.loader.cancel(Kind::LogTail);
                self.loader.cancel(Kind::LogVolume);
            }
            Err(e) => self.flash_error(e),
        }
    }

    fn selected_log(&self) -> Option<&LogRecord> {
        self.logs.sel.selected().and_then(|i| self.logs.records.get(i)).and_then(LogRow::record)
    }

    pub(crate) fn key_logs(&mut self, k: &KeyEvent) -> Option<Outcome> {
        if let Some(scroll) = self.logs.detail {
            match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.logs.detail = None,
                KeyCode::Down | KeyCode::Char('j') => self.logs.detail = Some(scroll.saturating_add(1)),
                KeyCode::Up | KeyCode::Char('k') => self.logs.detail = Some(scroll.saturating_sub(1)),
                KeyCode::PageDown => self.logs.detail = Some(scroll.saturating_add(10)),
                KeyCode::PageUp => self.logs.detail = Some(scroll.saturating_sub(10)),
                KeyCode::Char('o' | 't') => match self.selected_log().and_then(|l| l.trace_id.clone()) {
                    Some(id) => {
                        self.open_trace(&id);
                    }
                    None => self.flash_error("this record has no trace id"),
                },
                _ => {}
            }
            return Some(Outcome::Consumed);
        }
        if let Some(i) = nav(self.logs.sel.selected().unwrap_or(0), self.logs.records.len(), k, 20) {
            self.logs.sel.select(Some(i));
            return Some(Outcome::Consumed);
        }
        match k.code {
            KeyCode::Char(keys::SEARCH | 'f') => {
                let v = self.logs.query.clone();
                self.start_prompt(
                    PromptTarget::LogFilter,
                    "filter logs",
                    "service= level=warn trace= words · ↵ apply · esc cancel",
                    &v,
                );
                Some(Outcome::Consumed)
            }
            KeyCode::Char('s') => {
                let (q, lvl) = cycle_level(&self.logs.query);
                self.set_log_filter(&q);
                let name = match lvl {
                    Some(9) => "info and above",
                    Some(13) => "warnings and errors",
                    Some(_) => "errors only",
                    None => "all severities",
                };
                self.flash(format!("severity: {name}"));
                Some(Outcome::Consumed)
            }
            KeyCode::Char(' ' | 'F') => {
                self.logs.follow = !self.logs.follow;
                if self.logs.follow {
                    self.logs.tail_at = None;
                    self.logs.sel.select(self.logs.records.len().checked_sub(1));
                    self.flash("following new logs");
                } else {
                    self.flash("paused");
                }
                Some(Outcome::Consumed)
            }
            KeyCode::Enter => {
                if self.selected_log().is_some() {
                    self.logs.detail = Some(0);
                }
                Some(Outcome::Consumed)
            }
            KeyCode::Char('o') => {
                match self.selected_log().and_then(|l| l.trace_id.clone()) {
                    Some(id) => {
                        self.open_trace(&id);
                    }
                    None => self.flash_error("this record has no trace id"),
                }
                Some(Outcome::Consumed)
            }
            KeyCode::Char('c') => {
                self.set_log_filter("");
                Some(Outcome::Consumed)
            }
            KeyCode::Esc if !self.logs.query.is_empty() => {
                self.set_log_filter("");
                Some(Outcome::Consumed)
            }
            _ => None,
        }
    }

    pub(crate) fn draw_logs(&mut self, f: &mut Frame, area: Rect) {
        let hist_h = if area.height >= 18 { 7 } else { 0 };
        let [hist, list] = Layout::vertical([Constraint::Length(hist_h), Constraint::Min(3)]).areas(area);
        if hist_h > 0 {
            self.draw_log_histogram(f, hist);
        }
        self.draw_log_list(f, list);
        if self.logs.detail.is_some() {
            self.draw_log_detail(f, area);
        }
    }

    fn draw_log_histogram(&self, f: &mut Frame, area: Rect) {
        let pts = self.logs.series.as_ref().map(|s| s.points.as_slice()).unwrap_or(&[]);
        let sum = |g: fn(&LogSeriesPoint) -> u64| pts.iter().map(g).sum::<u64>();
        let legend = Line::from(vec![
            Span::styled(" ■ ", Style::new().fg(theme::RED)),
            Span::styled(format!("error {} ", compact(sum(|p| p.error) as f64)), theme::dim()),
            Span::styled(" ■ ", Style::new().fg(theme::YELLOW)),
            Span::styled(format!("warn {} ", compact(sum(|p| p.warn) as f64)), theme::dim()),
            Span::styled(" ■ ", Style::new().fg(theme::CYAN)),
            Span::styled(format!("info {} ", compact(sum(|p| p.info) as f64)), theme::dim()),
            Span::styled(" ■ ", Style::new().fg(theme::FAINT)),
            Span::styled(format!("debug {} ", compact(sum(|p| p.debug) as f64)), theme::dim()),
        ]);
        // The volume query has no trace, level or attribute filter.
        let lf = &self.logs.filter;
        let partial = lf.level.is_some() || lf.trace.is_some() || !lf.attrs.is_empty();
        let title = if partial { "Volume · service and text filter only" } else { "Volume" };
        let block = widgets::panel_with(title, legend, false);
        let inner = block.inner(area);
        f.render_widget(block, area);
        if pts.is_empty() {
            widgets::placeholder(f, inner, "no logs in this window", theme::faint());
            return;
        }
        let buckets: Vec<Vec<u64>> = pts.iter().map(|p| vec![p.error, p.warn, p.info, p.debug]).collect();
        let [bars, axis] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
        f.render_widget(
            widgets::StackedBars { buckets: &buckets, colors: &[theme::RED, theme::YELLOW, theme::CYAN, theme::FAINT] },
            bars,
        );
        let (a, b) = (pts.first().unwrap().time, pts.last().unwrap().time);
        let labels = widgets::time_labels(a.timestamp() as f64, b.timestamp() as f64);
        let w = axis.width as usize;
        let mid = &labels[1];
        let left = &labels[0];
        let right = &labels[2];
        let gap1 = (w / 2).saturating_sub(left.chars().count() + mid.chars().count() / 2);
        let used = left.chars().count() + gap1 + mid.chars().count();
        let gap2 = w.saturating_sub(used + right.chars().count());
        f.render_widget(
            Span::styled(format!("{left}{}{mid}{}{right}", " ".repeat(gap1), " ".repeat(gap2)), theme::faint()),
            axis,
        );
    }

    fn draw_log_list(&mut self, f: &mut Frame, area: Rect) {
        let long = self.window_secs() > 86_400.0;
        let l = &mut self.logs;
        let state = if l.follow {
            Span::styled(" ● following ", Style::new().fg(theme::GREEN))
        } else {
            Span::styled(" ‖ paused ", Style::new().fg(theme::YELLOW))
        };
        let right = Line::from(vec![state, Span::styled(format!("· {} records ", l.records.len()), theme::faint())]);
        let block = widgets::panel_with("Logs", right, true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let [fl, table_area] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
        let filter_line = Line::from(vec![
            Span::styled(" ⌕ ", theme::accent()),
            if l.query.is_empty() {
                Span::styled("all logs · press / to filter, s for severity", theme::faint())
            } else {
                Span::styled(l.query.clone(), theme::bold())
            },
        ]);
        f.render_widget(filter_line, fl);
        // Build rows only for what fits: the list holds up to 5000 records.
        let sel = l.sel.selected();
        let (shown, sel) = widgets::visible(l.sel.offset_mut(), sel, l.records.len(), table_area.height);
        let shown = &l.records[shown];
        let time_fmt = if long { "%m-%d %H:%M:%S" } else { "%H:%M:%S%.3f" };
        let rows: Vec<Row> = shown
            .iter()
            .map(|row| {
                let r = match row {
                    LogRow::Record(r) => r,
                    LogRow::Gap(t) => {
                        return Row::new(vec![
                            Cell::from(widgets::utc(*t, time_fmt)).style(theme::faint()),
                            Cell::from(""),
                            Cell::from(""),
                            Cell::from(Span::styled(
                                "… older records skipped (paused or too many to show) …",
                                theme::faint(),
                            )),
                        ]);
                    }
                };
                let (lbl, c) = theme::severity(r.severity_number, &r.severity_text);
                let body_style = if r.severity_number >= 17 { Style::new().fg(theme::RED) } else { theme::text() };
                let mut body = vec![];
                if r.trace_id.is_some() {
                    body.push(Span::styled("◆ ", theme::faint()));
                }
                body.push(Span::styled(widgets::trunc(&r.body, 400), body_style));
                Row::new(vec![
                    Cell::from(widgets::utc(r.time, time_fmt)).style(theme::dim()),
                    Cell::from(Span::styled(format!("{lbl:<5}"), Style::new().fg(c).add_modifier(Modifier::BOLD))),
                    Cell::from(Span::styled(
                        widgets::trunc(&r.service_name, 20),
                        Style::new().fg(theme::service_color(&r.service_name)),
                    )),
                    Cell::from(Line::from(body)),
                ])
            })
            .collect();
        let empty = rows.is_empty();
        let svc_w = shown
            .iter()
            .filter_map(LogRow::record)
            .map(|r| widgets::width(&r.service_name))
            .max()
            .unwrap_or(8)
            .clamp(7, 20) as u16;
        let widths = [
            Constraint::Length(if long { 14 } else { 12 }),
            Constraint::Length(5),
            Constraint::Length(svc_w),
            Constraint::Fill(1),
        ];
        f.render_stateful_widget(
            Table::new(rows, widths).row_highlight_style(theme::selected()).highlight_symbol("▌"),
            table_area,
            &mut TableState::default().with_selected(sel),
        );
        if empty {
            let (msg, style) = match &l.error {
                Some(e) => (format!("⚠ {e}"), theme::error()),
                None if l.loaded => ("no logs match · waiting for new records…".into(), theme::faint()),
                None => ("loading…".into(), theme::faint()),
            };
            widgets::placeholder(f, table_area, &msg, style);
        }
    }

    fn draw_log_detail(&self, f: &mut Frame, area: Rect) {
        let Some(r) = self.selected_log() else { return };
        let w = (area.width as u32 * 9 / 10) as u16;
        let h = (area.height as u32 * 85 / 100) as u16;
        let rect = widgets::centered(area, w, h);
        let (lbl, c) = theme::severity(r.severity_number, &r.severity_text);
        let kv = |k: &str, v: String| {
            Line::from(vec![Span::styled(format!("{k:>10}  "), theme::dim()), Span::styled(v, theme::text())])
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled(format!(" {lbl} "), theme::badge(c)),
                Span::raw("  "),
                Span::styled(format!("● {}", r.service_name), Style::new().fg(theme::service_color(&r.service_name))),
                Span::styled(format!("  {}", widgets::utc(r.time, "%Y-%m-%d %H:%M:%S%.6f")), theme::dim()),
            ]),
            Line::raw(""),
        ];
        for l in r.body.lines() {
            lines.push(Line::from(Span::styled(l.to_string(), theme::bold())));
        }
        lines.push(Line::raw(""));
        if let Some(t) = &r.trace_id {
            lines.push(Line::from(vec![
                Span::styled(format!("{:>10}  ", "trace"), theme::dim()),
                Span::styled(t.clone(), theme::accent()),
                Span::styled("  (o to open)", theme::faint()),
            ]));
        }
        if let Some(s) = &r.span_id {
            lines.push(kv("span", s.clone()));
        }
        lines.push(kv("severity", format!("{} ({})", r.severity_text, r.severity_number)));
        lines.push(kv("observed", widgets::utc(r.observed_time, "%Y-%m-%d %H:%M:%S%.6f")));
        if !r.event_name.is_empty() {
            lines.push(kv("event", r.event_name.clone()));
        }
        if !r.scope_name.is_empty() {
            lines.push(kv("scope", r.scope_name.clone()));
        }
        for (title, attrs) in [("Attributes", &r.attributes), ("Resource", &r.resource_attributes)] {
            if attrs.is_empty() {
                continue;
            }
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(format!("── {title} ({}) ──", attrs.len()), theme::dim())));
            for (k, v) in attrs {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {k} "), theme::dim()),
                    Span::styled(v.clone(), theme::text()),
                ]));
            }
        }
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((self.logs.detail.unwrap_or(0), 0)).block(
                widgets::panel("Log record", true)
                    .title_bottom(Line::from(" esc close · o open trace ").right_aligned()),
            ),
            rect,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_cycle() {
        let (q, l) = cycle_level("service=api boom");
        assert_eq!((q.as_str(), l), ("level=info service=api boom", Some(9)));
        let (q, l) = cycle_level(&q);
        assert_eq!((q.as_str(), l), ("level=warn service=api boom", Some(13)));
        let (q, l) = cycle_level(&q);
        assert_eq!(l, Some(17));
        let (q, l) = cycle_level(&q);
        assert_eq!((q.as_str(), l), ("service=api boom", None));
        let (q, _) = cycle_level(r#"op="a b""#);
        assert_eq!(q, r#"level=info op="a b""#);
    }
}
