//! Non-interactive commands: fetch with `sdl-client`, print tables or JSON.

use crate::event_view as ev;
use crate::fmt;
use crate::paint as p;
use crate::{Command, EventsCmd, IssueListArgs, IssuesCmd, ProjectsCmd, SourcemapsCmd};
use anyhow::{Result, bail};
use chrono::Utc;
use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table, modifiers, presets};
use sdl_client::Client;
use sdl_client::types::*;
use serde::Serialize;

/// `println!` to a locked stdout that returns the error instead of
/// panicking; `run` treats a closed pipe (`sdl issues list | head`) as success.
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        writeln!(std::io::stdout().lock(), $($arg)*)?
    }};
}

/// How the host names the commands and its env vars, for the hints in
/// messages ("create one with: sdl projects create <name>").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hints {
    /// What runs these commands, e.g. `sdl` or `mallard errors`.
    pub command: &'static str,
    pub url_env: &'static str,
    pub token_env: &'static str,
    pub project_env: &'static str,
}

impl Hints {
    /// The standalone `sdl` binary.
    pub const SDL: Hints =
        Hints { command: "sdl", url_env: "SDL_URL", token_env: "SDL_TOKEN", project_env: "SDL_PROJECT" };
}

impl Default for Hints {
    fn default() -> Self {
        Hints::SDL
    }
}

/// One-line, actionable message for a client error. Safe to print: server
/// text in it is stripped of control characters.
pub fn describe(e: &sdl_client::Error, url: &str, hints: &Hints) -> String {
    fmt::clean(&describe_raw(e, url, hints))
}

fn describe_raw(e: &sdl_client::Error, url: &str, hints: &Hints) -> String {
    match e {
        sdl_client::Error::Api { status: 401, .. } => format!("invalid token (set {} or --token)", hints.token_env),
        sdl_client::Error::Api { status: 403, .. } => {
            format!("token not allowed to do this (check {})", hints.token_env)
        }
        sdl_client::Error::Api { status: 404, message } => {
            if message.is_empty() {
                "not found".into()
            } else {
                format!("not found: {message}")
            }
        }
        sdl_client::Error::Api { status, message } if *status >= 500 => {
            format!("server error ({status}): {message}")
        }
        sdl_client::Error::Api { status, message } => {
            format!("request rejected ({status}): {message}")
        }
        sdl_client::Error::InvalidSegment(_) => format!("{e}: ids and slugs can't be empty, '.' or '..'"),
        sdl_client::Error::Url(m) => {
            format!("invalid server url {url:?}: {m} (set {} or --url)", hints.url_env)
        }
        sdl_client::Error::Http(h) if h.is_connect() => {
            format!("cannot reach server at {url} (is it running? set {})", hints.url_env)
        }
        sdl_client::Error::Http(h) if h.is_timeout() => format!("request to {url} timed out"),
        sdl_client::Error::Http(h) if h.is_decode() => {
            format!("unexpected response from {url} (version mismatch?): {h}")
        }
        sdl_client::Error::Http(h) => format!("request to {url} failed: {h}"),
        e => format!("request to {url} failed: {e}"),
    }
}

/// What the commands need besides their own arguments. A host that mounts
/// [`Command`] builds this from its own global flags:
/// `Ctx::new(client).with_project(p).with_json(true)`.
pub struct Ctx {
    pub(crate) client: Client,
    /// Project id or slug; optional when the server has only one project.
    pub(crate) project: Option<String>,
    /// Print raw JSON instead of tables.
    pub(crate) json: bool,
    pub(crate) hints: Hints,
}

impl Ctx {
    /// No project, table output, [`Hints::SDL`].
    pub fn new(client: Client) -> Self {
        Ctx { client, project: None, json: false, hints: Hints::SDL }
    }

    /// Project id or slug; optional when the server has only one project.
    pub fn with_project(mut self, project: Option<String>) -> Self {
        self.project = project;
        self
    }

