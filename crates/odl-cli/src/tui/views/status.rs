//! Status: ingest counters, buffer, flushes, storage and maintenance.

use odl_client::api::Stats;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Cell, Gauge, Paragraph, Row, Table},
};

use crate::{
    fmt::{bytes, compact, ms, uptime},
    tui::{Embedded, theme, widgets},
};

impl Embedded {
    pub(crate) fn draw_status(&mut self, f: &mut Frame, area: Rect) {
        let Some(s) = &self.stats else {
            let block = widgets::panel("Status", true);
            let inner = block.inner(area);
            f.render_widget(block, area);
            let (msg, style) = match &self.stats_error {
                Some(e) => (format!("⚠ {e}"), theme::error()),
                None => ("loading…".to_string(), theme::muted()),
            };
            widgets::placeholder(f, inner, &msg, style);
            return;
        };
        let wide = area.width >= 100;
        let [left, right] = if wide {
            Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area)
        } else {
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area)
        };
        draw_ingest(f, left, s, self.ingest_rate);
        draw_storage(f, right, s);
    }
}

fn draw_ingest(f: &mut Frame, area: Rect, s: &Stats, rate: Option<f64>) {
    let i = &s.ingest;
    let right = Line::from(match rate {
        Some(r) => format!(" {}/s ", compact(r)),
        None => " ".into(),
    })
    .style(theme::dim());
    let block = widgets::panel_with("Ingest", right, true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [table_area, gauge_area, info_area] =
        Layout::vertical([Constraint::Length(5), Constraint::Length(2), Constraint::Min(0)]).areas(inner);
    let rows: Vec<Row> = i
        .signals
        .iter()
        .map(|sig| {
            Row::new(vec![
                Cell::from(sig.signal.as_str()).style(theme::text()),
                Cell::from(Line::from(compact(sig.requests as f64)).right_aligned()).style(theme::dim()),
                Cell::from(Line::from(compact(sig.accepted as f64)).right_aligned()).style(theme::bold()),
                Cell::from(Line::from(compact(sig.rejected as f64)).right_aligned()).style(if sig.rejected > 0 {
                    theme::error()
                } else {
                    theme::muted()
                }),
            ])
        })
        .collect();
    let header = Row::new(vec![
        Cell::from("signal"),
        Cell::from(Line::from("requests").right_aligned()),
        Cell::from(Line::from("accepted").right_aligned()),
        Cell::from(Line::from("rejected").right_aligned()),
    ])
    .style(theme::header_row());
    f.render_widget(
        Table::new(rows, [Constraint::Fill(1), Constraint::Length(10), Constraint::Length(10), Constraint::Length(10)])
            .header(header),
        table_area,
    );
    let fill = (i.buffered_bytes as f64 / i.max_buffer_bytes.max(1) as f64).clamp(0.0, 1.0);
    let color = if fill > 0.8 {
        theme::ERR
    } else if fill > 0.5 {
        theme::WARN
    } else {
        theme::OK
    };
    f.render_widget(
        Gauge::default().ratio(fill).gauge_style(Style::new().fg(color).bg(theme::SURFACE)).label(Span::styled(
            format!(
                "buffer {} rows · {} of {}",
                compact(i.buffered_rows as f64),
                bytes(i.buffered_bytes as f64),
                bytes(i.max_buffer_bytes as f64)
            ),
            theme::bold(),
        )),
        Rect { height: 1, ..gauge_area },
    );
    let mut lines = vec![Line::from(vec![
        Span::styled("flushes ", theme::dim()),
        Span::styled(compact(i.flushes as f64), theme::text()),
        Span::styled("   errors ", theme::dim()),
        Span::styled(
            i.flush_errors.to_string(),
            if i.flush_errors > 0 { theme::error() } else { Style::new().fg(theme::OK) },
        ),
    ])];
    if let Some(fl) = &i.last_flush {
        lines.push(Line::from(vec![
            Span::styled("last flush ", theme::dim()),
            Span::styled(format!("{} rows in {}", compact(fl.rows as f64), ms(fl.duration_ms)), theme::text()),
            Span::styled(format!(" at {}", widgets::local(fl.at, "%H:%M:%S")), theme::dim()),
        ]));
    }
    if let Some(e) = &i.last_flush_error {
        lines.push(Line::from(Span::styled(format!("last error: {e}"), theme::error())));
    }
    f.render_widget(Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }), info_area);
}

fn draw_storage(f: &mut Frame, area: Rect, s: &Stats) {
    let st = &s.storage;
    let block = widgets::panel_with(
        "Storage",
        Line::from(format!(" otel-ducklake {} · up {} ", s.version, uptime(s.uptime_secs))).style(theme::dim()),
        true,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [info_area, table_area, maint_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(3)]).areas(inner);
    let kv = |k: &str, v: String, style: Style| {
        Line::from(vec![Span::styled(format!("{k:>9} "), theme::dim()), Span::styled(v, style)])
    };
    f.render_widget(
        Paragraph::new(vec![
            kv("catalog", st.catalog_type.to_string(), theme::accent()),
            kv("data", st.data_path.clone(), theme::text()),
            kv("snapshot", st.snapshot_id.to_string(), theme::text()),
        ]),
        info_area,
    );
    let total_bytes: i64 = st.tables.iter().map(|t| t.bytes).sum();
    let rows: Vec<Row> = st
        .tables
        .iter()
        .map(|t| {
            let avg = if t.files > 0 { bytes(t.bytes as f64 / t.files as f64) } else { "–".into() };
            Row::new(vec![
                Cell::from(t.name.clone()).style(theme::text()),
                Cell::from(Line::from(compact(t.rows as f64)).right_aligned()).style(theme::bold()),
                Cell::from(Line::from(compact(t.inlined_rows as f64)).right_aligned()).style(theme::dim()),
                Cell::from(Line::from(compact(t.files as f64)).right_aligned()).style(theme::dim()),
                Cell::from(Line::from(bytes(t.bytes as f64)).right_aligned()).style(theme::text()),
                Cell::from(Line::from(avg).right_aligned()).style(theme::muted()),
            ])
        })
        .collect();
    let header = Row::new(vec![
        Cell::from(format!("table · {}", bytes(total_bytes as f64))),
        Cell::from(Line::from("rows").right_aligned()),
        Cell::from(Line::from("inlined").right_aligned()),
        Cell::from(Line::from("files").right_aligned()),
        Cell::from(Line::from("size").right_aligned()),
        Cell::from(Line::from("avg file").right_aligned()),
    ])
    .style(theme::header_row());
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Fill(1),
                Constraint::Length(8),
                Constraint::Length(8),
                Constraint::Length(6),
                Constraint::Length(10),
                Constraint::Length(9),
            ],
        )
        .header(header),
        table_area,
    );
    let maint = match &st.last_maintenance {
        Some(m) => vec![
            Line::from(vec![
                Span::styled("maintenance ", theme::dim()),
                Span::styled(m.task.clone(), theme::text()),
                Span::raw(" "),
                if m.ok {
                    Span::styled("ok", Style::new().fg(theme::OK))
                } else {
                    Span::styled("failed", theme::error())
                },
                Span::styled(
                    format!(" at {} in {}", widgets::local(m.at, "%H:%M:%S"), ms(m.duration_ms)),
                    theme::dim(),
                ),
            ]),
            Line::from(Span::styled(m.detail.clone(), theme::muted())),
        ],
        None => vec![Line::from(Span::styled("no maintenance run yet", theme::muted()))],
    };
    f.render_widget(Paragraph::new(maint).wrap(ratatui::widgets::Wrap { trim: true }), maint_area);
}
