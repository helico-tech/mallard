//! Read queries for the `/v1` API. Every query filters on `project_id` and,
//! where possible, on a `received_at` window (partition pruning) and
//! `group_hash` (row-group pruning thanks to the sort order).

use super::{Ns, us_to_dt};
use anyhow::Result;
use chrono::{DateTime, Utc};
use duckdb::types::ToSql;
use duckdb::{Connection, OptionalExt, params, params_from_iter};
use sdl_types::*;
use std::collections::HashMap;

pub fn group_id(hash: u64) -> String {
    format!("{hash:016x}")
}

pub fn parse_group_id(s: &str) -> Option<u64> {
    (s.len() == 16).then(|| u64::from_str_radix(s, 16).ok()).flatten()
}

pub const TREND_BUCKETS: i64 = 24;

pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Window {
    pub fn parse(since: Option<&str>, default: &str, now: DateTime<Utc>) -> Result<Window, String> {
        let s = since.unwrap_or(default);
        let d = humantime::parse_duration(s).map_err(|e| format!("invalid duration {s:?}: {e}"))?;
        let d = chrono::Duration::from_std(d).map_err(|e| e.to_string())?;
        Ok(Window { start: now - d, end: now })
    }
    fn start_us(&self) -> i64 {
        self.start.timestamp_micros()
    }
    fn span_us(&self) -> i64 {
        (self.end - self.start).num_microseconds().unwrap_or(i64::MAX).max(1)
    }
}

/// Bucket a set of issues' events over the window into `TREND_BUCKETS`.
fn trends(conn: &Connection, ns: &Ns, project_id: u32, w: &Window, groups: &[u64]) -> Result<HashMap<u64, Vec<u64>>> {
    let mut out: HashMap<u64, Vec<u64>> = groups.iter().map(|g| (*g, vec![0; TREND_BUCKETS as usize])).collect();
    if groups.is_empty() {
        return Ok(out);
    }
    let bucket = (w.span_us() / TREND_BUCKETS).max(1);
    // Numbers only, so building the IN list is injection-safe.
    let list = groups.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let sql = ns.sql(&format!(
        "SELECT group_hash, CAST(floor((epoch_us(received_at) - ?) / ?) AS BIGINT) AS b, count(*)
         FROM $S.events
         WHERE project_id = ? AND received_at >= make_timestamp(?) AND group_hash IN ({list})
         GROUP BY ALL"
    ));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![w.start_us(), bucket, project_id as i32, w.start_us()], |r| {
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

pub fn list_issues(conn: &Connection, ns: &Ns, project_id: u32, q: &IssueQuery, w: &Window) -> Result<IssueList> {
    let status = q.status.clone().unwrap_or_else(|| "unresolved".into());
    let sort = match q.sort.as_deref() {
        Some("first_seen") => "first_us",
        Some("count") => "cnt",
        Some("users") => "users",
        _ => "last_us",
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 500) as i64;

    let mut filters = String::new();
    let mut p: Vec<Box<dyn ToSql>> = vec![Box::new(project_id as i32), Box::new(w.start_us())];
    if let Some(env) = &q.environment {
        filters.push_str(" AND environment = ?");
        p.push(Box::new(env.clone()));
    }
    if let Some(rel) = &q.release {
        filters.push_str(" AND release = ?");
        p.push(Box::new(rel.clone()));
    }
    p.push(Box::new(project_id as i32)); // st
    p.push(Box::new(project_id as i32)); // issues join
    p.push(Box::new(status.clone()));
    p.push(Box::new(status.clone()));
    let mut search = String::new();
    if let Some(text) = q.query.as_ref().filter(|t| !t.trim().is_empty()) {
        search.push_str(" AND (agg.title ILIKE ? OR agg.culprit ILIKE ?)");
        let like = format!("%{}%", text.trim().replace('%', "\\%").replace('_', "\\_"));
        p.push(Box::new(like.clone()));
        p.push(Box::new(like));
    }
    p.push(Box::new(limit));

    let sql = ns.sql(&format!(
        "WITH agg AS (
            SELECT group_hash, count(*) AS cnt, count(DISTINCT user_key) AS users,
                   epoch_us(max(received_at)) AS last_us,
                   arg_max(title, received_at) AS title, arg_max(culprit, received_at) AS culprit,
                   arg_max(level, received_at) AS level
            FROM $S.events
            WHERE project_id = ? AND received_at >= make_timestamp(?){filters}
            GROUP BY group_hash
         ), st AS (
            SELECT group_hash, arg_max(status, changed_at) AS status
            FROM $S.issue_activity WHERE project_id = ? GROUP BY group_hash
         )
         SELECT agg.group_hash, agg.title, agg.culprit, agg.level, i.platform, i.exception_type,
                coalesce(st.status, 'unresolved') AS status, epoch_us(i.first_seen) AS first_us,
                agg.cnt, agg.users, agg.last_us
         FROM agg
         JOIN $S.issues i ON i.project_id = ? AND i.group_hash = agg.group_hash
         LEFT JOIN st ON st.group_hash = agg.group_hash
         WHERE (? = 'all' OR coalesce(st.status, 'unresolved') = ?){search}
         ORDER BY {sort} DESC
         LIMIT ?"
    ));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(p.iter().map(|b| b.as_ref())), |r| {
        Ok(IssueSummary {
            id: group_id(r.get(0)?),
            project_id,
            title: r.get(1)?,
            culprit: r.get(2)?,
            level: r.get(3)?,
            platform: r.get(4)?,
            exception_type: r.get(5)?,
            status: IssueStatus::parse(&r.get::<_, String>(6)?).unwrap_or_default(),
            first_seen: us_to_dt(r.get(7)?),
            count: r.get::<_, i64>(8)? as u64,
            user_count: r.get::<_, i64>(9)? as u64,
            last_seen: us_to_dt(r.get(10)?),
            trend: vec![],
        })
    })?;
    let mut issues: Vec<IssueSummary> = rows.collect::<Result<_, _>>()?;
    let groups: Vec<u64> = issues.iter().filter_map(|i| parse_group_id(&i.id)).collect();
    let mut t = trends(conn, ns, project_id, w, &groups)?;
    for i in &mut issues {
        if let Some(v) = parse_group_id(&i.id).and_then(|g| t.remove(&g)) {
            i.trend = v;
        }
    }
    Ok(IssueList { issues, window_start: w.start, window_end: w.end })
}