    /// Print raw JSON instead of tables.
    pub fn with_json(mut self, json: bool) -> Self {
        self.json = json;
        self
    }

    pub fn with_hints(mut self, hints: Hints) -> Self {
        self.hints = hints;
        self
    }

    /// The friendly message, with the client error kept as its source so a
    /// host can still downcast it. The server's message in the source is
    /// cleaned too: hosts may print the whole chain.
    pub(crate) fn err(&self, e: sdl_client::Error) -> anyhow::Error {
        let msg = describe(&e, self.url(), &self.hints);
        let e = match e {
            sdl_client::Error::Api { status, message } => {
                sdl_client::Error::Api { status, message: fmt::clean(&message) }
            }
            e => e,
        };
        anyhow::Error::new(e).context(msg)
    }

    fn url(&self) -> &str {
        self.client.base_url()
    }

    /// The `--project`, or the only project on the server.
    pub(crate) async fn resolve_project(&self) -> Result<String> {
        if let Some(p) = &self.project {
            return Ok(p.clone());
        }
        let projects = self.client.projects().await.map_err(|e| self.err(e))?;
        let h = &self.hints;
        match projects.as_slice() {
            [one] => Ok(one.slug.clone()),
            [] => bail!("no projects yet (create one: {} projects create <name>)", h.command),
            many => {
                let slugs: Vec<String> = many.iter().map(|p| fmt::clean(&p.slug)).collect();
                bail!("several projects; pick one with -p or {} ({})", h.project_env, slugs.join(", "))
            }
        }
    }

    fn print_json<T: Serialize>(&self, v: &T) -> Result<()> {
        out!("{}", json_safe(&serde_json::to_string_pretty(v)?));
        Ok(())
    }
}

/// Run one command. Prints to stdout; errors are one-line, friendly messages.
/// All text that comes from events or the server is stripped of terminal
/// control characters before it is printed (except in `--json` output).
pub async fn run(cmd: Command, ctx: &Ctx) -> Result<()> {
    p::init();
    match run_inner(cmd, ctx).await {
        Err(e) if is_broken_pipe(&e) => Ok(()),
        r => r,
    }
}

/// serde escapes C0 control characters in JSON strings but writes DEL, C1
/// (e.g. U+009B, an 8-bit CSI) and bidi overrides as they are. Escape those
/// too; `\uXXXX` is the same JSON value, so the output stays lossless.
fn json_safe(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        // Raw newlines are the pretty-printer's own; inside strings serde escapes them.
        if (c.is_control() && c != '\n')
            || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        {
            out.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe))
}

async fn run_inner(cmd: Command, ctx: &Ctx) -> Result<()> {
    match cmd {
        Command::SendTest { dsn, message } => send_test(&dsn, &message, ctx).await,
        Command::Info => info(ctx).await,
        Command::Projects(c) => projects(ctx, c).await,
        Command::Issues(c) => issues(ctx, c).await,
        Command::Events(c) => events(ctx, c).await,
        Command::Stats { since, interval } => stats(ctx, since, interval).await,
        Command::Sourcemaps(SourcemapsCmd::List) => sourcemaps(ctx).await,
        Command::Maintenance => {
            ctx.client.maintenance().await.map_err(|e| ctx.err(e))?;
            if ctx.json {
                out!("{{\"ok\":true}}");
            } else {
                out!("{} maintenance done (flushed, compacted, expired)", p::green("✓"));
            }
            Ok(())
        }
        Command::Tui => crate::tui::run(ctx).await,
    }
}

