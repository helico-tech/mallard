//! Read queries for the `/v1` API. Every query filters on `project_id` and,
//! where possible, on a `received_at` window (partition pruning) and
//! `group_hash` (row-group pruning thanks to the sort order).

use super::{Ns, us_to_dt};
use anyhow::Result;
use mallard_duck::duckdb::types::ToSql;
use mallard_duck::duckdb::{Connection, OptionalExt, params, params_from_iter};
use sdl_types::*;
use std::collections::HashMap;

pub fn group_id(hash: u64) -> String {
    format!("{hash:016x}")
}

/// 16 hex digits (no sign, which `from_str_radix` would accept).
pub fn parse_group_id(s: &str) -> Option<u64> {
    (s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| u64::from_str_radix(s, 16).ok()).flatten()
}

pub const TREND_BUCKETS: i64 = 24;

pub use mallard_common::window::Window;

fn start_us(w: &Window) -> i64 {
    w.from.timestamp_micros()
}

fn end_us(w: &Window) -> i64 {
    w.to.timestamp_micros()
}

fn span_us(w: &Window) -> i64 {
    (w.to - w.from).num_microseconds().unwrap_or(i64::MAX).max(1)
}

/// Bucket a set of issues' events over the window into `TREND_BUCKETS`.
fn trends(conn: &Connection, ns: &Ns, project_id: u32, w: &Window, groups: &[u64]) -> Result<HashMap<u64, Vec<u64>>> {
    let mut out: HashMap<u64, Vec<u64>> = groups.iter().map(|g| (*g, vec![0; TREND_BUCKETS as usize])).collect();
    if groups.is_empty() {
        return Ok(out);
    }
    let bucket = (span_us(w) / TREND_BUCKETS).max(1);
    // Numbers only, so building the IN list is injection-safe.
    let list = groups.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let sql = ns.sql(&format!(
        "SELECT group_hash, CAST(floor((epoch_us(received_at) - ?) / ?) AS BIGINT) AS b, count(*)
         FROM $S.events
         WHERE project_id = ? AND received_at >= make_timestamp(?) AND received_at < make_timestamp(?) AND group_hash IN ({list})
         GROUP BY ALL"
    ));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![start_us(w), bucket, project_id as i32, start_us(w), end_us(w)], |r| {
        Ok((r.get::<_, u64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    })?;
    for row in rows {
        let (g, b, c) = row?;
        if let Some(v) = out.get_mut(&g) {
            let idx = b.clamp(0, TREND_BUCKETS - 1) as usize;
            v[idx] += c as u64;
        }
    }
    Ok(out)
}

/// `status` filter of the issue list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    All,
    Only(IssueStatus),
}

impl StatusFilter {
    /// `None` (the default) means unresolved issues.
    pub fn parse(s: Option<&str>) -> Option<Self> {
        match s {
            None => Some(Self::Only(IssueStatus::Unresolved)),
            Some("all") => Some(Self::All),
            Some(s) => s.parse().ok().map(Self::Only),
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Only(s) => s.as_str(),
        }
    }
}

/// `sort` of the issue list (always descending).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueSort {
    LastSeen,
    FirstSeen,
    Count,
    Users,
}

impl IssueSort {
    /// `None` (the default) means `last_seen`.
    pub fn parse(s: Option<&str>) -> Option<Self> {
        match s {
            None | Some("last_seen") => Some(Self::LastSeen),
            Some("first_seen") => Some(Self::FirstSeen),
            Some("count") => Some(Self::Count),
            Some("users") => Some(Self::Users),
            Some(_) => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LastSeen => "last_seen",
            Self::FirstSeen => "first_seen",
            Self::Count => "count",
            Self::Users => "users",
        }
    }
    /// The sorted value (a BIGINT) in the issue list query.
    fn expr(self) -> &'static str {
        match self {
            Self::LastSeen => "agg.last_us",
            Self::FirstSeen => "epoch_us(i.first_seen)",
            Self::Count => "agg.cnt",
            Self::Users => "agg.users",
        }
    }
}

/// Where the next page of an issue list starts: after the row with this
/// sort value and group (`ORDER BY value DESC, group_hash`). It carries the
/// sort and the window, so every page ranks the same events. Text form
/// `<sort>:<value>:<group hex>:<from µs>:<to µs>`; clients treat it as
/// opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssueCursor {
    pub sort: IssueSort,
    value: i64,
    group: u64,
    pub from_us: i64,
    pub to_us: i64,
}

impl IssueCursor {
    pub fn parse(c: &str) -> Option<IssueCursor> {
        let mut p = c.split(':');
        let sort = IssueSort::parse(Some(p.next()?))?;
        let value = p.next()?.parse().ok()?;
        let group = parse_group_id(p.next()?)?;
        let from_us = p.next()?.parse().ok()?;
        let to_us = p.next()?.parse().ok()?;
        p.next().is_none().then_some(IssueCursor { sort, value, group, from_us, to_us })
    }
}

impl std::fmt::Display for IssueCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let IssueCursor { sort, value, group, from_us, to_us } = self;
        write!(f, "{}:{value}:{}:{from_us}:{to_us}", sort.as_str(), group_id(*group))
    }
}

