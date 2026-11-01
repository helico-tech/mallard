//! The error events of one trace (all projects).

use super::*;

pub(super) fn render_trace(f: &mut Frame, area: Rect, app: &mut App) {
    let spin = spinner(app);
    let App { projects, screen, now, .. } = app;
    let now = *now;
    let Screen::Trace(t) = screen else { return };
    let block = rounded(vec![
        title_span("Trace"),
        Span::styled(format!("{} ", t.trace_id), Style::new().fg(th::ACCENT2)),
        Span::styled(if t.loaded { format!("· {} error events ", t.events.len()) } else { String::new() }, th::faint()),
    ]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if !t.loaded {
        f.render_widget(load_state(t.error.as_deref(), &format!("{spin} loading trace…")), inner);
        return;
    }
    if t.events.is_empty() {
        let text = "No error events for this trace.";
        f.render_widget(Paragraph::new(Span::styled(text, th::dim())).alignment(Alignment::Center), inner);
        return;
    }
    let rows: Vec<Row> = t
        .events
        .iter()
        .map(|e| {
            let project = projects
                .iter()
                .find(|p| p.id == e.project_id)
                .map(|p| p.slug.clone())
                .unwrap_or_else(|| e.project_id.to_string());
            Row::new(vec![
                Cell::from(Span::styled(th::level_label(&e.level), th::badge(&e.level))),
                Cell::from(Span::styled(project, Style::new().fg(th::ACCENT2))),
                Cell::from(Span::styled(e.title.clone(), th::bold())),
                Cell::from(Span::styled(e.span_id.clone().unwrap_or_default(), th::faint())),
                Cell::from(Span::styled(fmt::relative_time(e.timestamp, now), th::text())),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Length(14),
            Constraint::Min(20),
            Constraint::Length(16),
            Constraint::Length(9),
        ],
    )
    .header(Row::new(["LEVEL", "PROJECT", "EVENT", "SPAN", "WHEN"]).style(th::faint().bold()))
    .row_highlight_style(th::selected_row())
    .highlight_symbol(Span::styled("▌", th::accent()))
    .highlight_spacing(HighlightSpacing::Always)
    .column_spacing(1);
    f.render_stateful_widget(table, inner, &mut t.state);
}
