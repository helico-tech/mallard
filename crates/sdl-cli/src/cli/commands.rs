//! The commands: fetch with `sdl-client`, print tables or JSON.

use super::{Ctx, classify};
use crate::concise::ConciseEvent;
use crate::event_view as ev;
use crate::fmt;
use crate::paint as p;
use crate::{EventsCmd, IssueListArgs, IssuesCmd, ProjectsCmd, SetStatusArgs, SqlArgs};
use anyhow::Result;
use chrono::Utc;
use comfy_table::{Attribute, Cell, Color, Table, presets};
use mallard_term::color::Out;
use mallard_term::error::Hints;
use mallard_term::output::List;
use mallard_term::table::{fancy, right, table};
use mallard_term::wait::until;
use mallard_term::window::{Window, WindowArgs};
use sdl_client::types::*;
use serde::Serialize;
use std::time::Duration;

const DAY: Duration = Duration::from_secs(86_400);

/// Default window of issue lists and details (the server's default too).
const ISSUE_WINDOW: Duration = Duration::from_secs(14 * 86_400);

/// Table with a line between rows, for multi-line cells (on a terminal).
fn table_with_row_lines(headers: &[&str]) -> Table {
    let mut t = table(headers);
    if fancy() {
        t.load_style(presets::UTF8_FULL.with_rounded_corners());
    }
    t
}

fn range(w: &Window) -> WindowRange {
    WindowRange { from: Some(w.from), to: Some(w.to), step: None }
}

fn level_cell(l: &str) -> Cell {
    let c = Cell::new(l);
    match l {
        "fatal" => c.fg(Color::Magenta),
        "error" => c.fg(Color::Red),
        "warning" => c.fg(Color::Yellow),
        "info" => c.fg(Color::Blue),
        _ => c.fg(Color::DarkGrey),
    }
}

fn status_cell(s: IssueStatus) -> Cell {
    let c = Cell::new(s.as_str());
    match s {
        IssueStatus::Unresolved => c.fg(Color::Yellow),
        IssueStatus::Resolved => c.fg(Color::Green),
        IssueStatus::Ignored => c.fg(Color::DarkGrey),
    }
}

fn kv(o: Out, key: &str, value: &str) -> Result<()> {
    // Pad before coloring: escape codes would count as width.
    out!(o, "  {} {value}", p::dim(&fmt::pad(key, 14)));
    Ok(())
}

pub(super) async fn info(ctx: &Ctx) -> Result<()> {
    let o = ctx.out;
    let i = ctx.client.info().await.map_err(|e| ctx.err(e))?;
    if !ctx.output.text() {
        return ctx.print_json(&i);
    }
    let i = fmt::sanitize(i, true)?;
    out!(o, "{} {}", p::bold("sentry-ducklake"), i.version);
    kv(o, "server", ctx.url())?;
    kv(o, "duckdb", &i.duckdb_version)?;
    kv(o, "catalog", &i.catalog)?;
    kv(o, "storage", &i.storage)?;
    kv(o, "queue", &format!("{} / {}", i.queue_depth, i.queue_capacity))?;
    Ok(())
}