/// Filters on event columns, shared by the issue and event lists:
/// `(sql, values)` with ` AND …` clauses and their bound values. `tags`
/// are `(key, value)` pairs; all must match.
fn event_filters(environment: Option<&str>, release: Option<&str>, tags: &[(String, String)]) -> (String, Vec<String>) {
    let mut sql = String::new();
    let mut values = Vec::new();
    if let Some(env) = environment {
        sql.push_str(" AND environment = ?");
        values.push(env.to_string());
    }
    if let Some(rel) = release {
        sql.push_str(" AND release = ?");
        values.push(rel.to_string());
    }
    for (k, v) in tags {
        sql.push_str(" AND tags[?] = ?");
        values.push(k.clone());
        values.push(v.clone());
    }
    (sql, values)
}

/// `%text%` for `ILIKE … ESCAPE '\'`, with `%`, `_` and `\` taken literally.
/// `None` for no or blank text.
fn like_pattern(text: Option<&str>) -> Option<String> {
    let text = text.map(str::trim).filter(|t| !t.is_empty())?;
    Some(format!("%{}%", text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")))
}

/// The issue list. `tags` filter the events counted (like `environment`
/// and `release`); `after` continues a previous page (its window is `w`).
#[allow(clippy::too_many_arguments)]
pub fn list_issues(
    conn: &Connection,
    ns: &Ns,
    project_id: u32,
    q: &IssueQuery,
    tags: &[(String, String)],
    w: &Window,
    status: StatusFilter,
    sort: IssueSort,
    after: Option<IssueCursor>,
) -> Result<IssueList> {
    let status = status.as_str();
    let limit = q.limit.unwrap_or(50).clamp(1, 500) as i64;

    let (filters, filter_values) = event_filters(q.environment.as_deref(), q.release.as_deref(), tags);
    let (pid, start_us, end_us) = (project_id as i32, start_us(w), end_us(w));

    // Phase 1: rank issues with cheap aggregates only (count, max). Distinct
    // users are only computed for all groups when sorting by them, and then
    // approximately.
    let users_expr = if sort == IssueSort::Users { "approx_count_distinct(user_key)" } else { "0" };
    let sort_kind = sort;
    let sort = sort.expr();
    let mut p: Vec<&dyn ToSql> = vec![&pid, &start_us, &end_us];
    p.extend(filter_values.iter().map(|v| v as &dyn ToSql));
    p.push(&pid); // st
    p.push(&pid); // issues join
    p.push(&status);
    p.push(&status);
    // Text search matches the issue's first title and culprit.
    let like = like_pattern(q.query.as_deref());
    let mut search = match &like {
        Some(like) => {
            p.push(like);
            p.push(like);
            r" AND (i.title ILIKE ? ESCAPE '\' OR i.culprit ILIKE ? ESCAPE '\')".to_string()
        }
        None => String::new(),
    };
    // New issues: first seen inside the window.
    if q.new == Some(true) {
        search.push_str(" AND i.first_seen >= make_timestamp(?) AND i.first_seen < make_timestamp(?)");
        p.push(&start_us);
        p.push(&end_us);
    }
    if let Some(rel) = &q.first_release {
        search.push_str(" AND i.first_release = ?");
        p.push(rel);
    }
    let after_values = after.map(|c| (c.value, c.group));
    if let Some((value, group)) = &after_values {
        search.push_str(&format!(" AND ({sort} < ? OR ({sort} = ? AND agg.group_hash > ?))"));
        p.push(value);
        p.push(value);
        p.push(group);
    }
    p.push(&limit);
    let sql = ns.sql(&format!(
        "WITH agg AS (
            SELECT group_hash, count(*) AS cnt, epoch_us(max(received_at)) AS last_us, CAST({users_expr} AS BIGINT) AS users
            FROM $S.events
            WHERE project_id = ? AND received_at >= make_timestamp(?) AND received_at < make_timestamp(?){filters}
            GROUP BY group_hash
         ), st AS (
            SELECT group_hash, arg_max(status, changed_at) AS status
            FROM $S.issue_activity WHERE project_id = ? GROUP BY group_hash
         )
         SELECT agg.group_hash, i.title, i.culprit, i.level, i.platform, i.exception_type,
                coalesce(st.status, 'unresolved') AS status, epoch_us(i.first_seen) AS first_us,
                agg.cnt, agg.last_us, {sort} AS sort_value
         FROM agg
         JOIN $S.issues i ON i.project_id = ? AND i.group_hash = agg.group_hash
         LEFT JOIN st ON st.group_hash = agg.group_hash
         WHERE (? = 'all' OR coalesce(st.status, 'unresolved') = ?){search}
         ORDER BY {sort} DESC, agg.group_hash
         LIMIT ?"
    ));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(p), |r| {
        let issue = IssueSummary {
            id: group_id(r.get(0)?),
            project_id,
            title: r.get(1)?,
            culprit: r.get(2)?,
            level: r.get(3)?,
            platform: r.get(4)?,
            exception_type: r.get(5)?,
            status: r.get::<_, String>(6)?.parse().unwrap_or_default(),
            first_seen: us_to_dt(r.get(7)?),
            count: r.get::<_, i64>(8)? as u64,
            last_seen: us_to_dt(r.get(9)?),
            user_count: 0,
            trend: vec![],
        };
        Ok((issue, r.get::<_, i64>(10)?))
    })?;
    let ranked: Vec<(IssueSummary, i64)> = rows.collect::<Result<_, _>>()?;
    let next_cursor = (ranked.len() as i64 == limit)
        .then(|| ranked.last())
        .flatten()
        .and_then(|(i, value)| Some((parse_group_id(&i.id)?, *value)))
        .map(|(group, value)| {
            IssueCursor { sort: sort_kind, value, group, from_us: start_us, to_us: end_us }.to_string()
        });
    let mut issues: Vec<IssueSummary> = ranked.into_iter().map(|(i, _)| i).collect();
    let groups: Vec<u64> = issues.iter().filter_map(|i| parse_group_id(&i.id)).collect();
    if groups.is_empty() {
        return Ok(IssueList { issues, window_start: w.from, window_end: w.to, next_cursor });
    }

    // Phase 2: exact users and the latest title/culprit/level, only for the
    // issues on this page.
    let list = groups.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let mut p: Vec<&dyn ToSql> = vec![&pid, &start_us, &end_us];
    p.extend(filter_values.iter().map(|v| v as &dyn ToSql));
    let sql = ns.sql(&format!(
        "SELECT group_hash, count(DISTINCT user_key), arg_max(title, received_at), arg_max(culprit, received_at),
                arg_max(level, received_at)
         FROM $S.events
         WHERE project_id = ? AND received_at >= make_timestamp(?) AND received_at < make_timestamp(?){filters} AND group_hash IN ({list})
         GROUP BY group_hash"
    ));
    let mut stmt = conn.prepare(&sql)?;
    let mut latest: HashMap<u64, (u64, String, Option<String>, String)> = HashMap::new();
    for row in stmt.query_map(params_from_iter(p), |r| {
        Ok((r.get::<_, u64>(0)?, r.get::<_, i64>(1)? as u64, r.get(2)?, r.get(3)?, r.get(4)?))
    })? {
        let (g, users, title, culprit, level) = row?;
        latest.insert(g, (users, title, culprit, level));
    }
    let mut t = trends(conn, ns, project_id, w, &groups)?;
    for i in &mut issues {
        let Some(g) = parse_group_id(&i.id) else { continue };
        if let Some((users, title, culprit, level)) = latest.remove(&g) {
            i.user_count = users;
            i.title = title;
            i.culprit = culprit.or(i.culprit.take());
            i.level = level;
        }
        if let Some(v) = t.remove(&g) {
            i.trend = v;
        }
    }
    Ok(IssueList { issues, window_start: w.from, window_end: w.to, next_cursor })
}