pub fn issue_detail(conn: &Connection, ns: &Ns, project_id: u32, group: u64, w: &Window) -> Result<Option<IssueDetail>> {
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
             FROM $S.events WHERE project_id = ? AND group_hash = ? AND received_at >= make_timestamp(?)",
        ),
        params![project_id as i32, group, w.start_us()],
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
        activity.push(IssueActivity { at: us_to_dt(at), status: IssueStatus::parse(&s).unwrap_or_default(), reason });
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
    }))
}

fn tag_facets(conn: &Connection, ns: &Ns, project_id: u32, group: u64, w: &Window) -> Result<Vec<TagFacet>> {
    let mut stmt = conn.prepare(&ns.sql(
        "SELECT k, v, count(*) AS c FROM (
            SELECT unnest(map_keys(tags)) AS k, unnest(map_values(tags)) AS v
            FROM $S.events WHERE project_id = ? AND group_hash = ? AND received_at >= make_timestamp(?)
         ) GROUP BY k, v ORDER BY k, c DESC",
    ))?;
    let mut facets: Vec<TagFacet> = Vec::new();
    for row in stmt.query_map(params![project_id as i32, group, w.start_us()], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
    })? {
        let (k, v, c) = row?;
        match facets.last_mut() {
            Some(f) if f.key == k => {
                f.unique += 1;
                if f.top.len() < 10 {
                    f.top.push(TagValueCount { value: v, count: c as u64 });
                }
            }
            _ => facets.push(TagFacet { key: k, unique: 1, top: vec![TagValueCount { value: v, count: c as u64 }] }),
        }
    }
    Ok(facets)
}

const EVENT_COLUMNS: &str = "event_id::VARCHAR, group_hash, epoch_us(timestamp), epoch_us(received_at), title, level,
    platform, environment, release, user_key, message, trace_id::VARCHAR, span_id, project_id";

fn event_summary(r: &duckdb::Row<'_>) -> duckdb::Result<EventSummary> {
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

pub fn list_events(
    conn: &Connection,
    ns: &Ns,
    project_id: u32,
    group: Option<u64>,
    q: &EventQuery,
) -> Result<EventList> {
    let limit = q.limit.unwrap_or(50).clamp(1, 500) as i64;
    let mut sql = format!("SELECT {EVENT_COLUMNS} FROM $S.events WHERE project_id = ?");
    let mut p: Vec<Box<dyn ToSql>> = vec![Box::new(project_id as i32)];
    if let Some(g) = group {
        sql.push_str(" AND group_hash = ?");
        p.push(Box::new(g));
    }
    if let Some(t) = q.trace_id.as_deref() {
        let t = sdl_protocol::event::parse_trace_id(t).ok_or_else(|| anyhow::anyhow!("trace_id must be 32 hex characters"))?;
        sql.push_str(" AND trace_id = ?::UUID");
        p.push(Box::new(t.to_string()));
    }
    if let Some(before) = q.before.as_deref().and_then(|c| c.parse::<i64>().ok()) {
        sql.push_str(" AND received_at < make_timestamp(?)");
        p.push(Box::new(before));
    }
    sql.push_str(" ORDER BY received_at DESC LIMIT ?");
    p.push(Box::new(limit));
    let mut stmt = conn.prepare(&ns.sql(&sql))?;
    let events: Vec<EventSummary> =
        stmt.query_map(params_from_iter(p.iter().map(|b| b.as_ref())), event_summary)?.collect::<Result<_, _>>()?;
    let next_cursor = (events.len() as i64 == limit)
        .then(|| events.last().map(|e| e.received_at.timestamp_micros().to_string()))
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
        "SELECT {EVENT_COLUMNS} FROM $S.events WHERE received_at >= make_timestamp(?) AND trace_id = ?::UUID"
    );
    let mut p: Vec<Box<dyn ToSql>> = vec![Box::new(w.start_us()), Box::new(trace_id.to_string())];
    if let Some(s) = span_id {
        sql.push_str(" AND span_id = ?");
        p.push(Box::new(s));
    }
    sql.push_str(" ORDER BY received_at DESC LIMIT ?");
    p.push(Box::new(limit.clamp(1, 500) as i64));
    let mut stmt = conn.prepare(&ns.sql(&sql))?;
    let events = stmt.query_map(params_from_iter(p.iter().map(|b| b.as_ref())), event_summary)?.collect::<Result<_, _>>()?;
    Ok(EventList { events, next_cursor: None })
}