pub(super) async fn projects(ctx: &Ctx, cmd: ProjectsCmd) -> Result<()> {
    let o = ctx.out;
    match cmd {
        ProjectsCmd::List => {
            let ps = ctx.client.projects().await.map_err(|e| ctx.err(e))?;
            if !ctx.output.text() {
                return ctx.output.list(&List::new(ps));
            }
            let ps = fmt::sanitize(ps, true)?;
            if ps.is_empty() {
                out!(o, "no projects yet; create one with: {} projects create <name>", ctx.hints.command);
                return Ok(());
            }
            let mut t = table(&["ID", "SLUG", "NAME", "PLATFORM", "DSN"]);
            for p in &ps {
                t.add_row(vec![
                    right(p.id),
                    Cell::new(&p.slug).add_attribute(Attribute::Bold),
                    Cell::new(&p.name),
                    Cell::new(p.platform.as_deref().unwrap_or("-")),
                    Cell::new(&p.dsn).fg(Color::Cyan),
                ]);
            }
            out!(o, "{t}");
            Ok(())
        }
        ProjectsCmd::Create { name, slug, platform, if_not_exists } => {
            let (pr, created) = if if_not_exists {
                create_or_get(ctx, &name, slug, platform).await?
            } else {
                let req = CreateProject { name, slug, platform };
                (ctx.client.create_project(&req).await.map_err(|e| ctx.err(e))?, true)
            };
            if !ctx.output.text() {
                return ctx.print_json(&pr);
            }
            let pr = fmt::sanitize(pr, true)?;
            let what = if created { "created project" } else { "project exists:" };
            out!(o, "{} {what} {} ({})", p::green("✓"), p::bold(&pr.slug), pr.id);
            print_project(ctx.out, &pr, &ctx.hints)?;
            Ok(())
        }
        ProjectsCmd::Show { project } => {
            let pr = ctx.client.project(&project).await.map_err(|e| ctx.err(e))?;
            if !ctx.output.text() {
                return ctx.print_json(&pr);
            }
            let pr = fmt::sanitize(pr, true)?;
            print_project(ctx.out, &pr, &ctx.hints)?;
            Ok(())
        }
    }
}

/// The project with this slug (derived from the name when not given),
/// created when it does not exist. `true` when it was created.
pub async fn create_or_get(
    ctx: &Ctx,
    name: &str,
    slug: Option<String>,
    platform: Option<String>,
) -> Result<(Project, bool)> {
    let slug = slug.unwrap_or_else(|| slugify(name));
    match ctx.client.project(&slug).await {
        Ok(p) => return Ok((p, false)),
        Err(e) if e.is_not_found() => {}
        Err(e) => return Err(ctx.err(e)),
    }
    let req = CreateProject { name: name.to_string(), slug: Some(slug.clone()), platform };
    match ctx.client.create_project(&req).await {
        Ok(p) => Ok((p, true)),
        // Created by someone else in between.
        Err(e) if e.status() == Some(409) => Ok((ctx.client.project(&slug).await.map_err(|e| ctx.err(e))?, false)),
        Err(e) => Err(ctx.err(e)),
    }
}

fn print_project(o: Out, pr: &Project, h: &Hints) -> Result<()> {
    out!(o);
    out!(o, "  {}", p::dim("DSN"));
    out!(o, "  {}", p::bold(&p::cyan(&pr.dsn)));
    out!(o);
    kv(o, "id", &pr.id.to_string())?;
    kv(o, "slug", &pr.slug)?;
    kv(o, "name", &pr.name)?;
    kv(o, "platform", pr.platform.as_deref().unwrap_or("-"))?;
    kv(o, "public key", &pr.public_key)?;
    kv(o, "created", &fmt::timestamp(pr.created_at))?;
    out!(o);
    out!(o, "  {}", p::dim(&format!("test it: {} send-test --dsn {}", h.command, pr.dsn)));
    Ok(())
}

fn issue_query(a: &IssueListArgs) -> IssueQuery {
    IssueQuery {
        status: Some(a.status.as_str().into()),
        query: a.query.clone(),
        since: None,
        sort: Some(a.sort.as_str().into()),
        environment: a.env.clone(),
        release: a.release.clone(),
        limit: a.limit,
        new: a.new.then_some(true),
        first_release: a.first_release.clone(),
        tag: a.tag.clone(),
        cursor: a.cursor.clone(),
    }
}

/// `issues show` in JSON: the issue and its latest event, concise.
#[derive(Serialize)]
struct IssueShow {
    #[serde(flatten)]
    detail: IssueDetail,
    latest_event: Option<ConciseEvent>,
}