pub fn issue_detail(
    conn: &Connection,
    ns: &Ns,
    project_id: u32,
    group: u64,
    w: &Window,
) -> Result<Option<IssueDetail>> {
    let base = conn
        .query_row(
            &ns.sql(
                "SELECT title, culprit, level, platform, exception_type, epoch_us(first_seen), first_release
                 FROM $S.issues WHERE project_id = ? AND group_hash = ? ORDER BY first_seen LIMIT 1",
            ),
            params![project_id as i32, group],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((title, culprit, level, platform, exception_type, first_us, first_release)) = base else {
        return Ok(None);
    };
    let agg = conn.query_row(
        &ns.sql(
            "SELECT count(*), count(DISTINCT user_key), epoch_us(max(received_at)),
                    arg_max(title, received_at), arg_max(culprit, received_at), arg_max(level, received_at),
                    arg_max(release, received_at), arg_max(event_id::VARCHAR, received_at)
             FROM $S.events WHERE project_id = ? AND group_hash = ? AND received_at >= make_timestamp(?) AND received_at < make_timestamp(?)",
        ),
        params![project_id as i32, group, start_us(w), end_us(w)],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        },
    )?;
    let (cnt, users, last_us, l_title, l_culprit, l_level, last_release, latest_event) = agg;
    let latest_event_id = match latest_event {
        Some(e) => Some(e.replace('-', "")),
        // No events in the window: still point at the newest event.
        None => conn
            .query_row(
                &ns.sql(
                    "SELECT event_id::VARCHAR FROM $S.events WHERE project_id = ? AND group_hash = ?
                     ORDER BY received_at DESC LIMIT 1",
                ),
                params![project_id as i32, group],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|e| e.replace('-', "")),
    };

    let mut activity = Vec::new();
    let mut stmt = conn.prepare(&ns.sql(
        "SELECT epoch_us(changed_at), status, reason FROM $S.issue_activity
         WHERE project_id = ? AND group_hash = ? ORDER BY changed_at DESC LIMIT 50",
    ))?;
    for row in stmt.query_map(params![project_id as i32, group], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })? {
        let (at, s, reason) = row?;
        activity.push(IssueActivity { at: us_to_dt(at), status: s.parse().unwrap_or_default(), reason });
    }
    let status = activity.first().map(|a| a.status).unwrap_or_default();

    let tags = tag_facets(conn, ns, project_id, group, w)?;
    let trend = trends(conn, ns, project_id, w, &[group])?.remove(&group).unwrap_or_default();
    let first_seen = us_to_dt(first_us);
    Ok(Some(IssueDetail {
        summary: IssueSummary {
            id: group_id(group),
            project_id,
            title: l_title.unwrap_or(title),
            culprit: l_culprit.or(culprit),
            level: l_level.unwrap_or(level),
            platform,
            exception_type,
            status,
            first_seen,
            last_seen: last_us.map(us_to_dt).unwrap_or(first_seen),
            count: cnt as u64,
            user_count: users as u64,
            trend,
        },
        first_release,
        last_release,
        latest_event_id,
        tags,
        activity,
        changed: None,
    }))
}

