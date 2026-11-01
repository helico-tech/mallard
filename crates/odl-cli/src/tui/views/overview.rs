//! Overview: KPI tiles, throughput/latency charts and the services table
//! (Enter drills into operations, Enter again opens matching traces).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use odl_client::api::*;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Cell, Row, Table, TableState},
};

use crate::{
    fmt::{compact, ms},
    tui::{
        Embedded, Outcome, View, due,
        loader::{Data, Kind},
        nav, theme,
        widgets::{self, Series, Tile},
    },
};

#[derive(Default)]
pub struct State {
    pub services: Vec<ServiceSummary>,
    pub sel: TableState,
    /// Drill-down: (service, operations)
    pub ops: Option<(String, Vec<OperationSummary>)>,
    /// The operations of `ops` arrived (they may be none).
    pub ops_loaded: bool,
    pub ops_sel: TableState,
    pub span_series: Option<SpanSeries>,
    pub log_series: Option<LogSeries>,
    pub error: Option<String>,
    pub refreshed: Option<Instant>,
}

impl State {
    pub fn set_services(&mut self, v: Vec<ServiceSummary>) {
        let keep = self.sel.selected().and_then(|i| self.services.get(i)).map(|s| s.name.clone());
        self.services = v;
        self.error = None;
        let i = keep.and_then(|n| self.services.iter().position(|s| s.name == n)).unwrap_or(0);
        self.sel.select((!self.services.is_empty()).then_some(i.min(self.services.len().saturating_sub(1))));
    }

    pub fn set_operations(&mut self, v: Vec<OperationSummary>) {
        if let Some((_, ops)) = &mut self.ops {
            self.ops_loaded = true;
            let keep = self.ops_sel.selected().and_then(|i| ops.get(i)).map(|o| o.name.clone());
            *ops = v;
            let i = keep.and_then(|n| ops.iter().position(|o| o.name == n)).unwrap_or(0);
            self.ops_sel.select((!ops.is_empty()).then_some(i.min(ops.len().saturating_sub(1))));
        }
    }
}

fn red_cells(r: &RedStats) -> Vec<Cell<'static>> {
    let ec = theme::error_rate_color(r.error_rate);
    vec![
        Cell::from(Line::from(format!("{}/s", compact(r.rate_per_sec))).right_aligned()).style(theme::text()),
        Cell::from(Line::from(format!("{:.2}%", r.error_rate * 100.0)).right_aligned()).style(Style::new().fg(ec)),
        Cell::from(Line::from(ms(r.p50_ms)).right_aligned()).style(theme::dim()),
        Cell::from(Line::from(ms(r.p95_ms)).right_aligned()).style(theme::text()),
        Cell::from(Line::from(ms(r.p99_ms)).right_aligned()).style(theme::dim()),
        Cell::from(Line::from(compact(r.span_count as f64)).right_aligned()).style(theme::muted()),
    ]
}

impl Embedded {
    pub(crate) fn refresh_overview(&mut self, force: bool) {
        if !force && !due(self.overview.refreshed, Duration::from_secs(5)) {
            return;
        }
        let since = Some(self.since().to_string());
        let c = self.client.clone();
        let q = ServiceQuery { since: since.clone(), ..Default::default() };
        if !self.loader.load(Kind::Services, force, async move { c.services(&q).await }, Data::Services) {
            return;
        }
        self.overview.refreshed = Some(Instant::now());
        let c = self.client.clone();
        let q = SpanSeriesQuery { since: since.clone(), ..Default::default() };
        self.loader.load(Kind::SpanSeries, force, async move { c.span_series(&q).await }, Data::SpanSeries);
        let c = self.client.clone();
        let q = LogSeriesQuery { since: since.clone(), ..Default::default() };
        self.loader.load(Kind::LogSeries, force, async move { c.log_series(&q).await }, Data::LogSeries);
        if let Some((svc, _)) = &self.overview.ops {
            let c = self.client.clone();
            let q = OperationQuery { since, service: Some(svc.clone()), ..Default::default() };
            self.loader.load(Kind::Operations, force, async move { c.operations(&q).await }, Data::Operations);
        }
    }

