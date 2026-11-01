//! Help and the project switcher, drawn over the screen.

use super::*;

pub(super) fn render_help(f: &mut Frame, area: Rect) {
    let r = centered(area, 76, 26);
    f.render_widget(Clear, r);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(th::accent())
        .title(title_span("Keys"))
        .title_bottom(Line::from(Span::styled(" any key closes ", th::faint())).right_aligned());
    let inner = block.inner(r);
    f.render_widget(block, r);
    let row = |k: &str, d: &str| {
        Line::from(vec![Span::styled(format!("  {k:<12}"), th::key()), Span::styled(d.to_string(), th::text())])
    };
    let left = vec![
        section_line("Everywhere"),
        row("↑ ↓  j k", "move"),
        row("g G", "top / bottom"),
        row("PgUp PgDn", "page"),
        row("R", "refresh now"),
        row("?", "this help"),
        row("ctrl-c", "quit"),
        Line::raw(""),
        section_line("Issues list"),
        row("⏎", "open issue"),
        row("tab ← →", "status tab"),
        row("1-4", "unresolved/resolved/…"),
        row("/", "search title, culprit"),
        row("s", "cycle sort"),
        row("t T", "cycle time window"),
        row("p", "switch project"),
        row("q", "quit"),
    ];
    let right = vec![
        section_line("Issue detail"),
        row("tab ← →", "next / prev section"),
        row("1-5", "jump to section"),
        row("[ ]", "older / newer event"),
        row("a", "in-app frames only"),
        row("w", "raw (minified) frames"),
        row("t", "events of this trace"),
        row("⏎", "show event (Events)"),
        row("esc q", "back"),
        Line::raw(""),
        section_line("Status"),
        row("r", "resolve"),
        row("i", "ignore"),
        row("u", "unresolve"),
        Line::raw(""),
        Line::from(Span::styled("  Data refreshes every 5s.", th::faint())),
    ];
    let [l, rr] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(inner);
    f.render_widget(Paragraph::new(left), l);
    f.render_widget(Paragraph::new(right), rr);
}

pub(super) fn render_projects(f: &mut Frame, area: Rect, app: &App, selected: usize) {
    let h = u16_sat(app.projects.len()).saturating_add(4).clamp(6, area.height.saturating_sub(2));
    let r = centered(area, 60, h);
    f.render_widget(Clear, r);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(th::accent())
        .title(title_span("Projects"))
        .title_bottom(Line::from(Span::styled(" ⏎ select · esc close ", th::faint())).right_aligned());
    let inner = block.inner(r);
    f.render_widget(block, r);
    if app.projects.is_empty() {
        f.render_widget(Paragraph::new(Span::styled("no projects", th::dim())).alignment(Alignment::Center), inner);
        return;
    }
    let current = app.current_project().map(|p| p.id);
    let rows: Vec<Row> = app
        .projects
        .iter()
        .map(|p| {
            let mark =
                if Some(p.id) == current { Span::styled("●", Style::new().fg(th::GREEN)) } else { Span::raw(" ") };
            Row::new(vec![
                Cell::from(mark),
                Cell::from(Span::styled(p.slug.clone(), th::bold())),
                Cell::from(Span::styled(p.name.clone(), th::text())),
                Cell::from(Span::styled(p.platform.clone().unwrap_or_default(), th::dim())),
            ])
        })
        .collect();
    let mut state = ratatui::widgets::TableState::default().with_selected(Some(selected));
    let table =
        Table::new(rows, [Constraint::Length(1), Constraint::Length(18), Constraint::Min(10), Constraint::Length(12)])
            .row_highlight_style(th::selected_row())
            .highlight_symbol(Span::styled("▌", th::accent()))
            .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(table, inner, &mut state);
}
