//! Rendering. Everything draws inside the given `area` only, so a host can
//! embed the TUI in part of its screen.

use super::app::*;
use super::theme::{self as th};
use crate::StatusFilter;
use crate::fmt;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row, Sparkline, Table, Wrap,
};
use sdl_client::types::IssueStatus;

mod detail;
mod issues;
mod overlays;
mod trace;

use detail::render_detail;
use issues::render_issues;
use overlays::{render_help, render_projects};
use trace::render_trace;

/// `n` as a terminal coordinate, saturating instead of wrapping.
fn u16_sat(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

pub(super) const MIN_WIDTH: u16 = 50;
pub(super) const MIN_HEIGHT: u16 = 12;

pub(super) fn render(f: &mut Frame, area: Rect, app: &mut App) {
    f.render_widget(Clear, area);
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        too_small(f, area);
        return;
    }
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)]).areas(area);
    render_header(f, header, app);
    match app.view() {
        View::Issues => render_issues(f, body, app),
        View::Detail => render_detail(f, body, app),
        View::Trace => render_trace(f, body, app),
    }
    render_footer(f, footer, app);
    match app.overlay {
        Overlay::None => {}
        Overlay::Help => render_help(f, area),
        Overlay::Projects { selected } => render_projects(f, area, app, selected),
    }
}

fn too_small(f: &mut Frame, area: Rect) {
    let text = Text::from(vec![
        Line::from("Terminal too small".bold().fg(th::YELLOW)),
        Line::from(Span::styled(
            format!("need {MIN_WIDTH}×{MIN_HEIGHT}, have {}×{}", area.width, area.height),
            th::dim(),
        )),
    ]);
    let h = 2.min(area.height);
    let y = area.y + area.height.saturating_sub(h) / 2;
    f.render_widget(
        Paragraph::new(text).alignment(Alignment::Center).wrap(Wrap { trim: true }),
        Rect { y, height: h, ..area },
    );
}

fn rounded<'a>(title: impl Into<Line<'a>>) -> Block<'a> {
    mallard_term::tui::rounded().title(title)
}

fn title_span(s: &str) -> Span<'static> {
    Span::styled(format!(" {s} "), th::bold())
}

/// A load's state in place of its content: the error (red) when it failed,
/// else the loading text.
fn load_state(error: Option<&str>, loading: &str) -> Paragraph<'static> {
    let line = match error {
        Some(e) => Line::from(vec![
            Span::styled(format!("✗ {e}"), Style::new().fg(th::RED)),
            Span::styled("  · r retry", th::dim()),
        ]),
        None => Line::from(Span::styled(loading.to_string(), th::dim())),
    };
    Paragraph::new(line).alignment(Alignment::Center).wrap(Wrap { trim: true })
}

use mallard_term::tui::{centered, hints};

fn spinner(app: &App) -> &'static str {
    mallard_term::tui::spinner(app.spinner)
}

// ---------------------------------------------------------------- header

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    f.render_widget(Block::new().style(Style::new().bg(th::PANEL)), area);
    let mut left = vec![Span::styled(" ◆ errors ", th::tab_active()), Span::raw(" ")];
    match app.current_project() {
        Some(p) => {
            left.push(Span::styled(p.name.clone(), th::bold()));
            left.push(Span::styled(
                format!("  {}{}", p.slug, p.platform.as_ref().map(|x| format!(" · {x}")).unwrap_or_default()),
                th::dim(),
            ));
        }
        None => left.push(Span::styled(app.project.clone().unwrap_or_else(|| "no project".into()), th::dim())),
    }
    let view = match app.view() {
        View::Issues => "",
        View::Detail => "  ›  issue",
        View::Trace => "  ›  trace",
    };
    left.push(Span::styled(view, th::faint()));

    let mut right: Vec<Span> = vec![];
    if app.loading() {
        right.push(Span::styled(format!("{} ", spinner(app)), th::accent()));
    }
    if let Some(i) = &app.info {
        let ratio = if i.queue_capacity == 0 { 0.0 } else { i.queue_depth as f64 / i.queue_capacity as f64 };
        let color = if ratio < 0.5 {
            th::GREEN
        } else if ratio < 0.8 {
            th::YELLOW
        } else {
            th::RED
        };
        let cells = 6usize;
        let filled = if i.queue_depth == 0 { 0 } else { ((ratio * cells as f64).ceil() as usize).clamp(1, cells) };
        right.push(Span::styled("queue ", th::dim()));
        right.push(Span::styled("■".repeat(filled), Style::new().fg(color)));
        right.push(Span::styled("·".repeat(cells - filled), th::faint()));
        right.push(Span::styled(
            format!(" {}/{}  ", fmt::compact(i.queue_depth), fmt::compact(i.queue_capacity)),
            th::text(),
        ));
    }
    if app.offline.is_some() {
        right.push(Span::styled("● offline  ", Style::new().fg(th::RED)));
    } else if app.info.is_some() {
        right.push(Span::styled("● live  ", Style::new().fg(th::GREEN)));
    }
    if area.width > 110 {
        right.push(Span::styled(format!("{}  ", app.server_url), th::faint()));
    }
    if let Some(i) = &app.info {
        right.push(Span::styled(format!("v{} ", i.version), th::dim()));
    }
    f.render_widget(Paragraph::new(Line::from(left)), area);
    f.render_widget(Paragraph::new(Line::from(right).right_aligned()), area);
}