fn tag_facets(conn: &Connection, ns: &Ns, project_id: u32, group: u64, w: &Window) -> Result<Vec<TagFacet>> {
    // Top 10 values per key and the distinct count are computed in SQL, so
    // high-cardinality tags (urls, ids) don't stream every value to us.
    let mut stmt = conn.prepare(&ns.sql(
        "WITH kv AS (
            SELECT k, v, count(*) AS c FROM (
                SELECT unnest(map_keys(tags)) AS k, unnest(map_values(tags)) AS v
                FROM $S.events WHERE project_id = ? AND group_hash = ? AND received_at >= make_timestamp(?) AND received_at < make_timestamp(?)
            ) GROUP BY k, v
         )
         SELECT k, v, c, count(*) OVER (PARTITION BY k) AS uniq
         FROM kv
         QUALIFY row_number() OVER (PARTITION BY k ORDER BY c DESC, v) <= 10
         ORDER BY k, c DESC, v",
    ))?;
    let mut facets: Vec<TagFacet> = Vec::new();
    for row in stmt.query_map(params![project_id as i32, group, start_us(w), end_us(w)], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))
    })? {
        let (k, v, c, uniq) = row?;
        let value = TagValueCount { value: v, count: c as u64 };
        match facets.last_mut() {
            Some(f) if f.key == k => f.top.push(value),
            _ => facets.push(TagFacet { key: k, unique: uniq as u64, top: vec![value] }),
        }
    }
    Ok(facets)
}

const EVENT_COLUMNS: &str = "event_id::VARCHAR, group_hash, epoch_us(timestamp), epoch_us(received_at), title, level,
    platform, environment, release, user_key, message, trace_id::VARCHAR, span_id, project_id";

fn event_summary(r: &mallard_duck::duckdb::Row<'_>) -> mallard_duck::duckdb::Result<EventSummary> {
    Ok(EventSummary {
        event_id: r.get::<_, String>(0)?.replace('-', ""),
        project_id: r.get::<_, i32>(13)? as u32,
        issue_id: group_id(r.get(1)?),
        timestamp: us_to_dt(r.get(2)?),
        received_at: us_to_dt(r.get(3)?),
        title: r.get(4)?,
        level: r.get(5)?,
        platform: r.get(6)?,
        environment: r.get(7)?,
        release: r.get(8)?,
        user: r.get(9)?,
        message: r.get(10)?,
        trace_id: r.get::<_, Option<String>>(11)?.map(|t| t.replace('-', "")),
        span_id: r.get::<_, Option<u64>>(12)?.map(|s| format!("{s:016x}")),
    })
}

/// Events of a project, newest first. `tags` are `(key, value)` filters
/// (all must match).
#[allow(clippy::too_many_arguments)]
pub fn list_events(
    conn: &Connection,
    ns: &Ns,
    project_id: u32,
    group: Option<u64>,
    q: &EventQuery,
    tags: &[(String, String)],
    w: Option<&Window>,
    before: Option<Cursor>,
) -> Result<EventList> {
    let limit = q.limit.unwrap_or(50).clamp(1, 500) as i64;
    let mut sql = format!("SELECT {EVENT_COLUMNS} FROM $S.events WHERE project_id = ?");
    let mut p: Vec<Box<dyn ToSql>> = vec![Box::new(project_id as i32)];
    if let Some(w) = w {
        sql.push_str(" AND received_at >= make_timestamp(?) AND received_at < make_timestamp(?)");
        p.push(Box::new(start_us(w)));
        p.push(Box::new(end_us(w)));
    }
    let (filters, values) = event_filters(q.environment.as_deref(), q.release.as_deref(), tags);
    sql.push_str(&filters);
    p.extend(values.into_iter().map(|v| Box::new(v) as Box<dyn ToSql>));
    if let Some(like) = like_pattern(q.query.as_deref()) {
        sql.push_str(r" AND (title ILIKE ? ESCAPE '\' OR message ILIKE ? ESCAPE '\')");
        p.push(Box::new(like.clone()));
        p.push(Box::new(like));
    }
    if let Some(g) = group {
        sql.push_str(" AND group_hash = ?");
        p.push(Box::new(g));
    }
    if let Some(t) = q.trace_id.as_deref() {
        let t = sdl_protocol::event::parse_trace_id(t)
            .ok_or_else(|| anyhow::anyhow!("trace_id must be 32 hex characters"))?;
        // Trace ids are random: bound the scan to recent days.
        let since = (chrono::Utc::now() - chrono::Duration::days(TRACE_WINDOW_DAYS)).timestamp_micros();
        sql.push_str(" AND trace_id = ?::UUID AND received_at >= make_timestamp(?)");
        p.push(Box::new(t.to_string()));
        p.push(Box::new(since));
    }
    if let Some(Cursor { us, id }) = before {
        match id {
            Some(id) => {
                sql.push_str(" AND (received_at < make_timestamp(?) OR (received_at = make_timestamp(?) AND event_id < ?::UUID))");
                p.push(Box::new(us));
                p.push(Box::new(us));
                p.push(Box::new(id.to_string()));
            }
            None => {
                sql.push_str(" AND received_at < make_timestamp(?)");
                p.push(Box::new(us));
            }
        }
    }
    sql.push_str(" ORDER BY received_at DESC, event_id DESC LIMIT ?");
    p.push(Box::new(limit));
    let mut stmt = conn.prepare(&ns.sql(&sql))?;
    let events: Vec<EventSummary> =
        stmt.query_map(params_from_iter(p.iter().map(|b| b.as_ref())), event_summary)?.collect::<Result<_, _>>()?;
    let next_cursor = (events.len() as i64 == limit)
        .then(|| events.last().map(|e| format!("{}:{}", e.received_at.timestamp_micros(), e.event_id)))
        .flatten();
    Ok(EventList { events, next_cursor })
}