fn table(headers: &[&str]) -> Table {
    let mut t = Table::new();
    t.load_preset(presets::UTF8_FULL_CONDENSED)
        .apply_modifier(modifiers::UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(headers.iter().map(|h| Cell::new(h).add_attribute(Attribute::Bold)));
    t
}

/// Table with a line between rows, for multi-line cells.
fn table_with_row_lines(headers: &[&str]) -> Table {
    let mut t = table(headers);
    t.load_preset(presets::UTF8_FULL).apply_modifier(modifiers::UTF8_ROUND_CORNERS);
    t
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

fn right(s: impl ToString) -> Cell {
    Cell::new(s).set_alignment(CellAlignment::Right)
}

fn kv(key: &str, value: &str) -> Result<()> {
    out!("  {:<14} {value}", p::dim(key));
    Ok(())
}

async fn info(ctx: &Ctx) -> Result<()> {
    let i = ctx.client.info().await.map_err(|e| ctx.err(e))?;
    if ctx.json {
        return ctx.print_json(&i);
    }
    let i = fmt::sanitize(i, true)?;
    out!("{} {}", p::bold("sentry-ducklake"), i.version);
    kv("server", ctx.url())?;
    kv("duckdb", &i.duckdb_version)?;
    kv("catalog", &i.catalog)?;
    kv("storage", &i.storage)?;
    kv("queue", &format!("{} / {}", i.queue_depth, i.queue_capacity))?;
    Ok(())
}

async fn projects(ctx: &Ctx, cmd: ProjectsCmd) -> Result<()> {
    match cmd {
        ProjectsCmd::List => {
            let ps = ctx.client.projects().await.map_err(|e| ctx.err(e))?;
            if ctx.json {
                return ctx.print_json(&ps);
            }
            let ps = fmt::sanitize(ps, true)?;
            if ps.is_empty() {
                out!("no projects yet; create one with: {} projects create <name>", ctx.hints.command);
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
            out!("{t}");
            Ok(())
        }
        ProjectsCmd::Create { name, slug, platform } => {
            let pr =
                ctx.client.create_project(&CreateProject { name, slug, platform }).await.map_err(|e| ctx.err(e))?;
            if ctx.json {
                return ctx.print_json(&pr);
            }
            let pr = fmt::sanitize(pr, true)?;
            out!("{} created project {} ({})", p::green("✓"), p::bold(&pr.slug), pr.id);
            print_project(&pr, &ctx.hints)?;
            Ok(())
        }
        ProjectsCmd::Show { project } => {
            let pr = ctx.client.project(&project).await.map_err(|e| ctx.err(e))?;
            if ctx.json {
                return ctx.print_json(&pr);
            }
            let pr = fmt::sanitize(pr, true)?;
            print_project(&pr, &ctx.hints)?;
            Ok(())
        }
    }
}

fn print_project(pr: &Project, h: &Hints) -> Result<()> {
    out!();
    out!("  {}", p::dim("DSN"));
    out!("  {}", p::bold(&p::cyan(&pr.dsn)));
    out!();
    kv("id", &pr.id.to_string())?;
    kv("slug", &pr.slug)?;
    kv("name", &pr.name)?;
    kv("platform", pr.platform.as_deref().unwrap_or("-"))?;
    kv("public key", &pr.public_key)?;
    kv("created", &fmt::timestamp(pr.created_at))?;
    out!();
    out!("  {}", p::dim(&format!("test it: {} send-test --dsn {}", h.command, pr.dsn)));
    Ok(())
}

fn issue_query(a: &IssueListArgs) -> IssueQuery {
    IssueQuery {
        status: Some(a.status.as_str().into()),
        query: a.query.clone(),
        since: a.since.clone(),
        sort: Some(a.sort.as_str().into()),
        environment: a.env.clone(),
        release: a.release.clone(),
        limit: a.limit,
    }
}

async fn issues(ctx: &Ctx, cmd: IssuesCmd) -> Result<()> {
    let project = ctx.resolve_project().await?;
    match cmd {
        IssuesCmd::List(a) => {
            let list = ctx.client.issues(&project, &issue_query(&a)).await.map_err(|e| ctx.err(e))?;
            if ctx.json {
                return ctx.print_json(&list);
            }
            let list = fmt::sanitize(list, true)?;
            if list.issues.is_empty() {
                out!("no issues match (window {} .. now)", fmt::timestamp(list.window_start));
                return Ok(());
            }
            let now = Utc::now();
            let mut t =
                table_with_row_lines(&["ID", "LEVEL", "ISSUE", "EVENTS", "USERS", "TREND", "LAST SEEN", "STATUS"]);
            for i in &list.issues {
                let title = match &i.culprit {
                    Some(c) if !c.is_empty() => format!("{}\n{}", i.title, c),
                    _ => i.title.clone(),
                };
                t.add_row(vec![
                    Cell::new(&i.id).fg(Color::DarkGrey),
                    level_cell(&i.level),
                    Cell::new(title),
                    right(fmt::compact(i.count)),
                    right(fmt::compact(i.user_count)),
                    Cell::new(fmt::sparkline(&fmt::resample(&i.trend, 12))).fg(Color::Magenta),
                    Cell::new(fmt::relative_time(i.last_seen, now)),
                    status_cell(i.status),
                ]);
            }
            out!("{t}");
            out!(
                "{}",
                p::dim(&format!("{} issues · window {} .. now", list.issues.len(), fmt::timestamp(list.window_start)))
            );
            Ok(())
        }
        IssuesCmd::Show { id, since } => {
            let d = ctx
                .client
                .issue(&project, &id, &WindowQuery { since, interval: None })
                .await
                .map_err(|e| ctx.err(e))?;
            if ctx.json {
                return ctx.print_json(&d);
            }
            print_issue(&fmt::sanitize(d, true)?, &fmt::clean(&project), &ctx.hints)?;
            Ok(())
        }
        IssuesCmd::Resolve { ids } => set_status(ctx, &project, &ids, IssueStatus::Resolved).await,
        IssuesCmd::Ignore { ids } => set_status(ctx, &project, &ids, IssueStatus::Ignored).await,
        IssuesCmd::Unresolve { ids } => set_status(ctx, &project, &ids, IssueStatus::Unresolved).await,
    }
}

fn print_issue(d: &IssueDetail, project: &str, h: &Hints) -> Result<()> {
    let s = &d.summary;
    let now = Utc::now();
    out!("{} {}", p::level(&format!("[{}]", s.level)), p::bold(&s.title));
    if let Some(c) = &s.culprit {
        out!("  {}", p::dim(c));
    }
    out!();
    kv("id", &s.id)?;
    kv("status", &p::status(s.status.as_str()))?;
    kv("platform", &s.platform)?;
    kv("events", &s.count.to_string())?;
    kv("users", &s.user_count.to_string())?;
    kv("first seen", &format!("{} ({})", fmt::timestamp(s.first_seen), fmt::relative_time(s.first_seen, now)))?;
    kv("last seen", &format!("{} ({})", fmt::timestamp(s.last_seen), fmt::relative_time(s.last_seen, now)))?;
    kv("first release", d.first_release.as_deref().unwrap_or("-"))?;
    kv("last release", d.last_release.as_deref().unwrap_or("-"))?;
    kv("latest event", d.latest_event_id.as_deref().unwrap_or("-"))?;
    kv("trend", &p::magenta(&fmt::sparkline(&s.trend)))?;
    if !d.tags.is_empty() {
        out!();
        out!("{}", p::bold("Tags"));
        for f in &d.tags {
            out!("  {} {}", p::bold(&f.key), p::dim(&format!("({} unique)", f.unique)));
            let total: u64 = f.top.iter().map(|v| v.count).sum::<u64>().max(1);
            for v in &f.top {
                let pct = v.count as f64 * 100.0 / total as f64;
                out!(
                    "    {:<28} {:<20} {:>5.1}%  {}",
                    fmt::truncate(&v.value, 28),
                    p::cyan(&format!("{:<20}", fmt::hbar(v.count, total, 20))),
                    pct,
                    p::dim(&v.count.to_string())
                );
            }
        }
    }
    if !d.activity.is_empty() {
        out!();
        out!("{}", p::bold("Activity"));
        for a in &d.activity {
            out!("  {}  {:<10} {}", p::dim(&fmt::timestamp(a.at)), p::status(a.status.as_str()), p::dim(&a.reason));
        }
    }
    if let Some(e) = &d.latest_event_id {
        out!();
        let cmd = h.command;
        out!("{}", p::dim(&format!("latest event: {cmd} events show {e} --issue {} -p {project}", s.id)));
    }
    Ok(())
}

async fn set_status(ctx: &Ctx, project: &str, ids: &[String], status: IssueStatus) -> Result<()> {
    let mut failed = 0;
    let mut done = Vec::new();
    for id in ids {
        match ctx.client.set_issue_status(project, id, status).await {
            Ok(d) => {
                if !ctx.json {
                    out!(
                        "{} {} {}  {}",
                        p::green("✓"),
                        status.as_str(),
                        p::bold(id),
                        p::dim(&fmt::truncate(&fmt::clean(&d.summary.title), 70))
                    );
                }
                done.push(d);
            }
            Err(e) => {
                failed += 1;
                eprintln!("{} {}: {}", p::red("✗"), fmt::clean(id), describe(&e, ctx.url(), &ctx.hints));
            }
        }
    }
    if ctx.json {
        ctx.print_json(&done)?;
    }
    if failed > 0 {
        bail!("{failed} of {} issues not updated", ids.len());
    }
    Ok(())
}

async fn events(ctx: &Ctx, cmd: EventsCmd) -> Result<()> {
    match cmd {
        EventsCmd::List { issue, trace, limit } => {
            let by_trace = trace.is_some();
            let list = if let Some(trace_id) = trace {
                let q = TraceQuery { trace_id, limit, ..Default::default() };
                ctx.client.events_by_trace_query(&q).await
            } else {
                let project = ctx.resolve_project().await?;
                ctx.client.events(&project, &EventQuery { issue, limit, ..Default::default() }).await
            }
            .map_err(|e| ctx.err(e))?;
            if ctx.json {
                return ctx.print_json(&list);
            }
            let list = fmt::sanitize(list, true)?;
            if list.events.is_empty() {
                out!("no events");
                return Ok(());
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
            out!("{t}");
            if list.next_cursor.is_some() {
                out!("{}", p::dim("more events available (use --limit)"));
            }
            Ok(())
        }
        EventsCmd::Show { event_id, issue } => {
            let project = ctx.resolve_project().await?;
            let e = ctx.client.event(&project, &event_id, issue.as_deref()).await.map_err(|e| ctx.err(e))?;
            if ctx.json {
                return ctx.print_json(&e);
            }
            print_event(&fmt::sanitize(e, true)?)?;
            Ok(())
        }
    }
}

fn section(title: &str) -> Result<()> {
    out!();
    out!("{}", p::bold(&p::cyan(&format!("── {title} "))));
    Ok(())
}

pub fn print_event(e: &EventDetail) -> Result<()> {
    let s = &e.summary;
    out!("{} {}", p::level(&format!("[{}]", s.level)), p::bold(&s.title));
    if let Some(c) = &e.culprit {
        out!("  {}", p::dim(c));
    }
    out!();
    kv("event", &s.event_id)?;
    kv("issue", &s.issue_id)?;
    kv("time", &format!("{} ({})", fmt::timestamp(s.timestamp), fmt::relative_time(s.timestamp, Utc::now())))?;
    kv("platform", &s.platform)?;
    if let Some(v) = &s.environment {
        kv("environment", v)?;
    }
    if let Some(v) = &s.release {
        kv("release", v)?;
    }
    if let Some(v) = &s.trace_id {
        kv("trace", &format!("{v}{}", s.span_id.as_ref().map(|sp| format!("  span {sp}")).unwrap_or_default()))?;
    }
    for (k, v) in ev::contexts(&e.payload) {
        kv(&k, &v)?;
    }
    if let Some(m) = ev::message(&e.payload) {
        section("Message")?;
        out!("  {m}");
    }

    let exceptions = ev::exceptions(&e.payload);
    for (i, x) in exceptions.iter().enumerate() {
        section(if i == 0 { "Exception" } else { "Caused by" })?;
        let mut head = p::bold(&p::red(&x.headline()));
        if let Some(m) = &x.mechanism {
            let handled = match x.handled {
                Some(false) => " · unhandled",
                Some(true) => " · handled",
                None => "",
            };
            head.push_str(&p::dim(&format!("  ({m}{handled})")));
        }
        out!("  {head}");
        let frames = x.frames_newest_first(false);
        if frames.is_empty() {
            out!("  {}", p::dim("(no stack trace)"));
        }
        for f in frames {
            if f.in_app {
                out!("    {} {}  {}", p::yellow("▸"), p::bold(f.function_name()), p::cyan(&f.location()));
                for (n, line, is_err) in f.context() {
                    let num = n.map(|n| format!("{n:>5}")).unwrap_or_else(|| "     ".into());
                    if is_err {
                        out!("      {} {} {}", p::highlight(&num), p::yellow("→"), p::highlight(&line));
                    } else {
                        out!("      {}   {}", p::dim(&num), p::dim(&line));
                    }
                }
            } else {
                out!("    {}", p::dim(&format!("  at {}  {}", f.function_name(), f.location())));
                if let Some(c) = &f.context_line {
                    out!("      {}", p::dim(&format!("      {}", c.trim())));
                }
            }
        }
    }

    let crumbs = ev::breadcrumbs(&e.payload);
    if !crumbs.is_empty() {
        section("Breadcrumbs (newest first)")?;
        for b in &crumbs {
            let time =
                b.timestamp.map(|t| t.format("%H:%M:%S%.3f").to_string()).unwrap_or_else(|| "            ".into());
            let cat = b.category.clone().or_else(|| b.ty.clone()).unwrap_or_default();
            out!(
                "  {}  {:<8} {:<16} {}",
                p::dim(&time),
                p::level(&b.level),
                p::cyan(&fmt::truncate(&cat, 16)),
                b.text()
            );
        }
    }

    if !e.tags.is_empty() {
        section("Tags")?;
        let w = e.tags.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0).min(24);
        for (k, v) in &e.tags {
            out!("  {}  {v}", p::dim(&format!("{k:<w$}")));
        }
    }

    let user = ev::user(&e.payload);
    if !user.is_empty() {
        section("User")?;
        for (k, v) in &user {
            kv(k, v)?;
        }
    }

    if let Some(r) = ev::request(&e.payload) {
        section("Request")?;
        out!("  {} {}", p::bold(r.method.as_deref().unwrap_or("")), r.url.as_deref().unwrap_or(""));
        if let Some(q) = &r.query {
            kv("query", q)?;
        }
        for (k, v) in &r.headers {
            kv(k, v)?;
        }
    }
    Ok(())
}

async fn stats(ctx: &Ctx, since: String, interval: Option<String>) -> Result<()> {
    let project = ctx.resolve_project().await?;
    let s = ctx
        .client
        .stats(&project, &WindowQuery { since: Some(since.clone()), interval })
        .await
        .map_err(|e| ctx.err(e))?;
    if ctx.json {
        return ctx.print_json(&s);
    }
    let s = fmt::sanitize(s, true)?;
    out!(
        "{} {}",
        p::bold("Project stats"),
        p::dim(&format!("last {since} · buckets of {}", fmt::duration(s.interval_seconds)))
    );
    out!();
    out!(
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
    out!();
    out!("  {}", p::magenta(&fmt::sparkline(&events)));
    out!();
    let max = events.iter().copied().max().unwrap_or(0);
    for b in &s.buckets {
        out!(
            "  {}  {:>7}  {}{}",
            p::dim(&b.start.format("%m-%d %H:%M").to_string()),
            b.events,
            p::red(&fmt::hbar(b.errors, max, 40)),
            p::yellow(&fmt::hbar(b.warnings + b.other, max, 40))
        );
    }
    out!();
    out!("  {} errors  {} warnings/other", p::red("█"), p::yellow("█"));
    Ok(())
}

async fn sourcemaps(ctx: &Ctx) -> Result<()> {
    let project = ctx.resolve_project().await?;
    let list = ctx.client.sourcemaps(&project).await.map_err(|e| ctx.err(e))?;
    if ctx.json {
        return ctx.print_json(&list);
    }
    let list = fmt::sanitize(list, true)?;
    if list.is_empty() {
        out!("no sourcemaps uploaded (upload with sentry-cli sourcemaps upload)");
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
    out!("{t}");
    Ok(())
}

async fn send_test(dsn: &str, message: &str, ctx: &Ctx) -> Result<()> {
    let (d, id) = crate::send_test::send(dsn, message).await?;
    if ctx.json {
        out!("{}", serde_json::json!({"id": id, "project_id": d.project_id, "url": d.envelope_url}));
    } else {
        out!("{} sent test event {} to project {}", p::green("✓"), p::bold(&id), d.project_id);
        out!("  {}", p::dim(&format!("POST {}", d.envelope_url)));
        out!("  {}", p::dim(&format!("check it: {} events show {id} -p {}", ctx.hints.command, d.project_id)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MALLARD: Hints = Hints {
        command: "mallard errors",
        url_env: "MALLARD_URL",
        token_env: "MALLARD_TOKEN",
        project_env: "MALLARD_PROJECT",
    };

    #[test]
    fn hints_name_the_host() {
        let e = sdl_client::Error::Api { status: 401, message: String::new() };
        assert_eq!(describe(&e, "http://x", &Hints::SDL), "invalid token (set SDL_TOKEN or --token)");
        assert_eq!(describe(&e, "http://x", &MALLARD), "invalid token (set MALLARD_TOKEN or --token)");
        let e = sdl_client::Client::new("nope", "").unwrap_err();
        assert!(describe(&e, "nope", &MALLARD).contains("set MALLARD_URL or --url"));
    }

    #[test]
    fn describe_maps_every_kind_of_error() {
        let api = |status: u16, m: &str| sdl_client::Error::Api { status, message: m.into() };
        let d = |e: sdl_client::Error| describe(&e, "http://x", &Hints::SDL);
        assert_eq!(d(api(403, "")), "token not allowed to do this (check SDL_TOKEN)");
        assert_eq!(d(api(404, "")), "not found");
        assert_eq!(d(api(404, "no issue")), "not found: no issue");
        assert_eq!(d(api(400, "bad since")), "request rejected (400): bad since");
        assert_eq!(d(api(503, "down\x1b[2J")), "server error (503): down\u{FFFD}[2J");
        let e = block_on(sdl_client::Client::new("http://x", "").unwrap().project("..")).unwrap_err();
        assert_eq!(d(e), "invalid path segment \"..\": ids and slugs can't be empty, '.' or '..'");
    }

    #[test]
    fn err_keeps_the_typed_source_and_cleans_it() {
        let ctx = Ctx::new(sdl_client::Client::new("http://x", "").unwrap()).with_hints(MALLARD);
        let e = ctx.err(sdl_client::Error::Api { status: 404, message: "gone\x1b]0;x\x07".into() });
        assert_eq!(e.to_string(), "not found: gone\u{FFFD}]0;x\u{FFFD}");
        let src = e.downcast_ref::<sdl_client::Error>().expect("typed source");
        assert!(src.is_not_found());
        assert!(!format!("{e:#}").chars().any(char::is_control), "{e:#}");
    }

    /// Minimal executor for futures that resolve without I/O.
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let mut f = std::pin::pin!(f);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    #[test]
    fn json_output_escapes_c1_and_bidi_but_stays_equal() {
        let v = serde_json::json!({"t": "a\u{9b}31m\u{202e}b\u{1b}c\nd"});
        let out = json_safe(&serde_json::to_string_pretty(&v).unwrap());
        assert!(!out.chars().any(|c| c.is_control() && c != '\n'), "{out:?}");
        assert!(out.contains("\\u009b") && out.contains("\\u202e"));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&out).unwrap(), v);
    }
}
