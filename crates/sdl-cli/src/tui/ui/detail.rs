//! One issue: title card, tabs (overview, stack trace, breadcrumbs,
//! events, JSON).

use super::*;

pub(super) fn render_detail(f: &mut Frame, area: Rect, app: &mut App) {
    let now = app.now;
    let window = app.window.since();
    let spin = spinner(app);
    let Screen::Detail(d) = &mut app.screen else { return };
    let [title, tabs, content] =
        Layout::vertical([Constraint::Length(4), Constraint::Length(1), Constraint::Min(0)]).areas(area);

    // Title card.
    let s = d.current_summary();
    let level = s.map(|s| s.level.as_str()).or(d.event.as_ref().map(|e| e.summary.level.as_str())).unwrap_or_default();
    let title_text = if d.title().is_empty() {
        d.event.as_ref().map(|e| e.summary.title.clone()).unwrap_or_default()
    } else {
        d.title().to_string()
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(th::level_color(level)));
    let inner = block.inner(title);
    f.render_widget(block, title);
    let mut l1 = vec![
        Span::styled(th::level_label(level), th::badge(level)),
        Span::raw(" "),
        Span::styled(title_text, th::bold()),
    ];
    if let Some(e) = &d.error {
        l1.push(Span::styled(format!("  ✗ {e}"), Style::new().fg(th::RED)));
    } else if s.is_none() {
        l1.push(Span::styled(format!("  {spin}"), th::dim()));
    }
    let mut l2 = vec![Span::raw(" ")];
    if let Some(s) = s {
        if let Some(c) = s.culprit.as_ref().filter(|c| !c.is_empty()) {
            l2.push(Span::styled(format!("{c}  "), th::dim()));
        }
        l2.push(status_span(s.status));
        l2.push(Span::styled(
            format!(
                "  ·  {} events  ·  {} users  ·  last seen {}  ·  first seen {}  ·  {window}",
                fmt::compact(s.count),
                fmt::compact(s.user_count),
                fmt::relative_time(s.last_seen, now),
                fmt::relative_time(s.first_seen, now)
            ),
            th::dim(),
        ));
        l2.push(Span::styled(format!("  ·  {}", s.id), th::faint()));
    } else {
        l2.push(Span::styled(d.id.clone(), th::faint()));
    }
    f.render_widget(Paragraph::new(vec![Line::from(l1), Line::from(l2)]), inner);

    // Tab bar + event navigation.
    let mut tab_spans = vec![Span::raw(" ")];
    for (i, t) in Tab::ALL.iter().enumerate() {
        let label = match t {
            Tab::Events if d.events_loaded => {
                format!(" {} {} {} ", i + 1, t.title(), d.events.len())
            }
            Tab::Breadcrumbs if d.event.is_some() => {
                format!(" {} {} {} ", i + 1, t.title(), d.crumbs.len())
            }
            _ => format!(" {} {} ", i + 1, t.title()),
        };
        tab_spans.push(Span::styled(label, if *t == d.tab() { th::tab_active() } else { th::tab_inactive() }));
        tab_spans.push(Span::raw(" "));
    }
    f.render_widget(Paragraph::new(Line::from(tab_spans)), tabs);
    let mut nav = vec![];
    if d.events_loaded && !d.events.is_empty() {
        let more = if d.next_cursor.is_some() { "+" } else { "" };
        nav.push(Span::styled("[ ", th::key()));
        nav.push(Span::styled(format!("event {}/{}{more}", d.event_idx + 1, d.events.len()), th::text()));
        if let Some(e) = &d.event {
            nav.push(Span::styled(
                format!(
                    " · {} · {}",
                    fmt::relative_time(e.summary.timestamp, now),
                    fmt::truncate(&e.summary.event_id, 8)
                ),
                th::dim(),
            ));
        } else {
            nav.push(Span::styled(" · loading…", th::dim()));
        }
        nav.push(Span::styled(" ] ", th::key()));
    }
    if tabs.width > 100 {
        f.render_widget(Paragraph::new(Line::from(nav).right_aligned()), tabs);
    }

    match d.tab() {
        Tab::Overview => render_overview(f, content, d, now, window),
        Tab::Stack => render_stack(f, content, d),
        Tab::Breadcrumbs => render_breadcrumbs(f, content, d),
        Tab::Events => render_events(f, content, d, now),
        Tab::Json => render_json(f, content, d),
    }
}