/// Events of one trace across all projects, newest first. The window keeps
/// the scan to recent daily partitions.
pub fn events_by_trace(
    conn: &Connection,
    ns: &Ns,
    trace_id: uuid::Uuid,
    span_id: Option<u64>,
    w: &Window,
    limit: u32,
) -> Result<EventList> {
    let mut sql = format!(
        "SELECT {EVENT_COLUMNS} FROM $S.events WHERE received_at >= make_timestamp(?) AND received_at < make_timestamp(?) AND trace_id = ?::UUID"
    );
    let mut p: Vec<Box<dyn ToSql>> = vec![Box::new(start_us(w)), Box::new(end_us(w)), Box::new(trace_id.to_string())];
    if let Some(s) = span_id {
        sql.push_str(" AND span_id = ?");
        p.push(Box::new(s));
    }
    sql.push_str(" ORDER BY received_at DESC LIMIT ?");
    p.push(Box::new(limit.clamp(1, 500) as i64));
    let mut stmt = conn.prepare(&ns.sql(&sql))?;
    let events =
        stmt.query_map(params_from_iter(p.iter().map(|b| b.as_ref())), event_summary)?.collect::<Result<_, _>>()?;
    Ok(EventList { events, next_cursor: None })
}

/// Error events of many traces at once (for trace lists): per trace the
/// count and the newest [`MAX_EVENTS_PER_TRACE`] events. One query on the
/// UUID/UBIGINT columns, never the payload.
pub fn events_by_traces(conn: &Connection, ns: &Ns, trace_ids: &[uuid::Uuid], w: &Window) -> Result<EventsByTrace> {
    let mut out = EventsByTrace::default();
    if trace_ids.is_empty() {
        return Ok(out);
    }
    let ids = vec!["?::UUID"; trace_ids.len()].join(", ");
    let sql = format!(
        "SELECT trace_id::VARCHAR, event_id::VARCHAR, group_hash, project_id, span_id, title,
                count(*) OVER (PARTITION BY trace_id)
         FROM $S.events
         WHERE received_at >= make_timestamp(?) AND received_at < make_timestamp(?) AND trace_id IN ({ids})
         QUALIFY row_number() OVER (PARTITION BY trace_id ORDER BY received_at DESC, event_id) <= ?
         ORDER BY trace_id, received_at DESC, event_id"
    );
    let mut p: Vec<Box<dyn ToSql>> = vec![Box::new(start_us(w)), Box::new(end_us(w))];
    p.extend(trace_ids.iter().map(|t| Box::new(t.to_string()) as Box<dyn ToSql>));
    p.push(Box::new(MAX_EVENTS_PER_TRACE as i64));
    let mut stmt = conn.prepare(&ns.sql(&sql))?;
    let mut rows = stmt.query(params_from_iter(p.iter().map(|b| b.as_ref())))?;
    while let Some(r) = rows.next()? {
        let trace = out.traces.entry(r.get::<_, String>(0)?.replace('-', "")).or_default();
        trace.count = r.get::<_, i64>(6)? as u64;
        trace.events.push(TraceEvent {
            event_id: r.get::<_, String>(1)?.replace('-', ""),
            issue_id: group_id(r.get(2)?),
            project_id: r.get::<_, i32>(3)? as u32,
            span_id: r.get::<_, Option<u64>>(4)?.map(|s| format!("{s:016x}")),
            title: r.get(5)?,
        });
    }
    Ok(out)
}

/// Days searched by trace id lookups.
pub const TRACE_WINDOW_DAYS: i64 = 30;

/// Paging cursor `"<received_at micros>:<event_id>"` (ties broken by event
/// id); a bare `"<micros>"` is accepted too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    us: i64,
    id: Option<uuid::Uuid>,
}

impl Cursor {
    pub fn parse(c: &str) -> Option<Cursor> {
        match c.split_once(':') {
            Some((us, id)) => Some(Cursor { us: us.parse().ok()?, id: Some(uuid::Uuid::try_parse(id).ok()?) }),
            None => Some(Cursor { us: c.parse().ok()?, id: None }),
        }
    }
}

/// Look up one event, in one project or (`None`) in any. Event ids are
/// random, so without the issue (which prunes by sort order) we search
/// growing windows: most lookups are for recent events and only read the
/// last day's partitions.
pub fn event_detail(
    conn: &Connection,
    ns: &Ns,
    project_id: Option<u32>,
    event_id: uuid::Uuid,
    group: Option<u64>,
) -> Result<Option<EventDetail>> {
    if group.is_some() {
        return event_detail_in(conn, ns, project_id, event_id, group, None);
    }
    let now = chrono::Utc::now();
    let mut lower: Option<i64> = None;
    for days in [1, 7, 31] {
        let from = (now - chrono::Duration::days(days)).timestamp_micros();
        if let Some(e) = event_detail_in(conn, ns, project_id, event_id, None, Some((from, lower)))? {
            return Ok(Some(e));
        }
        lower = Some(from);
    }
    // Everything older than the last window.
    event_detail_in(conn, ns, project_id, event_id, None, Some((i64::MIN, lower)))
}