// ---------------------------------------------------------------- footer

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let line = if let Some(input) = &app.input {
        Line::from(vec![
            Span::styled(" / ", th::tab_active()),
            Span::styled(format!(" {input}"), Style::new().fg(th::YELLOW)),
            Span::styled("█", Style::new().fg(th::YELLOW).add_modifier(Modifier::SLOW_BLINK)),
            Span::styled("   enter search · esc cancel · ctrl-u clear", th::dim()),
        ])
    } else if let Some(t) = &app.toast {
        let c = if t.error { th::RED } else { th::GREEN };
        Line::from(Span::styled(format!(" {}", t.text), Style::new().fg(c).add_modifier(Modifier::BOLD)))
    } else if let Some(e) = app.offline.as_ref().or(app.global_error.as_ref()) {
        Line::from(vec![
            Span::styled(format!(" ✗ {e}"), Style::new().fg(th::RED)),
            Span::styled("   r retry", th::dim()),
        ])
    } else {
        let pairs: &[(&str, &str)] = match app.view() {
            View::Issues => &[
                ("↑↓", "move"),
                ("⏎", "open"),
                ("tab", "status"),
                ("/", "search"),
                ("s", "sort"),
                ("t", "window"),
                ("p", "project"),
                ("R/I/U", "resolve/ignore/unresolve"),
                ("r", "refresh"),
                ("?", "help"),
                ("q", "quit"),
            ],
            View::Detail => &[
                ("tab", "section"),
                ("↑↓", "move"),
                ("[ ]", "older/newer event"),
                ("a", "in-app"),
                ("w", "raw"),
                ("x", "trace"),
                ("R/I/U", "status"),
                ("esc", "back"),
                ("?", "help"),
            ],
            View::Trace => &[("↑↓", "move"), ("⏎", "open event"), ("esc", "back"), ("?", "help")],
        };
        Line::from(hints(pairs))
    };
    f.render_widget(Paragraph::new(line), area);
}

fn status_span(s: IssueStatus) -> Span<'static> {
    Span::styled(s.as_str().to_string(), Style::new().fg(th::status_color(s.as_str())))
}

fn kv_line(key: &str, value: Span<'static>) -> Line<'static> {
    Line::from(vec![Span::styled(format!("  {}", fmt::pad(key, 14)), th::dim()), value])
}

fn section_line(title: &str) -> Line<'static> {
    Line::from(Span::styled(title.to_string(), Style::new().fg(th::ACCENT).add_modifier(Modifier::BOLD)))
}

#[cfg(test)]
mod tests {
    use super::detail::json_line;
    use super::*;
    use crate::tui::fixtures::*;
    use crossterm::event::{KeyCode, KeyEvent};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use sdl_client::types::IssueList;