/// One line of `issues resolve|ignore|unresolve` in JSON.
#[derive(Serialize)]
struct StatusResult {
    id: String,
    status: Option<IssueStatus>,
    changed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<mallard_term::error::ErrorBody>,
}

pub(super) async fn issues(ctx: &Ctx, cmd: IssuesCmd) -> Result<()> {
    let o = ctx.out;
    let project = ctx.resolve_project().await?;
    match cmd {
        IssuesCmd::List(a) => {
            let w = a.window.resolve(ISSUE_WINDOW)?;
            let list = ctx.client.issues_in(&project, &issue_query(&a), &range(&w)).await.map_err(|e| ctx.err(e))?;
            let w = Window { from: list.window_start, to: list.window_end };
            let limit = Some(a.limit.unwrap_or(50).clamp(1, 500));
            let page = List::new(list.issues.clone()).limit(limit).cursor(list.next_cursor.clone()).window(w);
            if !ctx.output.text() {
                return ctx.output.list(&page);
            }
            let list = fmt::sanitize(list, true)?;
            if list.issues.is_empty() {
                out!(o, "no issues match (window {} .. now)", fmt::timestamp(list.window_start));
                return Ok(());
            }
            let now = Utc::now();
            // Piped: one row per line, no sparkline.
            let fancy = fancy();
            let mut headers = vec!["ID", "LEVEL", "ISSUE", "EVENTS", "USERS", "TREND", "LAST SEEN", "STATUS"];
            if !fancy {
                headers.retain(|h| *h != "TREND");
            }
            let mut t = table_with_row_lines(&headers);
            for i in &list.issues {
                let sep = if fancy { "\n" } else { " · " };
                let title = match &i.culprit {
                    Some(c) if !c.is_empty() => format!("{}{sep}{}", i.title, c),
                    _ => i.title.clone(),
                };
                let mut row = vec![
                    Cell::new(&i.id).fg(Color::DarkGrey),
                    level_cell(&i.level),
                    Cell::new(title),
                    right(fmt::compact(i.count)),
                    right(fmt::compact(i.user_count)),
                ];
                if fancy {
                    row.push(Cell::new(fmt::sparkline(&fmt::resample(&i.trend, 12))).fg(Color::Magenta));
                }
                row.extend([Cell::new(fmt::relative_time(i.last_seen, now)), status_cell(i.status)]);
                t.add_row(row);
            }
            out!(o, "{t}");
            out!(
                o,
                "{}",
                p::dim(&format!("{} issues · window {} .. now", list.issues.len(), fmt::timestamp(list.window_start)))
            );
            ctx.output.truncation_note(&page);
            Ok(())
        }
        IssuesCmd::Show { id, window } => {
            let w = window.resolve(ISSUE_WINDOW)?;
            let d = ctx
                .client
                .issue_in(&project, &id, &WindowQuery::default(), &range(&w))
                .await
                .map_err(|e| ctx.err(e))?;
            if !ctx.output.text() {
                let latest_event = match &d.latest_event_id {
                    Some(e) => match ctx.client.event(&project, e, Some(&d.summary.id)).await {
                        Ok(e) => Some(ConciseEvent::new(e, ctx.output.raw)),
                        Err(e) if e.is_not_found() => None,
                        Err(e) => return Err(ctx.err(e)),
                    },
                    None => None,
                };
                return ctx.print_json(&IssueShow { detail: d, latest_event });
            }
            print_issue(ctx.out, &fmt::sanitize(d, true)?, &fmt::clean(&project), &ctx.hints)?;
            Ok(())
        }
        IssuesCmd::Resolve(a) => set_status(ctx, &project, a, IssueStatus::Resolved).await,
        IssuesCmd::Ignore(a) => set_status(ctx, &project, a, IssueStatus::Ignored).await,
        IssuesCmd::Unresolve(a) => set_status(ctx, &project, a, IssueStatus::Unresolved).await,
    }
}