fn render_overview(f: &mut Frame, area: Rect, d: &mut Detail, now: chrono::DateTime<chrono::Utc>, window: &str) {
    let block = rounded(title_span("Overview"));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let two_cols = inner.width >= 96;
    let col_w = if two_cols { inner.width / 2 } else { inner.width };

    let mut left: Vec<Line> = vec![section_line("Details")];
    let s = d.current_summary();
    let txt = |v: String| Span::styled(v, th::text());
    if let Some(s) = s {
        left.push(kv_line("Status", status_span(s.status)));
        left.push(kv_line("Level", Span::styled(s.level.clone(), Style::new().fg(th::level_color(&s.level)))));
        left.push(kv_line("Platform", txt(s.platform.clone())));
        left.push(kv_line("Events", txt(s.count.to_string())));
        left.push(kv_line("Users", txt(s.user_count.to_string())));
        left.push(kv_line(
            "First seen",
            txt(format!("{}  ({})", fmt::timestamp(s.first_seen), fmt::relative_time(s.first_seen, now))),
        ));
        left.push(kv_line(
            "Last seen",
            txt(format!("{}  ({})", fmt::timestamp(s.last_seen), fmt::relative_time(s.last_seen, now))),
        ));
        if let Some(t) = &s.exception_type {
            left.push(kv_line("Exception", txt(t.clone())));
        }
    }
    if let Some(dd) = &d.detail {
        left.push(kv_line("First release", txt(dd.first_release.clone().unwrap_or_else(|| "–".into()))));
        left.push(kv_line("Last release", txt(dd.last_release.clone().unwrap_or_else(|| "–".into()))));
    }
    if let Some(s) = s {
        left.push(Line::raw(""));
        left.push(section_line(&format!("Trend · last {window}")));
        let w = (col_w as usize).saturating_sub(4).clamp(8, 72);
        left.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(fmt::sparkline(&fmt::resample(&s.trend, w)), Style::new().fg(th::ACCENT)),
        ]));
    }
    if let Some(e) = &d.event {
        left.push(Line::raw(""));
        left.push(section_line("Shown event"));
        left.push(kv_line("Event", txt(e.summary.event_id.clone())));
        left.push(kv_line(
            "Time",
            txt(format!("{}  ({})", fmt::timestamp(e.summary.timestamp), fmt::relative_time(e.summary.timestamp, now))),
        ));
        for (k, v) in
            [("Environment", &e.summary.environment), ("Release", &e.summary.release), ("User", &e.summary.user)]
        {
            if let Some(v) = v {
                left.push(kv_line(k, txt(v.clone())));
            }
        }
        if let Some(t) = &e.summary.trace_id {
            left.push(kv_line("Trace", Span::styled(format!("{t}  (t: open)"), Style::new().fg(th::ACCENT2))));
        }
        for (k, v) in &d.contexts {
            left.push(kv_line(k, txt(v.clone())));
        }
        if let Some(r) = &d.request {
            let (method, url) = (r.method.as_deref().unwrap_or_default(), r.url.as_deref().unwrap_or_default());
            left.push(kv_line("Request", txt(format!("{method} {url}"))));
        }
    }
    if let Some(dd) = &d.detail {
        left.push(Line::raw(""));
        left.push(section_line("Activity"));
        if dd.activity.is_empty() {
            left.push(Line::from(Span::styled("  no status changes", th::faint())));
        }
        for a in &dd.activity {
            left.push(Line::from(vec![
                Span::styled(format!("  {}  ", fmt::timestamp(a.at)), th::dim()),
                status_span(a.status),
                Span::styled(format!("  {}", a.reason), th::faint()),
            ]));
        }
    }

    let mut right: Vec<Line> = vec![section_line("Tags")];
    match &d.detail {
        None => match &d.error {
            Some(e) => right.push(Line::from(Span::styled(format!("  ✗ {e}"), Style::new().fg(th::RED)))),
            None => right.push(Line::from(Span::styled("  loading…", th::dim()))),
        },
        Some(dd) if dd.tags.is_empty() => right.push(Line::from(Span::styled("  no tags", th::faint()))),
        Some(dd) => {
            let bar_w = (col_w as usize).saturating_sub(2 + 22 + 1 + 7 + 2).clamp(6, 40);
            for t in &dd.tags {
                right.push(Line::from(vec![
                    Span::styled(format!("  {}", t.key), th::bold()),
                    Span::styled(format!("  {} unique", t.unique), th::faint()),
                ]));
                let total: u64 = t.top.iter().map(|v| v.count).sum::<u64>().max(1);
                for v in &t.top {
                    let bar = fmt::hbar(v.count, total, bar_w);
                    let pad = bar_w.saturating_sub(bar.chars().count());
                    right.push(Line::from(vec![
                        Span::styled(format!("    {}", fmt::pad(&fmt::truncate(&v.value, 21), 22)), th::text()),
                        Span::styled(bar, Style::new().fg(th::ACCENT2)),
                        Span::styled("·".repeat(pad), th::faint()),
                        Span::styled(format!(" {:>5.1}%", v.count as f64 * 100.0 / total as f64), th::dim()),
                    ]));
                }
                right.push(Line::raw(""));
            }
        }
    }

    let total_lines = if two_cols { left.len().max(right.len()) } else { left.len() + 1 + right.len() };
    d.max_scroll = total_lines.saturating_sub(inner.height as usize);
    d.scroll = d.scroll.min(d.max_scroll);
    let scroll = (u16_sat(d.scroll), 0);
    if two_cols {
        let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(inner);
        f.render_widget(Paragraph::new(left).scroll(scroll), l);
        f.render_widget(Paragraph::new(right).scroll(scroll), r);
    } else {
        left.push(Line::raw(""));
        left.extend(right);
        f.render_widget(Paragraph::new(left).scroll(scroll), inner);
    }
}