/// `range`: (from, to) in micros; `to = None` means up to now.
fn event_detail_in(
    conn: &Connection,
    ns: &Ns,
    project_id: Option<u32>,
    event_id: uuid::Uuid,
    group: Option<u64>,
    range: Option<(i64, Option<i64>)>,
) -> Result<Option<EventDetail>> {
    let mut sql = format!(
        "SELECT {EVENT_COLUMNS}, culprit, CAST(tags AS JSON)::VARCHAR, payload FROM $S.events
         WHERE event_id = ?::UUID"
    );
    let mut p: Vec<Box<dyn ToSql>> = vec![Box::new(event_id.to_string())];
    if let Some(pid) = project_id {
        sql.push_str(" AND project_id = ?");
        p.push(Box::new(pid as i32));
    }
    if let Some(g) = group {
        sql.push_str(" AND group_hash = ?");
        p.push(Box::new(g));
    }
    if let Some((from, to)) = range {
        if from > i64::MIN {
            sql.push_str(" AND received_at >= make_timestamp(?)");
            p.push(Box::new(from));
        }
        if let Some(to) = to {
            sql.push_str(" AND received_at < make_timestamp(?)");
            p.push(Box::new(to));
        }
    }
    sql.push_str(" LIMIT 1");
    let row = conn
        .query_row(&ns.sql(&sql), params_from_iter(p.iter().map(|b| b.as_ref())), |r| {
            Ok((
                event_summary(r)?,
                r.get::<_, Option<String>>(14)?,
                r.get::<_, Option<String>>(15)?,
                r.get::<_, String>(16)?,
            ))
        })
        .optional()?;
    Ok(row.map(|(summary, culprit, tags, payload)| {
        let tags: Vec<(String, String)> = tags
            .and_then(|t| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&t).ok())
            .map(|m| m.into_iter().map(|(k, v)| (k, v.as_str().unwrap_or_default().to_string())).collect())
            .unwrap_or_default();
        EventDetail {
            summary,
            culprit,
            tags,
            payload: serde_json::from_str(&payload).unwrap_or_else(|e| {
                tracing::warn!(%event_id, error = %e, "stored event payload is not valid JSON");
                serde_json::Value::Null
            }),
        }
    }))
}

pub fn project_stats(
    conn: &Connection,
    ns: &Ns,
    project_id: u32,
    w: &Window,
    interval_us: Option<i64>,
) -> Result<ProjectStats> {
    let (interval_us, n) = stats_buckets(span_us(w), interval_us);
    let mut buckets: Vec<StatsBucket> = (0..n)
        .map(|i| StatsBucket {
            start: us_to_dt(start_us(w) + i * interval_us),
            events: 0,
            errors: 0,
            warnings: 0,
            other: 0,
        })
        .collect();
    let mut stmt = conn.prepare(&ns.sql(
        "SELECT CAST(floor((epoch_us(received_at) - ?) / ?) AS BIGINT) AS b, count(*),
                count(*) FILTER (WHERE level IN ('error', 'fatal')),
                count(*) FILTER (WHERE level = 'warning')
         FROM $S.events WHERE project_id = ? AND received_at >= make_timestamp(?) AND received_at < make_timestamp(?)
         GROUP BY b",
    ))?;
    for row in stmt.query_map(params![start_us(w), interval_us, project_id as i32, start_us(w), end_us(w)], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))
    })? {
        let (b, total, errors, warnings) = row?;
        if let Some(bk) = buckets.get_mut(b.clamp(0, n - 1) as usize) {
            bk.events += total as u64;
            bk.errors += errors as u64;
            bk.warnings += warnings as u64;
            bk.other += (total - errors - warnings) as u64;
        }
    }
    let (total, issues, users) = conn.query_row(
        &ns.sql(
            "SELECT count(*), count(DISTINCT group_hash), count(DISTINCT user_key)
             FROM $S.events WHERE project_id = ? AND received_at >= make_timestamp(?) AND received_at < make_timestamp(?)",
        ),
        params![project_id as i32, start_us(w), end_us(w)],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
    )?;
    let new_issues: i64 = conn.query_row(
        &ns.sql("SELECT count(*) FROM $S.issues WHERE project_id = ? AND first_seen >= make_timestamp(?) AND first_seen < make_timestamp(?)"),
        params![project_id as i32, start_us(w), end_us(w)],
        |r| r.get(0),
    )?;
    Ok(ProjectStats {
        project_id,
        window_start: w.from,
        window_end: w.to,
        interval_seconds: (interval_us / 1_000_000) as u64,
        total_events: total as u64,
        unique_issues: issues as u64,
        new_issues: new_issues as u64,
        affected_users: users as u64,
        buckets,
    })
}