fn print_issue(o: Out, d: &IssueDetail, project: &str, h: &Hints) -> Result<()> {
    let s = &d.summary;
    let now = Utc::now();
    out!(o, "{} {}", p::level(&format!("[{}]", s.level)), p::bold(&s.title));
    if let Some(c) = &s.culprit {
        out!(o, "  {}", p::dim(c));
    }
    out!(o);
    kv(o, "id", &s.id)?;
    kv(o, "status", &p::status(s.status.as_str()))?;
    kv(o, "platform", &s.platform)?;
    kv(o, "events", &s.count.to_string())?;
    kv(o, "users", &s.user_count.to_string())?;
    kv(o, "first seen", &format!("{} ({})", fmt::timestamp(s.first_seen), fmt::relative_time(s.first_seen, now)))?;
    kv(o, "last seen", &format!("{} ({})", fmt::timestamp(s.last_seen), fmt::relative_time(s.last_seen, now)))?;
    kv(o, "first release", d.first_release.as_deref().unwrap_or("-"))?;
    kv(o, "last release", d.last_release.as_deref().unwrap_or("-"))?;
    kv(o, "latest event", d.latest_event_id.as_deref().unwrap_or("-"))?;
    if fancy() {
        kv(o, "trend", &p::magenta(&fmt::sparkline(&s.trend)))?;
    }
    if !d.tags.is_empty() {
        out!(o);
        out!(o, "{}", p::bold("Tags"));
        for f in &d.tags {
            out!(o, "  {} {}", p::bold(&f.key), p::dim(&format!("({} unique)", f.unique)));
            let total: u64 = f.top.iter().map(|v| v.count).sum::<u64>().max(1);
            for v in &f.top {
                let pct = v.count as f64 * 100.0 / total as f64;
                out!(
                    o,
                    "    {} {} {:>5.1}%  {}",
                    fmt::pad(&fmt::truncate(&v.value, 28), 28),
                    p::cyan(&fmt::pad(&fmt::hbar(v.count, total, 20), 20)),
                    pct,
                    p::dim(&v.count.to_string())
                );
            }
        }
    }
    if !d.activity.is_empty() {
        out!(o);
        out!(o, "{}", p::bold("Activity"));
        for a in &d.activity {
            let status = p::status(&fmt::pad(a.status.as_str(), 10));
            out!(o, "  {}  {status} {}", p::dim(&fmt::timestamp(a.at)), p::dim(&a.reason));
        }
    }
    if let Some(e) = &d.latest_event_id {
        out!(o);
        let cmd = h.command;
        out!(o, "{}", p::dim(&format!("latest event: {cmd} events show {e} --issue {} -p {project}", s.id)));
    }
    Ok(())
}

async fn set_status(ctx: &Ctx, project: &str, a: SetStatusArgs, status: IssueStatus) -> Result<()> {
    let o = ctx.out;
    let mut last_err = None;
    let mut results = Vec::new();
    let req = UpdateIssue { status, reason: a.reason.clone() };
    for id in &a.ids {
        match ctx.client.update_issue(project, id, &req).await {
            Ok(d) => {
                // Servers before `changed` was added: treat as changed.
                let changed = d.changed.unwrap_or(true);
                if ctx.output.text() {
                    let (mark, what) = if changed {
                        (p::green("✓"), status.as_str().to_string())
                    } else {
                        (p::dim("·"), format!("already {status}"))
                    };
                    out!(
                        o,
                        "{mark} {what} {}  {}",
                        p::bold(id),
                        p::dim(&fmt::truncate(&fmt::clean(&d.summary.title), 70))
                    );
                }
                results.push(StatusResult {
                    id: id.clone(),
                    status: Some(d.summary.status),
                    changed: Some(changed),
                    error: None,
                });
            }
            Err(e) => {
                let d = classify(&e, ctx.url(), &ctx.hints);
                if ctx.output.text() {
                    eprintln!("{} {}: {d}", p::red("✗"), fmt::clean(id));
                }
                results.push(StatusResult { id: id.clone(), status: None, changed: None, error: Some(d.body()) });
                last_err = Some(d);
            }
        }
    }
    if !ctx.output.text() {
        ctx.print_json(&serde_json::json!({ "results": results }))?;
    }
    if let Some(d) = last_err {
        let failed = results.iter().filter(|r| r.error.is_some()).count();
        let msg = format!("{failed} of {} issues not updated: {}", a.ids.len(), d.message);
        let mut e = mallard_term::error::Described::new(d.kind, msg);
        e.hint = d.hint;
        e.status = d.status;
        return Err(anyhow::Error::new(e));
    }
    Ok(())
}