    pub(super) fn draw(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, f.area(), app)).unwrap();
        screen(&term)
    }

    #[test]
    fn json_lines_are_highlighted_by_kind() {
        let parts =
            |l: &str| json_line(l).spans.iter().map(|s| (s.content.to_string(), s.style.fg)).collect::<Vec<_>>();
        let p = parts(r#"    "level": "error","#);
        assert_eq!(p[0].0, "    ");
        assert_eq!(p[1], ("\"level\"".into(), Some(th::ACCENT)));
        assert_eq!(p[3], ("\"error\"".into(), Some(th::GREEN)));
        assert_eq!(p[4].0, ",");
        assert_eq!(parts(r#""n": -1.5"#)[3], ("-1.5".into(), Some(th::YELLOW)));
        assert_eq!(parts(r#""ok": true,"#)[3].1, Some(th::MAGENTA));
        assert_eq!(parts("null")[1].1, Some(th::MAGENTA));
        // A value line without a key, and structure lines.
        assert_eq!(parts(r#"  "x","#)[1], ("\"x\"".into(), Some(th::GREEN)));
        assert_eq!(parts("  },")[1].0, "}");
    }

    fn press(app: &mut App, c: KeyCode) {
        let _ = app.on_key(KeyEvent::from(c));
    }

    #[test]
    fn issues_view_shows_stats_table_and_header() {
        let mut app = loaded_app();
        let s = draw(&mut app, 140, 40);
        for want in [
            "◆ errors",
            "Web App",
            "queue",
            "12/20k",
            "Unresolved 3",
            "EVENTS",
            "1.3k",
            "USERS",
            "TypeError: Cannot read properties of undefined",
            "app/checkout.js in parseConfig",
            "1.2k",
            "3m ago",
            "1d ago",
            " ERROR ",
            " FATAL ",
            "unresolved",
            "sort Last seen",
            "window 24h",
            "TREND",
        ] {
            assert!(s.contains(want), "missing {want:?} in\n{s}");
        }
        // A sparkline character is drawn in the trend column.
        assert!(s.contains('█'));
    }

    #[test]
    fn escape_sequences_from_events_never_reach_the_buffer() {
        let mut app = App::new("http://x", None);
        app.start();
        app.on_response(Resp::Projects(Ok(projects())));
        let mut list = issues();
        list[0].title = "evil\x1b]52;c;cHduZWQ=\x07 \x1b[2Jtitle\u{9b}31m".into();
        list[0].culprit = Some("cul\rprit\tx".into());
        let key = app.issues_key().unwrap();
        app.on_response(Resp::Issues(key, Ok(IssueList { issues: list, window_start: now(), window_end: now() })));
        app.now = now();
        let s = draw(&mut app, 140, 40);
        assert!(!s.chars().any(|c| c.is_control() && c != '\n'), "control character in buffer:\n{s}");
        assert!(s.contains("evil\\u{1b}]52;c;cHduZWQ=\\u{7} \\u{1b}[2Jtitle"), "{s}");
        assert!(s.contains("cul prit    x"), "{s}");
    }

    #[test]
    fn errors_render_in_place_and_offline_only_for_network_errors() {
        let mut app = App::new("http://x", Some("web".into()));
        app.start();
        app.on_response(Resp::Projects(Ok(projects())));
        let key = app.issues_key().unwrap();
        app.on_response(Resp::Issues(key, Err(FetchError::api("not found: project not found"))));
        app.on_response(Resp::Info(Ok(info())));
        let s = draw(&mut app, 120, 30);
        assert!(s.contains("✗ not found: project not found"), "{s}");
        assert!(s.contains("● live") && !s.contains("offline"), "{s}");

        app.on_response(Resp::Stats {
            project: "web".into(),
            window: app.window,
            result: Err(FetchError::offline("cannot reach server")),
        });
        let s = draw(&mut app, 120, 30);
        assert!(s.contains("● offline") && s.contains("cannot reach server"), "{s}");
        // Info succeeding clears "offline" but not the issue list error.
        app.on_response(Resp::Info(Ok(info())));
        assert!(app.offline.is_none());
        assert!(app.issues_error.is_some() && app.stats_error.is_some());
        let s = draw(&mut app, 120, 30);
        assert!(s.contains("project not found") && !s.contains("offline"), "{s}");
    }

    #[test]
    fn no_projects_hint_names_the_host_command() {
        let mut app = App::new("http://x", None);
        app.hints.command = "mallard errors";
        app.start();
        app.on_response(Resp::Projects(Ok(vec![])));
        let s = draw(&mut app, 120, 30);
        assert!(s.contains("create one with: mallard errors projects create <name>"), "{s}");
    }

    #[test]
    fn narrow_and_tiny_terminals() {
        let mut app = loaded_app();
        let s = draw(&mut app, 60, 16);
        assert!(s.contains("ERROR") && !s.contains("TREND"), "{s}");
        let s = draw(&mut app, 40, 10);
        assert!(s.contains("Terminal too small"), "{s}");
    }

    #[test]
    fn empty_and_search_states() {
        let mut app = loaded_app();
        press(&mut app, KeyCode::Char('/'));
        for c in "zzz".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let s = draw(&mut app, 120, 30);
        assert!(s.contains("zzz") && s.contains("enter search"), "{s}");
        press(&mut app, KeyCode::Enter);
        let key = app.issues_key().unwrap();
        app.on_response(Resp::Issues(
            key,
            Ok(sdl_client::types::IssueList { issues: vec![], window_start: now(), window_end: now() }),
        ));
        let s = draw(&mut app, 120, 30);
        assert!(s.contains("No unresolved issues matching \"zzz\" in the last 24h"), "{s}");
    }

    #[test]
    fn detail_tabs_render() {
        let mut app = detail_app();
        let s = draw(&mut app, 140, 45);
        for want in [
            "Error: inside span",
            "Overview",
            "Stack trace",
            "web@1.2.3",
            "browser",
            "Chrome 141",
            "event 1/2",
            "Activity",
            "node v24.22.1",
        ] {
            assert!(s.contains(want), "overview missing {want:?}\n{s}");
        }

        press(&mut app, KeyCode::Char('2'));
        let s = draw(&mut app, 140, 45);
        for want in [
            "Error: inside span",
            "Caused by",
            "TypeError: root cause",
            "parseConfig",
            "/app/smoke.mjs:11:84",
            "throw new Error",
            "line nine",
            "unhandled",
        ] {
            assert!(s.contains(want), "stack missing {want:?}\n{s}");
        }
        // In-app only hides node internals; raw shows the minified frame.
        assert!(s.contains("runMain"));
        press(&mut app, KeyCode::Char('a'));
        let s = draw(&mut app, 140, 45);
        assert!(!s.contains("runMain") && s.contains("parseConfig"), "{s}");
        press(&mut app, KeyCode::Char('w'));
        let s = draw(&mut app, 140, 45);
        assert!(s.contains("/app/smoke.min.js:1:999"), "{s}");

        press(&mut app, KeyCode::Char('3'));
        let s = draw(&mut app, 140, 45);
        assert!(s.contains("user logged in") && s.contains("GET /api/cart -> 500"), "{s}");

        press(&mut app, KeyCode::Char('4'));
        let s = draw(&mut app, 140, 45);
        assert!(s.contains("production") && s.contains("id:user-42") && s.contains("1h ago"), "{s}");

        press(&mut app, KeyCode::Char('5'));
        let s = draw(&mut app, 140, 45);
        assert!(s.contains("\"event_id\"") && s.contains("655354d4abd54c31a077367796352d8c"), "{s}");
    }

    #[test]
    fn overlays_and_trace_view() {
        let mut app = loaded_app();
        press(&mut app, KeyCode::Char('?'));
        let s = draw(&mut app, 120, 40);
        assert!(s.contains("Keys") && s.contains("older / newer event"), "{s}");
        // The shared keys (the same list as on the telemetry tab) and the time zone.
        for (_, what) in mallard_term::tui::keys::SHARED {
            assert!(s.contains(what), "missing {what:?} in\n{s}");
        }
        assert!(s.contains("All times are UTC."), "{s}");
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('p'));
        let s = draw(&mut app, 120, 40);
        assert!(s.contains("Projects") && s.contains("api"), "{s}");
        press(&mut app, KeyCode::Esc);

        app.show_trace("b7644661850f40e69380d8c9ea253e84");
        app.on_response(Resp::Trace { trace_id: "b7644661850f40e69380d8c9ea253e84".into(), result: Ok(event_list()) });
        let s = draw(&mut app, 120, 30);
        assert!(
            s.contains("Trace")
                && s.contains("b7644661850f40e69380d8c9ea253e84")
                && s.contains("2 error events")
                && s.contains("web"),
            "{s}"
        );
    }

    #[test]
    fn draws_only_inside_the_given_area() {
        let mut app = loaded_app();
        let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
        term.draw(|f| {
            let a = Rect { x: 10, y: 5, width: 120, height: 30 };
            render(f, a, &mut app)
        })
        .unwrap();
        let buf = term.backend().buffer();
        for y in 0..40u16 {
            for x in 0..140u16 {
                let inside = (10..130).contains(&x) && (5..35).contains(&y);
                if !inside {
                    assert_eq!(buf[(x, y)].symbol(), " ", "drew outside area at {x},{y}");
                }
            }
        }
    }
}
