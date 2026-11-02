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

use mallard_term::{text::clean, tui::keys};

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
        let (ok, label) = self.live();
        let mut right = vec![];
        if self.loader.any_pending() {
            right.push(Span::styled(format!("{} ", mallard_term::tui::spinner(self.spin)), theme::dim()));
        }
        if let Some(r) = self.ingest_rate {
            right.push(Span::styled("↗ ", theme::faint()));
            right.push(Span::styled(format!("{}/s", crate::fmt::compact(r)), theme::text()));
            right.push(Span::styled(" ingest  ", theme::faint()));
        }
        right.push(Span::styled("◷ ", theme::faint()));
        right.push(Span::styled(format!("last {}", self.since()), theme::accent()));
        right.push(Span::raw("  "));
        right.push(Span::styled("● ", Style::new().fg(if ok { theme::GREEN } else { theme::RED })));
        right.push(Span::styled(format!("{label} "), theme::dim()));
        let right = Line::from(right);
        let rw = right.width() as u16;
        let left = Line::from(vec![
            Span::styled(" ◆ telemetry ", theme::accent()),
            Span::styled("│ ", theme::faint()),
            Span::styled(
                widgets::trunc(self.client.base_url(), area.width.saturating_sub(rw + 22) as usize),
                theme::dim(),
            ),
        ]);
        f.render_widget(Block::default().style(Style::new().bg(theme::PANEL)), area);
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
                spans.push(Span::styled(format!(" {}", i + 1), theme::faint()));
                spans.push(Span::styled(format!(" {name} "), theme::dim()));
            }
            spans.push(Span::raw(" "));
        }
        f.render_widget(Line::from(spans), area);
    }

    pub(super) fn draw_footer(&self, f: &mut Frame, area: Rect) {
        if let Some(p) = &self.prompt {
            use unicode_width::UnicodeWidthChar;
            let label = format!(" {} ❯ ", p.label);
            let label_w = widgets::width(&label) + 1;
            // Scroll the text so the cursor stays in view.
            let room = (area.width as usize).saturating_sub(label_w + 1);
            let chars: Vec<char> = p.input.value.chars().collect();
            let cursor = p.input.cursor.min(chars.len());
            // A filter can hold a name as the server has it, so it shows cleaned. The cursor
            // stays after the same character.
            let cols =
                |c: char| clean(c.encode_utf8(&mut [0; 4])).chars().map(|c| c.width().unwrap_or(0)).sum::<usize>();
            let mut start = 0;
            let mut before: usize = chars[..cursor].iter().map(|c| cols(*c)).sum();
            while before > room && start < cursor {
                before -= cols(chars[start]);
                start += 1;
            }
            let mut spans = vec![
                Span::styled(label, theme::badge(theme::ACCENT2)),
                Span::raw(" "),
                Span::styled(clean(&chars[start..].iter().collect::<String>()).into_owned(), theme::bold()),
            ];
            if start == 0 {
                spans.push(Span::styled(format!("   {}", p.help), theme::faint()));
            }
            f.render_widget(Line::from(spans), area);
            let offset = u16::try_from(label_w + before).unwrap_or(u16::MAX);
            let x = area.x.saturating_add(offset);
            f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
            return;
        }
        let mut spans = widgets::hints(&self.hints());
        spans.extend(widgets::hints(&[("p", "app"), ("?", "help"), ("q", "quit")]));
        let line = Line::from(spans);
        let mut hints_area = area;
        if let Some((msg, is_err, _)) = &self.flash {
            let text = format!(" {} ", widgets::trunc(msg, (area.width / 2) as usize));
            let w = Line::from(text.as_str()).width() as u16;
            let style = if *is_err { theme::badge(theme::RED) } else { theme::badge(theme::GREEN) };
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
            h.retain(|(k, _)| *k != "x");
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
                vec![("j/k", "span"), ("pgup/dn", "scroll"), ("l", "logs"), ("x", "errors"), ("esc", "back")]
            }
            View::Traces => {
                vec![("/", "filter"), ("E", "errors only"), ("↵", "open"), ("x", "errors"), ("t", "window")]
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

    /// The app has no telemetry in the window: say so, and how to get it.
    pub(super) fn draw_no_telemetry(&self, f: &mut Frame, area: Rect, slug: &str, since: &str) {
        let lines = vec![
            Line::from(Span::styled(format!("App {slug} has no telemetry in the last {since}"), theme::bold())),
            Line::from(Span::styled(
                format!("set OTEL_SERVICE_NAME={slug} in the app (mallard setup {slug})"),
                theme::dim(),
            )),
            Line::from(Span::styled("t widens the window · p picks another app", theme::dim())),
        ];
        let y = area.y + area.height.saturating_sub(3) / 2;
        f.render_widget(
            Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center),
            Rect { y, height: 3.min(area.height), ..area },
        );
    }

    pub(super) fn draw_help(&self, f: &mut Frame, area: Rect) {
        // The keys every tab shares come first (mallard_term::tui::keys).
        let mut rows: Vec<(&str, &str)> = keys::SHARED.to_vec();
        rows.extend([
            ("", ""),
            ("Overview", "↵ service → operations → traces"),
            (
                "Traces",
                if self.errors_enabled {
                    "/ filter  E errors only  ↵ waterfall  l logs  x errors"
                } else {
                    "/ filter  E errors only  ↵ waterfall  l logs"
                },
            ),
            ("", "filter: service=checkout op=\"GET /\" errors min=250ms key=value"),
            ("Logs", "/ filter  s severity  space follow  ↵ details  o open trace"),
            ("", "filter: service=api level=warn trace=<id> words to find"),
            ("", "in a filter: tab accepts a suggestion, ↑↓ choose"),
            ("Metrics", "/ search  ↵ chart  a aggregation  b group by"),
            ("", ""),
            ("", keys::TIMES),
        ]);
        let w = 100.min(area.width);
        let h = (rows.len() as u16 + 4).min(area.height);
        let r = widgets::centered(area, w, h);
        f.render_widget(Clear, r);
        let lines: Vec<Line> = rows
            .iter()
            .map(|(k, d)| {
                Line::from(vec![
                    Span::styled(format!("{k:>24}  "), theme::key()),
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