pub(super) async fn events(ctx: &Ctx, cmd: EventsCmd) -> Result<()> {
    let o = ctx.out;
    match cmd {
        EventsCmd::List { issue, trace, limit, window, tag, release, env, query, cursor, wait } => {
            let by_trace = trace.is_some();
            let project = if by_trace { None } else { Some(ctx.resolve_project().await?) };
            // Resolved on every try: with --wait the window moves along.
            let fetch = || async {
                // Trace ids are searched in the last 30 days by default.
                let w = if by_trace {
                    Some(window.resolve(30 * DAY)?)
                } else {
                    window.is_set().then(|| window.resolve(DAY)).transpose()?
                };
                let r = w.as_ref().map(range).unwrap_or_default();
                let list = match (&trace, &project) {
                    (Some(trace_id), _) => {
                        let q = TraceQuery { trace_id: trace_id.clone(), limit, ..Default::default() };
                        ctx.client.events_by_trace_in(&q, &r).await
                    }
                    (None, project) => {
                        let q = EventQuery {
                            issue: issue.clone(),
                            limit,
                            tag: tag.clone(),
                            release: release.clone(),
                            environment: env.clone(),
                            query: query.clone(),
                            before: cursor.clone(),
                            ..Default::default()
                        };
                        ctx.client.events_in(project.as_deref().unwrap_or_default(), &q, &r).await
                    }
                }
                .map_err(|e| ctx.err(e))?;
                anyhow::Ok((w, list))
            };
            let ((w, list), waited) = until(&ctx.output, &wait, fetch, |(_, l)| !l.events.is_empty()).await?.split();
            let mut page = List::new(list.events.clone())
                .limit(Some(limit.unwrap_or(50).clamp(1, 500)))
                .cursor(list.next_cursor.clone());
            if let Some(w) = w {
                page = page.window(w);
            }
            if !ctx.output.text() {
                ctx.output.list(&page)?;
                return waited;
            }
            let list = fmt::sanitize(list, true)?;
            if list.events.is_empty() {
                out!(o, "no events");
                return waited;
            }
            let now = Utc::now();
            let mut headers = vec!["EVENT", "ISSUE", "LEVEL", "TITLE", "ENV", "RELEASE", "USER", "WHEN"];
            if by_trace {
                headers.insert(0, "PROJECT");
            }
            let mut t = table(&headers);
            for e in &list.events {
                let mut row = vec![];
                if by_trace {
                    row.push(right(e.project_id));
                }
                row.extend([
                    Cell::new(&e.event_id).fg(Color::DarkGrey),
                    Cell::new(&e.issue_id).fg(Color::DarkGrey),
                    level_cell(&e.level),
                    Cell::new(&e.title),
                    Cell::new(e.environment.as_deref().unwrap_or("-")),
                    Cell::new(e.release.as_deref().unwrap_or("-")),
                    Cell::new(e.user.as_deref().unwrap_or("-")),
                    Cell::new(fmt::relative_time(e.timestamp, now)),
                ]);
                t.add_row(row);
            }
            out!(o, "{t}");
            ctx.output.truncation_note(&page);
            Ok(())
        }
        EventsCmd::Show { event_id, issue } => {
            let project = ctx.resolve_project().await?;
            let e = ctx.client.event(&project, &event_id, issue.as_deref()).await.map_err(|e| ctx.err(e))?;
            if !ctx.output.text() {
                return ctx.print_json(&ConciseEvent::new(e, ctx.output.raw));
            }
            print_event(ctx.out, &fmt::sanitize(e, true)?)?;
            Ok(())
        }
    }
}

