//! The frame around the views: header, tabs, footer (hints, flash, prompt)
//! and the help overlay.

use std::time::Duration;

use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};

use super::{Embedded, VIEWS, View, theme, widgets};

impl Embedded {
    fn live(&self) -> (bool, &'static str) {
        match self.last_ok {
            Some(t) if t.elapsed() < Duration::from_secs(20) => (true, "live"),
            Some(_) => (false, "stale"),
            None if self.stats_error.is_some() => (false, "offline"),
            None => (false, "connecting"),
        }
    }

    pub(super) fn draw_header(&self, f: &mut Frame, area: Rect) {
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

    pub(super) fn draw_tabs(&self, f: &mut Frame, area: Rect) {
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

    pub(super) fn draw_footer(&self, f: &mut Frame, area: Rect) {
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

    pub(super) fn draw_help(&self, f: &mut Frame, area: Rect) {
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