/// Releases named by the project's events in the window, newest
/// `last_seen` first. `new_issues` counts issues whose `first_release` is
/// the version (not limited to the window: the issues table is small).
pub fn list_releases(conn: &Connection, ns: &Ns, project_id: u32, w: &Window, limit: u32) -> Result<ReleaseList> {
    let mut stmt = conn.prepare(&ns.sql(
        "WITH r AS (
            SELECT release, epoch_us(min(received_at)) AS first_us, epoch_us(max(received_at)) AS last_us,
                   count(*) AS events, count(DISTINCT group_hash) AS issues
            FROM $S.events
            WHERE project_id = ? AND received_at >= make_timestamp(?) AND received_at < make_timestamp(?)
              AND release IS NOT NULL
            GROUP BY release
         ), n AS (
            SELECT first_release, count(*) AS new_issues FROM $S.issues
            WHERE project_id = ? AND first_release IS NOT NULL GROUP BY first_release
         )
         SELECT r.release, r.first_us, r.last_us, r.events, r.issues, coalesce(n.new_issues, 0)
         FROM r LEFT JOIN n ON n.first_release = r.release
         ORDER BY r.last_us DESC, r.release
         LIMIT ?",
    ))?;
    let pid = project_id as i32;
    let limit = limit.clamp(1, 500) as i64;
    let rows = stmt.query_map(params![pid, start_us(w), end_us(w), pid, limit], |r| {
        Ok(Release {
            version: r.get(0)?,
            first_seen: us_to_dt(r.get(1)?),
            last_seen: us_to_dt(r.get(2)?),
            events: r.get::<_, i64>(3)? as u64,
            issues: r.get::<_, i64>(4)? as u64,
            new_issues: r.get::<_, i64>(5)? as u64,
        })
    })?;
    Ok(ReleaseList { releases: rows.collect::<Result<_, _>>()?, window_start: w.from, window_end: w.to })
}

/// Bucket size and count for a window of `span_us` (> 0): buckets of at
/// least one minute, at most 1000 of them. No overflow for any interval.
fn stats_buckets(span_us: i64, interval_us: Option<i64>) -> (i64, i64) {
    let interval_us = interval_us.unwrap_or(span_us / TREND_BUCKETS).max(60_000_000).max(span_us / 1000);
    let n = span_us / interval_us + i64::from(span_us % interval_us != 0);
    (interval_us, n.max(1))
}