fn section(o: Out, title: &str) -> Result<()> {
    out!(o);
    out!(o, "{}", p::bold(&p::cyan(&format!("── {title} "))));
    Ok(())
}

fn print_event(o: Out, e: &EventDetail) -> Result<()> {
    let s = &e.summary;
    out!(o, "{} {}", p::level(&format!("[{}]", s.level)), p::bold(&s.title));
    if let Some(c) = &e.culprit {
        out!(o, "  {}", p::dim(c));
    }
    out!(o);
    kv(o, "event", &s.event_id)?;
    kv(o, "issue", &s.issue_id)?;
    kv(o, "time", &format!("{} ({})", fmt::timestamp(s.timestamp), fmt::relative_time(s.timestamp, Utc::now())))?;
    kv(o, "platform", &s.platform)?;
    if let Some(v) = &s.environment {
        kv(o, "environment", v)?;
    }
    if let Some(v) = &s.release {
        kv(o, "release", v)?;
    }
    if let Some(v) = &s.trace_id {
        kv(o, "trace", &format!("{v}{}", s.span_id.as_ref().map(|sp| format!("  span {sp}")).unwrap_or_default()))?;
    }
    for (k, v) in ev::contexts(&e.payload) {
        kv(o, &k, &v)?;
    }
    if let Some(m) = ev::message(&e.payload) {
        section(o, "Message")?;
        out!(o, "  {m}");
    }

    let exceptions = ev::exceptions(&e.payload);
    for (i, x) in exceptions.iter().enumerate() {
        section(o, if i == 0 { "Exception" } else { "Caused by" })?;
        let mut head = p::bold(&p::red(&x.headline()));
        if let Some(m) = x.mechanism_label() {
            head.push_str(&p::dim(&format!("  ({m})")));
        }
        out!(o, "  {head}");
        let frames = x.frames_newest_first(false);
        if frames.is_empty() {
            out!(o, "  {}", p::dim("(no stack trace)"));
        }
        for f in frames {
            if f.in_app {
                out!(o, "    {} {}  {}", p::yellow("▸"), p::bold(f.function_name()), p::cyan(&f.location()));
                for (n, line, is_err) in f.context() {
                    let num = n.map(|n| format!("{n:>5}")).unwrap_or_else(|| "     ".into());
                    if is_err {
                        out!(o, "      {} {} {}", p::highlight(&num), p::yellow("→"), p::highlight(&line));
                    } else {
                        out!(o, "      {}   {}", p::dim(&num), p::dim(&line));
                    }
                }
            } else {
                out!(o, "    {}", p::dim(&format!("  at {}  {}", f.function_name(), f.location())));
                if let Some(c) = &f.context_line {
                    out!(o, "      {}", p::dim(&format!("      {}", c.trim())));
                }
            }
        }
    }

    let crumbs = ev::breadcrumbs(&e.payload);
    if !crumbs.is_empty() {
        section(o, "Breadcrumbs (newest first)")?;
        for b in &crumbs {
            let time =
                b.timestamp.map(|t| t.format("%H:%M:%S%.3f").to_string()).unwrap_or_else(|| "            ".into());
            let cat = b.category.clone().or_else(|| b.ty.clone()).unwrap_or_default();
            out!(
                o,
                "  {}  {} {} {}",
                p::dim(&time),
                p::level(&fmt::pad(&b.level, 8)),
                p::cyan(&fmt::pad(&fmt::truncate(&cat, 16), 16)),
                b.text()
            );
        }
    }

    if !e.tags.is_empty() {
        section(o, "Tags")?;
        let w = e.tags.iter().map(|(k, _)| fmt::width(k)).max().unwrap_or(0).min(24);
        for (k, v) in &e.tags {
            out!(o, "  {}  {v}", p::dim(&fmt::pad(k, w)));
        }
    }

    let user = ev::user(&e.payload);
    if !user.is_empty() {
        section(o, "User")?;
        for (k, v) in &user {
            kv(o, k, v)?;
        }
    }

    if let Some(r) = ev::request(&e.payload) {
        section(o, "Request")?;
        out!(o, "  {} {}", p::bold(r.method.as_deref().unwrap_or("")), r.url.as_deref().unwrap_or(""));
        if let Some(q) = &r.query {
            kv(o, "query", q)?;
        }
        for (k, v) in &r.headers {
            kv(o, k, v)?;
        }
    }
    Ok(())
}