pub fn event_detail(
    conn: &Connection,
    ns: &Ns,
    project_id: u32,
    event_id: uuid::Uuid,
    group: Option<u64>,
) -> Result<Option<EventDetail>> {
    let mut sql = format!(
        "SELECT {EVENT_COLUMNS}, culprit, CAST(tags AS JSON)::VARCHAR, payload FROM $S.events
         WHERE project_id = ? AND event_id = ?::UUID"
    );
    let mut p: Vec<Box<dyn ToSql>> = vec![Box::new(project_id as i32), Box::new(event_id.to_string())];
    if let Some(g) = group {
        sql.push_str(" AND group_hash = ?");
        p.push(Box::new(g));
    }
    sql.push_str(" LIMIT 1");
    let row = conn
        .query_row(&ns.sql(&sql), params_from_iter(p.iter().map(|b| b.as_ref())), |r| {
            Ok((event_summary(r)?, r.get::<_, Option<String>>(14)?, r.get::<_, Option<String>>(15)?, r.get::<_, String>(16)?))
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
            payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
        }
    }))
}

pub fn project_stats(
    conn: &Connection,
    ns: &Ns,
    project_id: u32,
    w: &Window,
    interval: Option<std::time::Duration>,
) -> Result<ProjectStats> {
    let interval_us = interval
        .map(|d| d.as_micros() as i64)
        .unwrap_or(w.span_us() / TREND_BUCKETS)
        .max(60_000_000)
        .max(w.span_us() / 1000); // at most 1000 buckets
    let n = ((w.span_us() + interval_us - 1) / interval_us).max(1);
    let mut buckets: Vec<StatsBucket> = (0..n)
        .map(|i| StatsBucket {
            start: us_to_dt(w.start_us() + i * interval_us),
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
         FROM $S.events WHERE project_id = ? AND received_at >= make_timestamp(?)
         GROUP BY b",
    ))?;
    for row in stmt.query_map(params![w.start_us(), interval_us, project_id as i32, w.start_us()], |r| {
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
             FROM $S.events WHERE project_id = ? AND received_at >= make_timestamp(?)",
        ),
        params![project_id as i32, w.start_us()],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
    )?;
    let new_issues: i64 = conn.query_row(
        &ns.sql("SELECT count(*) FROM $S.issues WHERE project_id = ? AND first_seen >= make_timestamp(?)"),
        params![project_id as i32, w.start_us()],
        |r| r.get(0),
    )?;
    Ok(ProjectStats {
        project_id,
        window_start: w.start,
        window_end: w.end,
        interval_seconds: (interval_us / 1_000_000) as u64,
        total_events: total as u64,
        unique_issues: issues as u64,
        new_issues: new_issues as u64,
        affected_users: users as u64,
        buckets,
    })
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

pub fn find_by_debug_id(conn: &Connection, ns: &Ns, project_id: u32, debug_id: uuid::Uuid) -> Result<Option<ArtifactLocation>> {
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
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?)),
            )
            .optional()?;
        let Some((bundle_id, object_key, minified_path, sourcemap_ref)) = row else { continue };
        // Resolve the `sourcemap` header relative to the minified url.
        let map_url = match &sourcemap_ref {
            Some(r) if r.contains("://") || r.starts_with("~/") => r.clone(),
            Some(r) => match url.rsplit_once('/') {
                Some((dir, _)) => format!("{dir}/{r}"),
                None => r.clone(),
            },
            None => format!("{url}.map"),
        };
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
            return Ok(Some(ArtifactLocation { bundle_id, object_key, map_path, map_url, minified_path: Some(minified_path) }));
        }
    }
    Ok(None)
}