pub fn list_bundles(conn: &Connection, ns: &Ns, project_id: u32) -> Result<Vec<ArtifactBundle>> {
    let mut stmt = conn.prepare(&ns.sql(
        "SELECT b.bundle_id, b.release, b.dist, b.file_count, b.size_bytes, epoch_us(b.created_at),
                coalesce(string_agg(DISTINCT f.debug_id::VARCHAR, ','), '')
         FROM $S.artifact_bundles b
         LEFT JOIN $S.artifact_files f ON f.bundle_id = b.bundle_id AND f.project_id = b.project_id
         WHERE b.project_id = ?
         GROUP BY ALL ORDER BY 6 DESC LIMIT 500",
    ))?;
    let rows = stmt.query_map(params![project_id as i32], |r| {
        Ok(ArtifactBundle {
            id: r.get(0)?,
            release: r.get(1)?,
            dist: r.get(2)?,
            file_count: r.get::<_, i32>(3)? as u32,
            size_bytes: r.get::<_, i64>(4)? as u64,
            created_at: us_to_dt(r.get(5)?),
            debug_ids: r.get::<_, String>(6)?.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect(),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Where a sourcemap lives: bundle object + zip entries.
#[derive(Debug, Clone)]
pub struct ArtifactLocation {
    pub bundle_id: String,
    pub object_key: String,
    pub map_path: String,
    pub map_url: Option<String>,
    pub minified_path: Option<String>,
}

pub fn find_by_debug_id(
    conn: &Connection,
    ns: &Ns,
    project_id: u32,
    debug_id: uuid::Uuid,
) -> Result<Option<ArtifactLocation>> {
    let row = conn
        .query_row(
            &ns.sql(
                "SELECT f.bundle_id, b.object_key, f.zip_path, f.url,
                        (SELECT m.zip_path FROM $S.artifact_files m
                         WHERE m.bundle_id = f.bundle_id AND m.project_id = f.project_id
                           AND m.debug_id = f.debug_id AND m.file_type = 'minified_source' LIMIT 1)
                 FROM $S.artifact_files f
                 JOIN $S.artifact_bundles b ON b.bundle_id = f.bundle_id AND b.project_id = f.project_id
                 WHERE f.project_id = ? AND f.debug_id = ?::UUID AND f.file_type = 'source_map'
                 ORDER BY b.created_at DESC LIMIT 1",
            ),
            params![project_id as i32, debug_id.to_string()],
            |r| {
                Ok(ArtifactLocation {
                    bundle_id: r.get(0)?,
                    object_key: r.get(1)?,
                    map_path: r.get(2)?,
                    map_url: r.get(3)?,
                    minified_path: r.get(4)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// The url of a minified file's map: its `sourcemap` header resolved
/// against the file's url, or `<url>.map`.
fn map_url_of(url: &str, sourcemap_ref: Option<&str>) -> String {
    match sourcemap_ref {
        Some(r) if r.contains("://") || r.starts_with("~/") => r.to_string(),
        Some(r) => match url.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/{r}"),
            None => r.to_string(),
        },
        None => format!("{url}.map"),
    }
}

/// Release-based fallback: find the minified file by url, then its map.
pub fn find_by_url(
    conn: &Connection,
    ns: &Ns,
    project_id: u32,
    release: &str,
    dist: Option<&str>,
    candidates: &[String],
) -> Result<Option<ArtifactLocation>> {
    for url in candidates {
        let row = conn
            .query_row(
                &ns.sql(
                    "SELECT f.bundle_id, b.object_key, f.zip_path, f.sourcemap_url
                     FROM $S.artifact_files f
                     JOIN $S.artifact_bundles b ON b.bundle_id = f.bundle_id AND b.project_id = f.project_id
                     WHERE f.project_id = ? AND f.release = ? AND f.dist IS NOT DISTINCT FROM ?
                       AND f.url = ? AND f.file_type = 'minified_source'
                     ORDER BY b.created_at DESC LIMIT 1",
                ),
                params![project_id as i32, release, dist, url],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((bundle_id, object_key, minified_path, sourcemap_ref)) = row else { continue };
        let map_url = map_url_of(url, sourcemap_ref.as_deref());
        let map = conn
            .query_row(
                &ns.sql(
                    "SELECT zip_path, url FROM $S.artifact_files
                     WHERE bundle_id = ? AND project_id = ? AND file_type = 'source_map' AND url = ? LIMIT 1",
                ),
                params![bundle_id, project_id as i32, map_url],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        if let Some((map_path, map_url)) = map {
            return Ok(Some(ArtifactLocation {
                bundle_id,
                object_key,
                map_path,
                map_url,
                minified_path: Some(minified_path),
            }));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_US: i64 = 86_400_000_000;

    #[test]
    fn cursors() {
        let id = uuid::Uuid::new_v4();
        assert_eq!(Cursor::parse(&format!("123:{}", id.simple())), Some(Cursor { us: 123, id: Some(id) }));
        assert_eq!(Cursor::parse("-5"), Some(Cursor { us: -5, id: None }));
        assert_eq!(Cursor::parse("abc"), None);
        assert_eq!(Cursor::parse("123:not-a-uuid"), None);
    }

    #[test]
    fn issue_cursors_round_trip() {
        let c = IssueCursor { sort: IssueSort::Count, value: 42, group: 0xab, from_us: -1, to_us: 9 };
        assert_eq!(c.to_string(), "count:42:00000000000000ab:-1:9");
        assert_eq!(IssueCursor::parse(&c.to_string()), Some(c));
        for bad in [
            "",
            "count:1:00000000000000ab:0",
            "nope:1:00000000000000ab:0:1",
            "count:x:00000000000000ab:0:1",
            "count:1:ab:0:1",
            "count:1:00000000000000ab:0:1:2",
        ] {
            assert_eq!(IssueCursor::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn like_patterns_are_literal() {
        assert_eq!(like_pattern(Some(" a_b%c\\ ")).as_deref(), Some("%a\\_b\\%c\\\\%"));
        assert_eq!(like_pattern(Some("  ")), None);
    }

    #[test]
    fn map_urls_of_release_artifacts() {
        assert_eq!(map_url_of("~/static/app.js", Some("app.js.map")), "~/static/app.js.map");
        assert_eq!(map_url_of("~/static/app.js", Some("~/maps/app.map")), "~/maps/app.map");
        assert_eq!(map_url_of("~/static/app.js", Some("https://cdn.test/a.map")), "https://cdn.test/a.map");
        assert_eq!(map_url_of("app.js", Some("app.js.map")), "app.js.map");
        assert_eq!(map_url_of("~/static/app.js", None), "~/static/app.js.map");
    }

    #[test]
    fn group_ids_are_strict_hex() {
        assert_eq!(parse_group_id("00f067aa0ba902b7"), Some(0x00f067aa0ba902b7));
        assert_eq!(parse_group_id("+00f067aa0ba902b"), None);
        assert_eq!(parse_group_id("00f067aa0ba902b"), None);
    }

    #[test]
    fn issue_list_filters() {
        assert_eq!(StatusFilter::parse(None), Some(StatusFilter::Only(IssueStatus::Unresolved)));
        assert_eq!(StatusFilter::parse(Some("all")), Some(StatusFilter::All));
        assert_eq!(StatusFilter::parse(Some("ignored")), Some(StatusFilter::Only(IssueStatus::Ignored)));
        assert_eq!(StatusFilter::parse(Some("nope")), None);
        assert_eq!(IssueSort::parse(None), Some(IssueSort::LastSeen));
        assert_eq!(IssueSort::parse(Some("users")), Some(IssueSort::Users));
        assert_eq!(IssueSort::parse(Some("last_sene")), None, "typos are rejected, not ignored");
    }

    #[test]
    fn stats_buckets_default_min_and_max() {
        assert_eq!(stats_buckets(DAY_US, None), (3_600_000_000, 24));
        // At least one minute per bucket.
        assert_eq!(stats_buckets(3_600_000_000, Some(1)), (60_000_000, 60));
        // At most 1000 buckets.
        assert_eq!(stats_buckets(DAY_US, Some(1_000_000)), (86_400_000, 1000));
        // A partial last bucket counts.
        assert_eq!(stats_buckets(DAY_US, Some(DAY_US / 2 + 1)).1, 2);
    }

    #[test]
    fn stats_buckets_huge_interval_does_not_overflow() {
        assert_eq!(stats_buckets(DAY_US, Some(i64::MAX)), (i64::MAX, 1));
        assert_eq!(stats_buckets(i64::MAX, Some(i64::MAX)), (i64::MAX, 1));
    }
}