pub(super) async fn stats(ctx: &Ctx, window: WindowArgs, interval: Option<String>) -> Result<()> {
    let o = ctx.out;
    let project = ctx.resolve_project().await?;
    let w = window.resolve(DAY)?;
    let s = ctx
        .client
        .stats_in(&project, &WindowQuery { since: None, interval }, &range(&w))
        .await
        .map_err(|e| ctx.err(e))?;
    if !ctx.output.text() {
        return ctx.print_json(&s);
    }
    let s = fmt::sanitize(s, true)?;
    out!(
        o,
        "{} {}",
        p::bold("Project stats"),
        p::dim(&format!(
            "{} .. {} · buckets of {}",
            fmt::timestamp(s.window_start),
            fmt::timestamp(s.window_end),
            fmt::duration(s.interval_seconds)
        ))
    );
    out!(o);
    out!(
        o,
        "  {}  {}    {}  {}    {}  {}    {}  {}",
        p::dim("events"),
        p::bold(&s.total_events.to_string()),
        p::dim("issues"),
        p::bold(&s.unique_issues.to_string()),
        p::dim("new issues"),
        p::bold(&s.new_issues.to_string()),
        p::dim("affected users"),
        p::bold(&s.affected_users.to_string())
    );
    let events: Vec<u64> = s.buckets.iter().map(|b| b.events).collect();
    if fancy() {
        out!(o);
        out!(o, "  {}", p::magenta(&fmt::sparkline(&events)));
    }
    out!(o);
    let max = events.iter().copied().max().unwrap_or(0);
    for b in &s.buckets {
        out!(
            o,
            "  {}  {:>7}  {}{}",
            p::dim(&b.start.format("%m-%d %H:%M").to_string()),
            b.events,
            p::red(&fmt::hbar(b.errors, max, 40)),
            p::yellow(&fmt::hbar(b.warnings + b.other, max, 40))
        );
    }
    out!(o);
    out!(o, "  {} errors  {} warnings/other", p::red("█"), p::yellow("█"));
    Ok(())
}

pub(super) async fn releases(ctx: &Ctx, window: WindowArgs, limit: Option<u32>) -> Result<()> {
    let o = ctx.out;
    let project = ctx.resolve_project().await?;
    let w = window.resolve(ISSUE_WINDOW)?;
    let list = ctx
        .client
        .releases(&project, &ReleaseQuery { since: None, limit }, &range(&w))
        .await
        .map_err(|e| ctx.err(e))?;
    if !ctx.output.text() {
        let w = Window { from: list.window_start, to: list.window_end };
        let limit = Some(limit.unwrap_or(50).clamp(1, 500));
        return ctx.output.list(&List::new(list.releases).limit(limit).window(w));
    }
    let list = fmt::sanitize(list, true)?;
    if list.releases.is_empty() {
        out!(o, "no releases in events (window {} .. now)", fmt::timestamp(list.window_start));
        return Ok(());
    }
    let now = Utc::now();
    let mut t = table(&["RELEASE", "EVENTS", "ISSUES", "NEW ISSUES", "FIRST SEEN", "LAST SEEN"]);
    for r in &list.releases {
        t.add_row(vec![
            Cell::new(&r.version).add_attribute(Attribute::Bold),
            right(fmt::compact(r.events)),
            right(fmt::compact(r.issues)),
            right(fmt::compact(r.new_issues)),
            Cell::new(fmt::relative_time(r.first_seen, now)),
            Cell::new(fmt::relative_time(r.last_seen, now)),
        ]);
    }
    out!(o, "{t}");
    Ok(())
}