fn render_stack(f: &mut Frame, area: Rect, d: &mut Detail) {
    let flag = |on: bool| {
        if on { Span::styled("on", Style::new().fg(th::GREEN)) } else { Span::styled("off", th::faint()) }
    };
    let mut title = vec![title_span("Stack trace"), Span::styled("newest first · ", th::faint())];
    title.extend([Span::styled("a", th::key()), Span::styled(" in-app only ", th::dim()), flag(d.in_app_only)]);
    if d.has_raw() {
        title.extend([Span::styled("  w", th::key()), Span::styled(" raw ", th::dim()), flag(d.raw)]);
    }
    title.push(Span::raw(" "));
    let block = rounded(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if d.event.is_none() {
        f.render_widget(load_state(d.event_error.as_deref(), "loading event…"), inner);
        return;
    }
    if d.exceptions.is_empty() {
        let msg = d.message.clone();
        let mut lines = vec![Line::from(Span::styled("This event has no stack trace.", th::dim()))];
        if let Some(m) = msg {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(m, th::text())));
        }
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center).wrap(Wrap { trim: false }), inner);
        return;
    }

    let frames = d.stack_frames();
    let mut lines: Vec<Line> = vec![];
    let (mut sel_start, mut sel_end) = (0usize, 0usize);
    let width = inner.width as usize;
    let push_exc_header = |lines: &mut Vec<Line>, i: usize| {
        let e = &d.exceptions[i];
        if i > 0 {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled("Caused by", th::faint().add_modifier(Modifier::ITALIC))));
        }
        let mut spans = vec![Span::styled(e.headline(), Style::new().fg(th::RED).add_modifier(Modifier::BOLD))];
        if let Some(m) = e.mechanism_label() {
            spans.push(Span::styled(format!("  ({m})"), th::dim()));
        }
        lines.push(Line::from(spans));
    };
    // Frames are grouped by exception, in chain order; every exception gets
    // its header even when the in-app filter hides all its frames.
    let mut next_exc = 0;
    for (idx, sf) in frames.iter().enumerate() {
        while next_exc <= sf.exception {
            push_exc_header(&mut lines, next_exc);
            next_exc += 1;
        }
        let selected = idx == d.frame;
        if selected {
            sel_start = lines.len();
        }
        let fr = sf.frame;
        let marker = if selected { Span::styled("▶ ", th::accent()) } else { Span::raw("  ") };
        let dot =
            if fr.in_app { Span::styled("● ", Style::new().fg(th::YELLOW)) } else { Span::styled("  ", th::faint()) };
        let (fs, ls) = if fr.in_app { (th::bold(), Style::new().fg(th::ACCENT2)) } else { (th::dim(), th::faint()) };
        let mut line = Line::from(vec![
            marker,
            dot,
            Span::styled(fr.function_name().to_string(), fs),
            Span::styled("  ", th::faint()),
            Span::styled(
                fmt::truncate_left(&fr.location(), width.saturating_sub(6 + fmt::width(fr.function_name()))),
                ls,
            ),
        ]);
        if selected {
            line = line.style(Style::new().bg(th::SELECTED));
        }
        lines.push(line);
        if selected {
            if fr.context_line.is_some() {
                for (n, text, is_err) in fr.context() {
                    let num = n.map(|n| format!("{n:>6}")).unwrap_or_else(|| "      ".into());
                    let body = fmt::truncate(&text.replace('\t', "    "), width.saturating_sub(12));
                    if is_err {
                        let pad = width.saturating_sub(12 + fmt::width(&body));
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("  {num} → "),
                                Style::new().fg(th::YELLOW).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                format!("{body}{}", " ".repeat(pad)),
                                Style::new()
                                    .fg(th::TEXT)
                                    .bg(ratatui::style::Color::Indexed(52))
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]));
                    } else {
                        lines.push(Line::from(vec![
                            Span::styled(format!("  {num} │ "), th::faint()),
                            Span::styled(body, th::dim()),
                        ]));
                    }
                }
            } else {
                lines.push(Line::from(Span::styled("           no source context for this frame", th::faint())));
            }
            sel_end = lines.len().saturating_sub(1);
        }
    }
    for i in next_exc..d.exceptions.len() {
        push_exc_header(&mut lines, i);
    }
    if frames.is_empty() {
        lines.push(Line::raw(""));
        let hint = if d.in_app_only { "no in-app frames · press a to show all frames" } else { "no stack frames" };
        lines.push(Line::from(Span::styled(hint, th::dim())));
    }

    let h = inner.height as usize;
    if sel_start < d.stack_scroll {
        d.stack_scroll = sel_start.saturating_sub(1);
    } else if sel_end >= d.stack_scroll + h {
        d.stack_scroll = (sel_end + 1).saturating_sub(h).min(sel_start);
    }
    d.stack_scroll = d.stack_scroll.min(lines.len().saturating_sub(h));
    f.render_widget(Paragraph::new(lines).scroll((u16_sat(d.stack_scroll), 0)), inner);
}