    pub(crate) fn key_overview(&mut self, k: &KeyEvent) -> Option<Outcome> {
        let o = &mut self.overview;
        if let Some((svc, ops)) = &o.ops {
            if let Some(i) = nav(o.ops_sel.selected().unwrap_or(0), ops.len(), k, 10) {
                o.ops_sel.select(Some(i));
                return Some(Outcome::Consumed);
            }
            match k.code {
                KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                    o.ops = None;
                    return Some(Outcome::Consumed);
                }
                KeyCode::Enter => {
                    let svc = svc.clone();
                    let op = o.ops_sel.selected().and_then(|i| ops.get(i)).map(|op| op.name.clone());
                    self.show_traces_for(Some(svc), op);
                    return Some(Outcome::Consumed);
                }
                _ => return None,
            }
        }
        if let Some(i) = nav(o.sel.selected().unwrap_or(0), o.services.len(), k, 10) {
            o.sel.select(Some(i));
            return Some(Outcome::Consumed);
        }
        match k.code {
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                let svc = o.sel.selected().and_then(|i| o.services.get(i)).map(|s| s.name.clone())?;
                o.ops = Some((svc, Vec::new()));
                o.ops_loaded = false;
                o.ops_sel.select(Some(0));
                self.refresh_overview(true);
                Some(Outcome::Consumed)
            }
            _ => None,
        }
    }

    fn show_traces_for(&mut self, service: Option<String>, operation: Option<String>) {
        self.traces.show_for(crate::tui::query::TraceFilter { service, operation, ..Default::default() });
        // A pending list for the old filter must not be applied.
        self.loader.cancel(Kind::Traces);
        self.view = View::Traces;
    }

    pub(crate) fn draw_overview(&mut self, f: &mut Frame, area: Rect) {
        let charts_h = if area.height >= 24 { (area.height * 35 / 100).max(8) } else { 0 };
        let tiles_h = if area.height >= 14 { 5 } else { 0 };
        let [tiles, charts, table] =
            Layout::vertical([Constraint::Length(tiles_h), Constraint::Length(charts_h), Constraint::Min(4)])
                .areas(area);
        if tiles_h > 0 {
            self.draw_tiles(f, tiles);
        }
        if charts_h > 0 {
            self.draw_charts(f, charts);
        }
        self.draw_services(f, table);
    }

    fn draw_tiles(&self, f: &mut Frame, area: Rect) {
        let secs = self.window_secs();
        let o = &self.overview;
        let pts = o.span_series.as_ref().map(|s| s.points.as_slice()).unwrap_or(&[]);
        let count: u64 = pts.iter().map(|p| p.count).sum();
        let errors: u64 = pts.iter().map(|p| p.errors).sum();
        let err_rate = if count > 0 { errors as f64 / count as f64 } else { 0.0 };
        let p95 =
            if count > 0 { pts.iter().map(|p| p.p95_ms * p.count as f64).sum::<f64>() / count as f64 } else { 0.0 };
        let req_spark: Vec<u64> = pts.iter().map(|p| p.count).collect();
        let err_spark: Vec<u64> = pts.iter().map(|p| p.errors).collect();
        let lat_spark: Vec<u64> = pts.iter().map(|p| (p.p95_ms * 1000.0) as u64).collect();
        let lpts = o.log_series.as_ref().map(|s| s.points.as_slice()).unwrap_or(&[]);
        let logs: u64 = lpts.iter().map(|p| p.total).sum();
        let log_errs: u64 = lpts.iter().map(|p| p.error).sum();
        let log_spark: Vec<u64> = lpts.iter().map(|p| p.total).collect();
        let (buf_val, buf_sub, buf_color) = match &self.stats {
            Some(s) => {
                let fill = s.ingest.buffered_bytes as f64 / s.ingest.max_buffer_bytes.max(1) as f64;
                let c = if fill > 0.8 {
                    theme::ERR
                } else if fill > 0.5 {
                    theme::WARN
                } else {
                    theme::OK
                };
                (
                    format!("{:.0}%", fill * 100.0),
                    if s.ingest.flush_errors > 0 {
                        format!("{} flush errors", s.ingest.flush_errors)
                    } else {
                        format!("{} flushes", compact(s.ingest.flushes as f64))
                    },
                    c,
                )
            }
            None => ("–".into(), String::new(), theme::MUTED),
        };
        let cols = if area.width >= 100 { 5 } else { 4 };
        let areas = Layout::horizontal(vec![Constraint::Ratio(1, cols); cols as usize]).split(area);
        let has_data = o.span_series.is_some();
        let dash = |s: String| if has_data { s } else { "–".to_string() };
        Tile {
            label: "requests",
            value: dash(format!("{}/s", compact(count as f64 / secs))),
            color: theme::ACCENT,
            sub: format!("{} total", compact(count as f64)),
            spark: &req_spark,
        }
        .render(f, areas[0]);
        Tile {
            label: "errors",
            value: dash(format!("{:.2}%", err_rate * 100.0)),
            color: theme::error_rate_color(err_rate),
            sub: format!("{} failed", compact(errors as f64)),
            spark: &err_spark,
        }
        .render(f, areas[1]);
        Tile {
            label: "p95 latency",
            value: dash(ms(p95)),
            color: theme::WARN,
            sub: "avg of per-bucket p95".into(),
            spark: &lat_spark,
        }
        .render(f, areas[2]);
        Tile {
            label: "logs",
            value: if o.log_series.is_some() { format!("{}/s", compact(logs as f64 / secs)) } else { "–".into() },
            color: theme::CYAN,
            sub: format!("{} errors", compact(log_errs as f64)),
            spark: &log_spark,
        }
        .render(f, areas[3]);
        if cols == 5 {
            Tile { label: "ingest buffer", value: buf_val, color: buf_color, sub: buf_sub, spark: &[] }
                .render(f, areas[4]);
        }
    }

    fn draw_charts(&self, f: &mut Frame, area: Rect) {
        let [left, right] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area);
        let pts = self.overview.span_series.as_ref();
        let step = pts.map(|s| s.step_secs.max(1) as f64).unwrap_or(60.0);
        let to_xy = |sel: &dyn Fn(&SpanSeriesPoint) -> f64| -> Vec<(f64, f64)> {
            pts.map(|s| s.points.iter().map(|p| (p.time.timestamp() as f64, sel(p))).collect()).unwrap_or_default()
        };
        let rate = to_xy(&|p| p.count as f64 / step);
        let errs = to_xy(&|p| p.errors as f64 / step);
        let with_data = |v: Vec<(f64, f64)>| -> Vec<(f64, f64)> {
            // Latency is undefined in empty buckets; skip them.
            v.into_iter().filter(|(_, y)| *y > 0.0).collect()
        };
        let p50 = with_data(to_xy(&|p| p.p50_ms));
        let p95 = with_data(to_xy(&|p| p.p95_ms));
        let p99 = with_data(to_xy(&|p| p.p99_ms));
        // The server returns every bucket of the window; use them as the time axis.
        let bounds = pts
            .and_then(|s| Some([s.points.first()?.time.timestamp() as f64, s.points.last()?.time.timestamp() as f64]));
        let throughput = [
            Series { name: "requests/s".into(), color: theme::ACCENT, points: &rate },
            Series { name: "errors/s".into(), color: theme::ERR, points: &errs },
        ];
        widgets::time_chart(
            f,
            left,
            widgets::panel_with("Throughput", widgets::legend(&throughput), false),
            &throughput,
            bounds,
            &compact,
        );
        let latency = [
            Series { name: "p50".into(), color: theme::OK, points: &p50 },
            Series { name: "p95".into(), color: theme::WARN, points: &p95 },
            Series { name: "p99".into(), color: theme::ERR, points: &p99 },
        ];
        widgets::time_chart(
            f,
            right,
            widgets::panel_with("Latency", widgets::legend(&latency), false),
            &latency,
            bounds,
            &ms,
        );
    }

    fn draw_services(&mut self, f: &mut Frame, area: Rect) {
        let o = &mut self.overview;
        let header =
            |first: &[&'static str]| {
                let mut h: Vec<&str> = first.to_vec();
                h.extend(["rate", "errors", "p50", "p95", "p99", "spans"]);
                Row::new(h.into_iter().enumerate().map(|(i, t)| {
                    if i < first.len() { Cell::from(t) } else { Cell::from(Line::from(t).right_aligned()) }
                }))
                .style(theme::header_row())
            };
        let red_w = [10, 9, 9, 9, 9, 8].map(Constraint::Length);
        if let Some((svc, ops)) = &o.ops {
            let rows: Vec<Row> = ops
                .iter()
                .map(|op| {
                    let mut cells = vec![
                        Cell::from(op.name.clone()).style(theme::text()),
                        Cell::from(op.kind.as_str().to_lowercase()).style(theme::muted()),
                    ];
                    cells.extend(red_cells(&op.red));
                    Row::new(cells)
                })
                .collect();
            let mut widths = vec![Constraint::Fill(1), Constraint::Length(9)];
            widths.extend(red_w);
            let title = Line::from(vec![
                Span::styled(" Operations · ", theme::title()),
                Span::styled(format!("● {svc} "), Style::new().fg(theme::service_color(svc))),
            ]);
            let block = widgets::panel_with("", Line::from(format!(" {} ", ops.len())).style(theme::muted()), true)
                .title(title);
            let empty = rows.is_empty();
            let inner = block.inner(area);
            let sel = o.ops_sel.selected();
            widgets::fit_offset(o.ops_sel.offset_mut(), sel, ops.len(), inner.height.saturating_sub(1));
            f.render_stateful_widget(
                Table::new(rows, widths)
                    .header(header(&["operation", "kind"]))
                    .block(block)
                    .row_highlight_style(theme::selected())
                    .highlight_symbol("▌"),
                area,
                &mut o.ops_sel,
            );
            if empty {
                let (msg, style) = match &o.error {
                    Some(e) => (format!("⚠ {e}"), theme::error()),
                    None if o.ops_loaded => ("no operations in this window".into(), theme::muted()),
                    None => ("loading operations…".into(), theme::muted()),
                };
                widgets::placeholder(f, inner, &msg, style);
            }
            return;
        }
        let rows: Vec<Row> = o
            .services
            .iter()
            .map(|s| {
                let mut cells = vec![Cell::from(Line::from(vec![
                    Span::styled("● ", Style::new().fg(theme::service_color(&s.name))),
                    Span::styled(s.name.clone(), theme::text()),
                ]))];
                cells.extend(red_cells(&s.red));
                Row::new(cells)
            })
            .collect();
        let mut widths = vec![Constraint::Fill(1)];
        widths.extend(red_w);
        let block = widgets::panel_with(
            "Services",
            Line::from(format!(" {} services ", o.services.len())).style(theme::muted()),
            true,
        );
        let inner = block.inner(area);
        let empty = rows.is_empty();
        let sel = o.sel.selected();
        widgets::fit_offset(o.sel.offset_mut(), sel, o.services.len(), inner.height.saturating_sub(1));
        f.render_stateful_widget(
            Table::new(rows, widths)
                .header(header(&["service"]))
                .block(block)
                .row_highlight_style(theme::selected())
                .highlight_symbol("▌"),
            area,
            &mut o.sel,
        );
        if empty {
            let (msg, style) = match &o.error {
                Some(e) => (format!("⚠ {e}"), theme::error()),
                None if o.refreshed.is_some() => ("no spans in this window".to_string(), theme::muted()),
                None => ("loading…".to_string(), theme::muted()),
            };
            widgets::placeholder(f, inner, &msg, style);
        }
    }
}
