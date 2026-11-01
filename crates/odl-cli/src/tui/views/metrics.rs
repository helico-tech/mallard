//! Metrics: searchable metric catalog and a time-series chart with
//! aggregation and group-by.

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
        Embedded, Outcome, PromptTarget, due,
        loader::{Data, Kind},
        nav, theme,
        widgets::{self, Series},
    },
};

#[derive(Default)]
pub struct State {
    pub list: Vec<MetricInfo>,
    pub search: String,
    pub sel: TableState,
    pub error: Option<String>,
    pub refreshed: Option<Instant>,
    pub chart: Option<ChartState>,
}

pub struct ChartState {
    pub info: MetricInfo,
    pub aggs: Vec<&'static str>,
    pub agg: usize,
    pub group_by: String,
    pub result: Option<MetricQueryResult>,
    pub error: Option<String>,
    pub refreshed: Option<Instant>,
}

/// Aggregations offered for a metric type; the first is the server default.
pub fn aggs_for(info: &MetricInfo) -> Vec<&'static str> {
    match info.metric_type.as_str() {
        "gauge" => vec!["avg", "min", "max", "sum", "count"],
        "sum" if info.temporality == "cumulative" || info.temporality == "delta" => {
            vec!["rate", "increase", "avg", "max", "sum"]
        }
        "sum" => vec!["avg", "min", "max", "sum"],
        _ => vec!["p95", "p50", "p90", "p99", "avg", "count", "rate"],
    }
}

impl State {
    pub fn visible(&self) -> Vec<&MetricInfo> {
        let s = self.search.to_lowercase();
        self.list.iter().filter(|m| s.is_empty() || m.name.to_lowercase().contains(&s)).collect()
    }

    pub fn clamp_selection(&mut self) {
        let n = self.visible().len();
        let i = self.sel.selected().unwrap_or(0).min(n.saturating_sub(1));
        self.sel.select((n > 0).then_some(i));
    }

    pub fn set_list(&mut self, v: Vec<MetricInfo>) {
        let keep = self.sel.selected().and_then(|i| self.visible().get(i).map(|m| m.name.clone()));
        self.list = v;
        self.error = None;
        let idx = keep.and_then(|n| self.visible().iter().position(|m| m.name == n));
        self.sel.select(idx.or(Some(0)));
        self.clamp_selection();
    }

    pub fn set_result(&mut self, r: MetricQueryResult) {
        if let Some(c) = &mut self.chart
            && c.info.name == r.name
        {
            c.result = Some(r);
            c.error = None;
        }
    }
}

fn unit_fmt(unit: &str) -> fn(f64) -> String {
    match unit {
        "s" => |v| ms(v * 1000.0),
        "ms" => ms,
        "By" => |v| crate::fmt::bytes(v),
        "1" => |v| {
            if v.abs() <= 1.0 { format!("{:.1}%", v * 100.0) } else { compact(v) }
        },
        _ => compact,
    }
}

/// Value formatter for a chart: counts and rates are plain numbers whatever
/// the metric unit; unit "1" is a percentage only when all values are <= 1.
pub(crate) fn chart_fmt(unit: &str, agg: &str, max_abs: f64) -> Box<dyn Fn(f64) -> String> {
    match agg {
        "count" | "increase" => return Box::new(compact),
        "rate" => return Box::new(|v| format!("{}/s", compact(v))),
        _ => {}
    }
    match unit {
        "1" if max_abs <= 1.0 => Box::new(|v| format!("{:.1}%", v * 100.0)),
        "1" => Box::new(compact),
        other => Box::new(unit_fmt(other)),
    }
}