fn render_breadcrumbs(f: &mut Frame, area: Rect, d: &mut Detail) {
    let block = rounded(vec![title_span("Breadcrumbs"), Span::styled("newest first ", th::faint())]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if d.event.is_none() {
        f.render_widget(load_state(d.event_error.as_deref(), "loading event…"), inner);
        return;
    }
    if d.crumbs.is_empty() {
        let t = "This event has no breadcrumbs.";
        f.render_widget(Paragraph::new(Span::styled(t, th::dim())).alignment(Alignment::Center), inner);
        return;
    }
    let rows: Vec<Row> = d
        .crumbs
        .iter()
        .map(|b| {
            Row::new(vec![
                Cell::from(Span::styled(
                    b.timestamp.map(|t| t.format("%H:%M:%S%.3f").to_string()).unwrap_or_default(),
                    th::dim(),
                )),
                Cell::from(Span::styled(b.level.clone(), Style::new().fg(th::level_color(&b.level)))),
                Cell::from(Span::styled(
                    b.category.clone().or(b.ty.clone()).unwrap_or_default(),
                    Style::new().fg(th::ACCENT2),
                )),
                Cell::from(Span::styled(b.text(), th::text())),
            ])
        })
        .collect();
    let table =
        Table::new(rows, [Constraint::Length(12), Constraint::Length(8), Constraint::Length(18), Constraint::Min(10)])
            .header(Row::new(["TIME", "LEVEL", "CATEGORY", "MESSAGE"]).style(th::faint().bold()))
            .row_highlight_style(th::selected_row())
            .highlight_symbol(Span::styled("▌", th::accent()))
            .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(table, inner, &mut d.crumbs_state);
}

fn render_events(f: &mut Frame, area: Rect, d: &mut Detail, now: chrono::DateTime<chrono::Utc>) {
    let more = if d.next_cursor.is_some() { "· more below " } else { "" };
    let block = rounded(vec![title_span("Events"), Span::styled(format!("newest first · ⏎ show {more}"), th::faint())]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if !d.events_loaded {
        f.render_widget(load_state(d.events_error.as_deref(), "loading events…"), inner);
        return;
    }
    if d.events.is_empty() {
        let t = "No events in this issue.";
        f.render_widget(Paragraph::new(Span::styled(t, th::dim())).alignment(Alignment::Center), inner);
        return;
    }
    let shown = d.event_id.clone();
    let rows: Vec<Row> = d
        .events
        .iter()
        .map(|e| {
            let mark = if shown.as_deref() == Some(e.event_id.as_str()) {
                Span::styled("●", th::accent())
            } else {
                Span::raw(" ")
            };
            Row::new(vec![
                Cell::from(mark),
                Cell::from(Span::styled(fmt::relative_time(e.timestamp, now), th::text())),
                Cell::from(Span::styled(fmt::truncate(&e.event_id, 10), th::dim())),
                Cell::from(Span::styled(e.environment.clone().unwrap_or_default(), Style::new().fg(th::ACCENT2))),
                Cell::from(Span::styled(e.release.clone().unwrap_or_default(), th::text())),
                Cell::from(Span::styled(e.user.clone().unwrap_or_default(), th::dim())),
                Cell::from(Span::styled(e.message.clone().unwrap_or_else(|| e.title.clone()), th::text())),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(9),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(14),
            Constraint::Length(16),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(["", "WHEN", "EVENT", "ENV", "RELEASE", "USER", "MESSAGE"]).style(th::faint().bold()))
    .row_highlight_style(th::selected_row())
    .highlight_symbol(Span::styled("▌", th::accent()))
    .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(table, inner, &mut d.events_state);
}

/// Very small JSON highlighter for pretty-printed (one key per line) JSON.
pub(super) fn json_line(line: &str) -> Line<'static> {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);
    let mut spans = vec![Span::raw(indent.to_string())];
    let mut value = rest;
    if let Some(pos) = rest.find("\": ").filter(|_| rest.starts_with('"')) {
        spans.push(Span::styled(rest[..pos + 1].to_string(), Style::new().fg(th::ACCENT)));
        spans.push(Span::styled(": ", th::faint()));
        value = &rest[pos + 3..];
    }
    let (v, comma) = match value.strip_suffix(',') {
        Some(v) => (v, ","),
        None => (value, ""),
    };
    let style = if v.starts_with('"') {
        Style::new().fg(th::GREEN)
    } else if v == "true" || v == "false" || v == "null" {
        Style::new().fg(th::MAGENTA)
    } else if v.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        Style::new().fg(th::YELLOW)
    } else {
        th::faint()
    };
    spans.push(Span::styled(v.to_string(), style));
    spans.push(Span::styled(comma, th::faint()));
    Line::from(spans)
}

fn render_json(f: &mut Frame, area: Rect, d: &mut Detail) {
    let block = rounded(vec![title_span("JSON"), Span::styled("full event payload ", th::faint())]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if d.event.is_none() {
        f.render_widget(load_state(d.event_error.as_deref(), "loading event…"), inner);
        return;
    }
    let h = inner.height as usize;
    d.max_scroll = d.json.len().saturating_sub(h);
    d.scroll = d.scroll.min(d.max_scroll);
    let num_w = d.json.len().to_string().len();
    let lines: Vec<Line> = d
        .json
        .iter()
        .enumerate()
        .skip(d.scroll)
        .take(h)
        .map(|(i, l)| {
            let mut line = json_line(l);
            line.spans.insert(0, Span::styled(format!("{:>num_w$} ", i + 1), th::faint()));
            line
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}