pub(super) async fn sql(ctx: &Ctx, a: SqlArgs) -> Result<()> {
    let o = ctx.out;
    let r = ctx.client.sql(&a.query, Some(a.limit)).await.map_err(|e| ctx.err(e))?;
    if !ctx.output.text() {
        return ctx.print_json(&r);
    }
    let headers: Vec<String> = r.columns.iter().map(|c| fmt::clean(&c.name).into_owned()).collect();
    let headers: Vec<&str> = headers.iter().map(String::as_str).collect();
    let mut t = table(&headers);
    for row in &r.rows {
        t.add_row(row.iter().map(|v| match v {
            serde_json::Value::String(s) => Cell::new(fmt::clean(s)),
            serde_json::Value::Null => Cell::new("NULL").fg(Color::DarkGrey),
            serde_json::Value::Number(n) => right(n),
            // Nested values print as JSON: strings in it are escaped.
            other => Cell::new(mallard_term::text::json_safe(&other.to_string())),
        }));
    }
    out!(o, "{t}");
    let more = if r.truncated { " (truncated; raise --limit)" } else { "" };
    out!(o, "{}", p::dim(&format!("{} rows{more} in {:.1}ms", r.rows.len(), r.elapsed_ms)));
    Ok(())
}

pub(super) async fn sourcemaps(ctx: &Ctx) -> Result<()> {
    let o = ctx.out;
    let project = ctx.resolve_project().await?;
    let list = ctx.client.sourcemaps(&project).await.map_err(|e| ctx.err(e))?;
    if !ctx.output.text() {
        return ctx.output.list(&List::new(list));
    }
    let list = fmt::sanitize(list, true)?;
    if list.is_empty() {
        out!(o, "no sourcemaps uploaded (upload with sentry-cli sourcemaps upload)");
        return Ok(());
    }
    let mut t = table(&["BUNDLE", "RELEASE", "DIST", "FILES", "SIZE", "DEBUG IDS", "UPLOADED"]);
    for b in &list {
        t.add_row(vec![
            Cell::new(fmt::truncate(&b.id, 12)).fg(Color::DarkGrey),
            Cell::new(b.release.as_deref().unwrap_or("-")),
            Cell::new(b.dist.as_deref().unwrap_or("-")),
            right(b.file_count),
            right(fmt::bytes(b.size_bytes)),
            right(b.debug_ids.len()),
            Cell::new(fmt::timestamp(b.created_at)),
        ]);
    }
    out!(o, "{t}");
    Ok(())
}

pub(super) async fn send_test(dsn: &str, message: &str, ctx: &Ctx) -> Result<()> {
    let o = ctx.out;
    let (d, id) = crate::send_test::send(dsn, message).await?;
    if !ctx.output.text() {
        ctx.print_json(&serde_json::json!({"id": id, "project_id": d.project_id, "url": d.envelope_url}))?;
    } else {
        out!(o, "{} sent test event {} to project {}", p::green("✓"), p::bold(&id), d.project_id);
        out!(o, "  {}", p::dim(&format!("POST {}", d.envelope_url)));
        out!(o, "  {}", p::dim(&format!("check it: {} events show {id} -p {}", ctx.hints.command, d.project_id)));
    }
    Ok(())
}