impl Embedded {
    pub(crate) fn refresh_metrics(&mut self, force: bool) {
        if let Some(c) = &mut self.metrics.chart {
            if !force && !due(c.refreshed, Duration::from_secs(10)) {
                return;
            }
            let q = MetricQuery {
                name: c.info.name.clone(),
                since: Some(self.windows[self.win].clone()),
                agg: Some(c.aggs[c.agg].to_string()),
                group_by: (!c.group_by.trim().is_empty()).then(|| c.group_by.trim().to_string()),
                ..Default::default()
            };
            let cl = self.client.clone();
            if self.loader.load(Kind::MetricQuery, force, async move { cl.metric_query(&q).await }, Data::MetricQuery) {
                c.refreshed = Some(Instant::now());
            }
            return;
        }
        if !force && !due(self.metrics.refreshed, Duration::from_secs(30)) {
            return;
        }
        let q = MetricListQuery { since: Some(self.since()), ..Default::default() };
        let c = self.client.clone();
        if self.loader.load(Kind::MetricList, force, async move { c.metrics(&q).await }, Data::MetricList) {
            self.metrics.refreshed = Some(Instant::now());
        }
    }

    pub(crate) fn set_metric_group_by(&mut self, v: &str) {
        if let Some(c) = &mut self.metrics.chart {
            c.group_by = v.to_string();
            c.result = None;
            c.refreshed = None;
            self.loader.cancel(Kind::MetricQuery);
        }
    }

    pub(crate) fn key_metrics(&mut self, k: &KeyEvent) -> Option<Outcome> {
        if let Some(c) = &mut self.metrics.chart {
            match k.code {
                KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                    self.metrics.chart = None;
                    self.loader.cancel(Kind::MetricQuery);
                }
                KeyCode::Char('a') | KeyCode::Right => {
                    c.agg = (c.agg + 1) % c.aggs.len();
                    c.refreshed = None;
                }
                KeyCode::Char('A') => {
                    c.agg = (c.agg + c.aggs.len() - 1) % c.aggs.len();
                    c.refreshed = None;
                }
                KeyCode::Char('b') => {
                    let v = c.group_by.clone();
                    self.start_prompt(
                        PromptTarget::MetricGroupBy,
                        "group by",
                        "attribute keys, comma separated (e.g. service.name,http.route) · ↵ apply",
                        &v,
                    );
                }
                _ => return None,
            }
            return Some(Outcome::Consumed);
        }
        let n = self.metrics.visible().len();
        if let Some(i) = nav(self.metrics.sel.selected().unwrap_or(0), n, k, 10) {
            self.metrics.sel.select(Some(i));
            return Some(Outcome::Consumed);
        }
        match k.code {
            KeyCode::Char('/' | 'f') => {
                let v = self.metrics.search.clone();
                self.start_prompt(PromptTarget::MetricSearch, "search metrics", "type to filter · ↵ done", &v);
                Some(Outcome::Consumed)
            }
            KeyCode::Esc if !self.metrics.search.is_empty() => {
                self.metrics.search.clear();
                self.metrics.clamp_selection();
                Some(Outcome::Consumed)
            }
            KeyCode::Enter | KeyCode::Right => {
                let info =
                    self.metrics.sel.selected().and_then(|i| self.metrics.visible().get(i).map(|m| (*m).clone()))?;
                let aggs = aggs_for(&info);
                self.metrics.chart = Some(ChartState {
                    info,
                    aggs,
                    agg: 0,
                    group_by: String::new(),
                    result: None,
                    error: None,
                    refreshed: None,
                });
                self.refresh_metrics(true);
                Some(Outcome::Consumed)
            }
            _ => None,
        }
    }

    pub(crate) fn draw_metrics(&mut self, f: &mut Frame, area: Rect) {
        if self.metrics.chart.is_some() {
            self.draw_metric_chart(f, area);
        } else {
            self.draw_metric_list(f, area);
        }
    }

    fn draw_metric_list(&mut self, f: &mut Frame, area: Rect) {
        let m = &mut self.metrics;
        let visible = m.visible();
        let block = widgets::panel_with(
            "Metrics",
            Line::from(format!(" {} of {} ", visible.len(), m.list.len())).style(theme::muted()),
            true,
        );
        let inner = block.inner(area);
        f.render_widget(block, area);
        let [sl, table_area] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
        f.render_widget(
            Line::from(vec![
                Span::styled(" ⌕ ", theme::accent()),
                if m.search.is_empty() {
                    Span::styled("press / to search", theme::muted())
                } else {
                    Span::styled(m.search.clone(), theme::bold())
                },
            ]),
            sl,
        );
        let rows: Vec<Row> = visible
            .iter()
            .map(|mi| {
                let kind = match mi.metric_type.as_str() {
                    "gauge" => ("gauge", theme::CYAN),
                    "sum" => ("counter", theme::OK),
                    "histogram" => ("histogram", theme::ACCENT2),
                    "exponential_histogram" => ("exp hist", theme::ACCENT2),
                    "summary" => ("summary", theme::WARN),
                    other => (other, theme::DIM),
                };
                Row::new(vec![
                    Cell::from(mi.name.clone()).style(theme::text()),
                    Cell::from(Span::styled(kind.0.to_string(), Style::new().fg(kind.1))),
                    Cell::from(mi.unit.clone()).style(theme::dim()),
                    Cell::from(Line::from(compact(mi.series_count as f64)).right_aligned()).style(theme::text()),
                    Cell::from(Line::from(compact(mi.point_count as f64)).right_aligned()).style(theme::dim()),
                    Cell::from(mi.services.join(" ")).style(theme::muted()),
                ])
            })
            .collect();
        let empty = rows.is_empty();
        let header = Row::new(vec![
            Cell::from("metric"),
            Cell::from("type"),
            Cell::from("unit"),
            Cell::from(Line::from("series").right_aligned()),
            Cell::from(Line::from("points").right_aligned()),
            Cell::from("services"),
        ])
        .style(theme::header_row());
        let widths = [
            Constraint::Fill(3),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(7),
            Constraint::Length(8),
            Constraint::Fill(2),
        ];
        let n = rows.len();
        let sel = m.sel.selected();
        widgets::fit_offset(m.sel.offset_mut(), sel, n, table_area.height.saturating_sub(1));
        f.render_stateful_widget(
            Table::new(rows, widths).header(header).row_highlight_style(theme::selected()).highlight_symbol("▌"),
            table_area,
            &mut m.sel,
        );
        if empty {
            let (msg, style) = match &m.error {
                Some(e) => (format!("⚠ {e}"), theme::error()),
                None if m.refreshed.is_some() && !m.list.is_empty() => ("no metric matches".into(), theme::muted()),
                None if m.refreshed.is_some() => ("no metrics in this window".into(), theme::muted()),
                None => ("loading…".into(), theme::muted()),
            };
            widgets::placeholder(f, table_area, &msg, style);
        }
    }

    fn draw_metric_chart(&mut self, f: &mut Frame, area: Rect) {
        let window_secs = self.window_secs();
        let Some(c) = &self.metrics.chart else { return };
        let unit = c.result.as_ref().map(|r| r.unit.clone()).unwrap_or_else(|| c.info.unit.clone());
        let mut chips = vec![Span::styled(" agg ", theme::dim())];
        for (i, a) in c.aggs.iter().enumerate() {
            if i == c.agg {
                chips.push(Span::styled(format!(" {a} "), theme::badge(theme::ACCENT)));
            } else {
                chips.push(Span::styled(format!(" {a} "), theme::muted()));
            }
        }
        chips.push(Span::styled("   by ", theme::dim()));
        chips.push(if c.group_by.is_empty() {
            Span::styled("(none) b to set", theme::muted())
        } else {
            Span::styled(c.group_by.clone(), theme::bold())
        });
        let title = format!(
            "{} · {}{}",
            c.info.name,
            c.info.metric_type,
            if unit.is_empty() { String::new() } else { format!(" · {unit}") }
        );
        let block = widgets::panel(&title, true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let series = c.result.as_ref().map(|r| r.series.as_slice()).unwrap_or(&[]);
        let legend_h = if inner.height > 14 { (series.len().min(8) as u16 + 1).max(2) } else { 0 };
        let [chips_area, chart_area, legend_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(4), Constraint::Length(legend_h)]).areas(inner);
        f.render_widget(Line::from(chips), chips_area);
        if c.result.is_none() {
            let (msg, style) = match &c.error {
                Some(e) => (format!("⚠ {e}"), theme::error()),
                None => ("loading…".into(), theme::muted()),
            };
            widgets::placeholder(f, chart_area, &msg, style);
            return;
        }
        let max_abs = c
            .result
            .iter()
            .flat_map(|r| &r.series)
            .flat_map(|s| &s.points)
            .map(|p| p.value.abs())
            .filter(|v| v.is_finite())
            .fold(0.0, f64::max);
        let fmt = chart_fmt(&unit, c.aggs[c.agg], max_abs);
        let label = |s: &MetricSeries| -> String {
            if s.labels.is_empty() {
                c.aggs[c.agg].to_string()
            } else {
                s.labels.values().cloned().collect::<Vec<_>>().join(" · ")
            }
        };
        let points: Vec<Vec<(f64, f64)>> =
            series.iter().map(|s| s.points.iter().map(|p| (p.time.timestamp() as f64, p.value)).collect()).collect();
        let shown: Vec<Series> = series
            .iter()
            .zip(&points)
            .take(theme::SERIES.len())
            .enumerate()
            .map(|(i, (s, p))| Series { name: widgets::trunc(&label(s), 30), color: theme::SERIES[i], points: p })
            .collect();
        let chart_block = ratatui::widgets::Block::default();
        let now = chrono::Utc::now().timestamp() as f64;
        let bounds = Some([now - window_secs, now]);
        widgets::time_chart(f, chart_area, chart_block, &shown, bounds, &fmt);
        if legend_h > 0 {
            let rows: Vec<Row> = series
                .iter()
                .enumerate()
                .take(8)
                .map(|(i, s)| {
                    let vals: Vec<f64> = s.points.iter().map(|p| p.value).collect();
                    let (mn, mx) = vals.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
                    let avg = if vals.is_empty() { 0.0 } else { vals.iter().sum::<f64>() / vals.len() as f64 };
                    let last = vals.last().copied().unwrap_or(0.0);
                    let color = theme::SERIES.get(i).copied().unwrap_or(theme::DIM);
                    let num = |v: f64| {
                        Cell::from(Line::from(if vals.is_empty() { "–".into() } else { fmt(v) }).right_aligned())
                    };
                    Row::new(vec![
                        Cell::from(Span::styled("━━", Style::new().fg(color))),
                        Cell::from(label(s)).style(theme::text()),
                        num(last).style(theme::bold()),
                        num(mn).style(theme::dim()),
                        num(avg).style(theme::dim()),
                        num(mx).style(theme::dim()),
                    ])
                })
                .collect();
            let header = Row::new(vec![
                Cell::from(""),
                Cell::from(format!("series ({})", series.len())),
                Cell::from(Line::from("last").right_aligned()),
                Cell::from(Line::from("min").right_aligned()),
                Cell::from(Line::from("avg").right_aligned()),
                Cell::from(Line::from("max").right_aligned()),
            ])
            .style(theme::header_row());
            let widths = [
                Constraint::Length(2),
                Constraint::Fill(1),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(10),
            ];
            f.render_widget(Table::new(rows, widths).header(header), legend_area);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(t: &str, temp: &str) -> MetricInfo {
        MetricInfo {
            name: "m".into(),
            metric_type: t.into(),
            unit: String::new(),
            description: String::new(),
            temporality: temp.into(),
            series_count: 1,
            point_count: 1,
            services: vec![],
            last_seen: chrono::Utc::now(),
        }
    }

    #[test]
    fn aggregations() {
        assert_eq!(aggs_for(&info("gauge", ""))[0], "avg");
        assert_eq!(aggs_for(&info("sum", "cumulative"))[0], "rate");
        assert_eq!(aggs_for(&info("histogram", "cumulative"))[0], "p95");
        assert_eq!(unit_fmt("s")(0.25), "250.0ms");
    }
}
