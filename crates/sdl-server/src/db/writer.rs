//! The single writer thread.
//!
//! Events are collected for up to `batch_window` (or `batch_max` events)
//! and committed in one DuckLake transaction: one snapshot per batch, not
//! per event. Each request is answered only after its batch committed, so
//! an accepted event is durable.
//!
//! Rows go first into an in-memory staging table with the fast DuckDB
//! appender, then `INSERT INTO <lake>.<schema>.events SELECT ... FROM staging` moves
//! them into the lake. (DuckDB does not allow one transaction to write to
//! two databases, so the staging append runs before the lake transaction.)

use super::{Caches, Ns, ProjectRow, us_to_dt};
use crate::config::Config;
use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use crossbeam_channel::{Receiver, RecvTimeoutError};
use duckdb::{Connection, params};
use sdl_protocol::ProcessedEvent;
use sdl_types::IssueStatus;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

pub struct EventWrite {
    pub project_id: u32,
    pub event: ProcessedEvent,
    pub payload_json: String,
    pub reply: oneshot::Sender<Result<(), Arc<anyhow::Error>>>,
}

/// Errors of writer commands that callers answer differently.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("a project with slug {0:?} already exists")]
    SlugTaken(String),
    #[error("issue not found")]
    IssueNotFound,
    #[error(transparent)]
    Db(#[from] anyhow::Error),
}

pub struct NewBundleFile {
    pub debug_id: Option<uuid::Uuid>,
    pub url: Option<String>,
    pub file_type: String,
    pub zip_path: String,
    pub sourcemap_url: Option<String>,
}

pub struct NewBundle {
    pub bundle_id: String,
    pub project_ids: Vec<u32>,
    pub release: Option<String>,
    pub dist: Option<String>,
    pub object_key: String,
    pub size_bytes: u64,
    pub files: Vec<NewBundleFile>,
}

pub enum WriteCmd {
    Event(Box<EventWrite>),
    CreateProject {
        name: String,
        slug: String,
        platform: Option<String>,
        reply: oneshot::Sender<Result<ProjectRow, WriteError>>,
    },
    SetStatus {
        project_id: u32,
        group_hash: u64,
        status: IssueStatus,
        reason: &'static str,
        reply: oneshot::Sender<Result<(), WriteError>>,
    },
    AddBundle {
        bundle: Box<NewBundle>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Run flush + compaction + retention now (used by tests and the CLI).
    Maintenance {
        reply: oneshot::Sender<Result<()>>,
    },
    /// Write everything still queued, flush inlined data and stop.
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

pub struct Writer {
    conn: Connection,
    ns: Ns,
    rx: Receiver<WriteCmd>,
    caches: Arc<Caches>,
    batch_max: usize,
    batch_window: Duration,
    flush_interval: Duration,
    compact_interval: Duration,
    retention_days: u32,
    catalog_wide_maintenance: bool,
    snapshot_retention: Duration,
    maintenance_threads: usize,
    last_flush: Instant,
    inlined_since_flush: usize,
    flush_rows: usize,
    last_compact: Instant,
}

/// A drained queue with at least this many events commits without waiting
/// for the batch window.
const EARLY_COMMIT_EVENTS: usize = 200;

/// Files merged per table and maintenance run (bounds how long one run blocks
/// the writer; the rest is merged in later runs).
const MAX_COMPACTED_FILES: usize = 1000;

/// Tables compacted by periodic maintenance.
const COMPACTED_TABLES: &[&str] = &["events", "issues", "issue_activity", "artifact_files"];

const STAGING: &str = "CREATE TABLE IF NOT EXISTS $T.staging_events (
    project_id INTEGER, received_us BIGINT, ts_us BIGINT, event_id VARCHAR, group_hash UBIGINT,
    level VARCHAR, platform VARCHAR, environment VARCHAR, release VARCHAR, dist VARCHAR,
    server_name VARCHAR, transaction_name VARCHAR, logger VARCHAR, title VARCHAR, culprit VARCHAR,
    exception_type VARCHAR, exception_value VARCHAR, message VARCHAR, handled BOOLEAN,
    user_key VARCHAR, trace_id VARCHAR, span_id UBIGINT, sdk_name VARCHAR, sdk_version VARCHAR, tags_json VARCHAR,
    size_bytes INTEGER, payload VARCHAR)";

const MOVE_STAGED: &str = "INSERT INTO $S.events SELECT
    project_id, make_timestamp(received_us), make_timestamp(ts_us), event_id::UUID, group_hash,
    level, platform, environment, release, dist, server_name, transaction_name, logger, title, culprit,
    exception_type, exception_value, message, handled, user_key, trace_id::UUID, span_id, sdk_name, sdk_version,
    CAST(tags_json::JSON AS MAP(VARCHAR, VARCHAR)), size_bytes, payload
    FROM $T.staging_events";

pub fn record_schema_version(conn: &Connection, ns: &Ns) -> Result<()> {
    let current: Option<String> = conn
        .query_row(&ns.sql("SELECT max(value) FROM $S.meta WHERE key = 'schema_version'"), [], |r| r.get(0))
        .ok()
        .flatten();
    let want = super::schema::SCHEMA_VERSION.to_string();
    if current.as_deref() != Some(want.as_str()) {
        conn.execute(
            &ns.sql("INSERT INTO $S.meta VALUES ('schema_version', ?, make_timestamp(?))"),
            params![want, Utc::now().timestamp_micros()],
        )?;
    }
    Ok(())
}

pub fn load_caches(conn: &Connection, ns: &Ns, caches: &Caches) -> Result<()> {
    let mut stmt = conn.prepare(
        &ns.sql("SELECT id, slug, name, platform, public_key, epoch_us(created_at) FROM $S.projects ORDER BY id"),
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ProjectRow {
            id: r.get::<_, i32>(0)? as u32,
            slug: r.get(1)?,
            name: r.get(2)?,
            platform: r.get(3)?,
            public_key: r.get(4)?,
            created_at: us_to_dt(r.get(5)?),
        })
    })?;
    let mut projects = caches.projects.write().expect("lock");
    for p in rows {
        let p = p?;
        projects.insert(p.id, p);
    }
    drop(projects);

    let mut stmt = conn.prepare(&ns.sql(
        "SELECT i.project_id, i.group_hash, coalesce(a.status, 'unresolved')
         FROM $S.issues i
         LEFT JOIN (SELECT project_id, group_hash, arg_max(status, changed_at) AS status
                    FROM $S.issue_activity GROUP BY ALL) a USING (project_id, group_hash)",
    ))?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i32>(0)? as u32, r.get::<_, u64>(1)?, r.get::<_, String>(2)?)))?;
    let mut issues = caches.issues.write().expect("lock");
    for row in rows {
        let (p, g, s) = row?;
        issues.insert((p, g), s.parse().unwrap_or_default());
    }
    let n_issues = issues.len();
    drop(issues);
    let mut stmt = conn.prepare(&ns.sql("SELECT DISTINCT bundle_id, project_id, release FROM $S.artifact_bundles"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)? as u32, r.get::<_, Option<String>>(2)?)))?;
    for row in rows {
        let (b, p, r) = row?;
        caches.add_bundle(p, &b, r.as_deref(), []);
    }
    let mut stmt = conn.prepare(&ns.sql(
        "SELECT DISTINCT bundle_id, project_id, debug_id::VARCHAR FROM $S.artifact_files
         WHERE debug_id IS NOT NULL AND file_type = 'source_map'",
    ))?;
    let rows =
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)? as u32, r.get::<_, String>(2)?)))?;
    for row in rows {
        let (b, p, id) = row?;
        caches.add_bundle(p, &b, None, uuid::Uuid::try_parse(&id).ok());
    }
    tracing::info!(
        projects = caches.projects.read().expect("lock").len(),
        issues = n_issues,
        bundles = caches.bundle_count(),
        "loaded caches"
    );
    Ok(())
}

impl Writer {
    pub fn new(conn: Connection, ns: Ns, rx: Receiver<WriteCmd>, caches: Arc<Caches>, cfg: &Config) -> Result<Self> {
        conn.execute_batch(&ns.sql("ATTACH IF NOT EXISTS ':memory:' AS $T"))?;
        conn.execute_batch(&ns.sql(STAGING))?;
        Ok(Writer {
            conn,
            ns,
            rx,
            caches,
            batch_max: cfg.batch_max.max(1),
            batch_window: cfg.batch_window,
            flush_interval: cfg.flush_interval,
            compact_interval: cfg.compact_interval,
            retention_days: cfg.retention_days,
            catalog_wide_maintenance: cfg.catalog_wide_maintenance,
            snapshot_retention: cfg.snapshot_retention,
            maintenance_threads: cfg.maintenance_threads,
            last_flush: Instant::now(),
            last_compact: Instant::now(),
            inlined_since_flush: 0,
            flush_rows: cfg.flush_rows.max(1),
        })
    }

    pub fn run(mut self) {
        tracing::info!(schema = %self.ns.schema, "writer started");
        let mut shutdown_reply = None;
        while shutdown_reply.is_none() {
            let next_maint = self.next_maintenance_in();
            let cmd = match self.rx.recv_timeout(next_maint) {
                Ok(c) => c,
                Err(RecvTimeoutError::Timeout) => {
                    self.periodic_maintenance();
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            match cmd {
                WriteCmd::Event(e) => {
                    // Group commit. Collect for up to `batch_window` after the
                    // first event, so a quiet server makes at most one commit
                    // (= DuckLake snapshot) per window. Under load, commit as
                    // soon as the queue is drained and the batch is big enough
                    // to amortize a commit, so throughput is not capped by
                    // clients / window.
                    let mut batch = vec![*e];
                    let deadline = Instant::now() + self.batch_window;
                    let mut pending = None;
                    while batch.len() < self.batch_max {
                        let next = match self.rx.try_recv() {
                            Ok(cmd) => Ok(cmd),
                            Err(_) if batch.len() >= EARLY_COMMIT_EVENTS => break,
                            Err(_) if Instant::now() >= deadline => break,
                            Err(_) => self.rx.recv_deadline(deadline),
                        };
                        match next {
                            Ok(WriteCmd::Event(e)) => batch.push(*e),
                            Ok(other) => {
                                pending = Some(other);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    self.write_events(batch);
                    match pending {
                        Some(WriteCmd::Shutdown { reply }) => shutdown_reply = Some(reply),
                        Some(cmd) => self.handle(cmd),
                        None => {}
                    }
                }
                WriteCmd::Shutdown { reply } => shutdown_reply = Some(reply),
                other => self.handle(other),
            }
            if shutdown_reply.is_none() {
                self.periodic_maintenance();
            }
        }
        // Drain what is still queued (late requests get answered, not dropped).
        let mut batch = Vec::new();
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                WriteCmd::Event(e) => batch.push(*e),
                WriteCmd::Shutdown { reply } => {
                    let _ = reply.send(());
                }
                other => self.handle(other),
            }
        }
        if !batch.is_empty() {
            self.write_events(batch);
        }
        // Flush inlined data so a clean shutdown leaves everything in Parquet.
        if let Err(e) = self.flush_inlined() {
            tracing::warn!(error = format!("{e:#}"), "final flush failed");
        }
        tracing::info!("writer stopped");
        if let Some(reply) = shutdown_reply {
            let _ = reply.send(());
        }
    }

    fn next_maintenance_in(&self) -> Duration {
        let f = self.flush_interval.saturating_sub(self.last_flush.elapsed());
        let c = self.compact_interval.saturating_sub(self.last_compact.elapsed());
        f.min(c).max(Duration::from_millis(10))
    }

    fn periodic_maintenance(&mut self) {
        if self.last_flush.elapsed() >= self.flush_interval || self.inlined_since_flush >= self.flush_rows {
            if let Err(e) = self.flush_inlined() {
                tracing::error!(error = format!("{e:#}"), "flushing inlined data failed");
            }
            self.last_flush = Instant::now();
        }
        if self.last_compact.elapsed() >= self.compact_interval {
            if let Err(e) = self.compact_and_expire() {
                tracing::error!(error = format!("{e:#}"), "compaction failed");
            }
            self.last_compact = Instant::now();
        }
    }

    fn handle(&mut self, cmd: WriteCmd) {
        match cmd {
            WriteCmd::Event(e) => self.write_events(vec![*e]),
            WriteCmd::CreateProject { name, slug, platform, reply } => {
                let _ = reply.send(self.create_project(name, slug, platform));
            }
            WriteCmd::SetStatus { project_id, group_hash, status, reason, reply } => {
                let _ = reply.send(self.set_status(project_id, group_hash, status, reason));
            }
            WriteCmd::AddBundle { bundle, reply } => {
                let _ = reply.send(self.add_bundle(&bundle));
            }
            WriteCmd::Shutdown { reply } => {
                // Only reached while draining; run() handles the first one.
                let _ = reply.send(());
            }
            WriteCmd::Maintenance { reply } => {
                let r = self.flush_inlined().and_then(|_| self.compact_and_expire());
                self.last_flush = Instant::now();
                self.last_compact = Instant::now();
                let _ = reply.send(r);
            }
        }
    }

    fn in_tx<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN TRANSACTION")?;
        match f(&self.conn) {
            Ok(v) => {
                if let Err(e) = self.conn.execute_batch("COMMIT") {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(e.into());
                }
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn write_events(&mut self, batch: Vec<EventWrite>) {
        let started = Instant::now();
        let n = batch.len();
        let result = self.try_write_events(&batch);
        let elapsed = started.elapsed();
        metrics::histogram!("sdl_write_batch_seconds").record(elapsed.as_secs_f64());
        metrics::histogram!("sdl_write_batch_events").record(n as f64);
        match &result {
            Ok(()) => {
                if n <= super::schema::EVENTS_INLINE_ROWS {
                    self.inlined_since_flush += n;
                }
                metrics::counter!("sdl_events_stored_total").increment(n as u64);
                tracing::debug!(events = n, ms = elapsed.as_millis() as u64, "batch committed");
            }
            Err(e) => {
                metrics::counter!("sdl_write_errors_total").increment(1);
                tracing::error!(events = n, error = format!("{e:#}"), "batch failed");
            }
        }
        let reply = result.map_err(Arc::new);
        for ev in batch {
            let _ = ev.reply.send(reply.clone());
        }
    }

    fn try_write_events(&mut self, batch: &[EventWrite]) -> Result<()> {
        let ns = &self.ns.clone();
        // Decide new issues and regressions against the cache, but only
        // apply cache changes after the commit succeeded.
        let mut new_issues: Vec<&EventWrite> = Vec::new();
        let mut activity: Vec<(u32, u64, IssueStatus, &'static str)> = Vec::new();
        let mut seen_in_batch: HashMap<(u32, u64), ()> = HashMap::new();
        {
            let issues = self.caches.issues.read().expect("lock");
            for w in batch {
                let key = (w.project_id, w.event.group_hash);
                if seen_in_batch.insert(key, ()).is_some() {
                    continue;
                }
                match issues.get(&key) {
                    None => new_issues.push(w),
                    Some(IssueStatus::Resolved) => activity.push((key.0, key.1, IssueStatus::Unresolved, "regression")),
                    Some(_) => {}
                }
            }
        }

        // A failed append in an earlier batch may have left rows behind.
        self.conn.execute_batch(&ns.sql("DELETE FROM $T.staging_events"))?;
        {
            let mut app = self.conn.appender_to_catalog_and_db("staging_events", &ns.staging, "main")?;
            for w in batch {
                let e = &w.event;
                let tags: serde_json::Map<String, serde_json::Value> =
                    e.tags.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
                app.append_row(params![
                    w.project_id as i32,
                    e.received_at.timestamp_micros(),
                    e.timestamp.timestamp_micros(),
                    e.event_id.to_string(),
                    e.group_hash,
                    e.level,
                    e.platform,
                    e.environment,
                    e.release,
                    e.dist,
                    e.server_name,
                    e.transaction,
                    e.logger,
                    e.title,
                    e.culprit,
                    e.exception_type,
                    e.exception_value,
                    e.message,
                    e.handled,
                    e.user_key,
                    e.trace_id.map(|t| t.to_string()),
                    e.span_id,
                    e.sdk_name,
                    e.sdk_version,
                    serde_json::Value::Object(tags).to_string(),
                    w.payload_json.len() as i32,
                    w.payload_json,
                ])?;
            }
            app.flush()?;
        }

        let now_us = Utc::now().timestamp_micros();
        let result = self.in_tx(|c| {
            c.execute_batch(&ns.sql(MOVE_STAGED))?;
            if !new_issues.is_empty() {
                let mut stmt = c.prepare(
                    &ns.sql("INSERT INTO $S.issues VALUES (?, ?, make_timestamp(?), ?::UUID, ?, ?, ?, ?, ?, ?)"),
                )?;
                for w in &new_issues {
                    let e = &w.event;
                    stmt.execute(params![
                        w.project_id as i32,
                        e.group_hash,
                        e.received_at.timestamp_micros(),
                        e.event_id.to_string(),
                        e.release,
                        e.title,
                        e.culprit,
                        e.level,
                        e.platform,
                        e.exception_type,
                    ])?;
                }
            }
            if !activity.is_empty() {
                let mut stmt =
                    c.prepare(&ns.sql("INSERT INTO $S.issue_activity VALUES (?, ?, make_timestamp(?), ?, ?)"))?;
                for (p, g, s, reason) in &activity {
                    stmt.execute(params![*p as i32, *g, now_us, s.as_str(), *reason])?;
                }
            }
            Ok(())
        });
        self.conn.execute_batch(&ns.sql("DELETE FROM $T.staging_events"))?;
        result?;

        let mut issues = self.caches.issues.write().expect("lock");
        for w in new_issues {
            issues.insert((w.project_id, w.event.group_hash), IssueStatus::Unresolved);
        }
        for (p, g, s, _) in activity {
            issues.insert((p, g), s);
        }
        Ok(())
    }

    fn create_project(
        &mut self,
        name: String,
        slug: String,
        platform: Option<String>,
    ) -> Result<ProjectRow, WriteError> {
        let ns = &self.ns.clone();
        let id = {
            let projects = self.caches.projects.read().expect("lock");
            if projects.values().any(|p| p.slug == slug) {
                return Err(WriteError::SlugTaken(slug));
            }
            projects.keys().max().copied().unwrap_or(0) + 1
        };
        let mut key = [0u8; 16];
        rand::fill(&mut key);
        let row = ProjectRow { id, slug, name, platform, public_key: hex::encode(key), created_at: Utc::now() };
        self.in_tx(|c| {
            c.execute(
                &ns.sql("INSERT INTO $S.projects VALUES (?, ?, ?, ?, ?, make_timestamp(?))"),
                params![
                    row.id as i32,
                    row.slug,
                    row.name,
                    row.platform,
                    row.public_key,
                    row.created_at.timestamp_micros()
                ],
            )?;
            Ok(())
        })?;
        self.caches.projects.write().expect("lock").insert(id, row.clone());
        Ok(row)
    }

    fn set_status(
        &mut self,
        project_id: u32,
        group_hash: u64,
        status: IssueStatus,
        reason: &str,
    ) -> Result<(), WriteError> {
        let ns = &self.ns.clone();
        if !self.caches.issues.read().expect("lock").contains_key(&(project_id, group_hash)) {
            return Err(WriteError::IssueNotFound);
        }
        self.in_tx(|c| {
            c.execute(
                &ns.sql("INSERT INTO $S.issue_activity VALUES (?, ?, make_timestamp(?), ?, ?)"),
                params![project_id as i32, group_hash, Utc::now().timestamp_micros(), status.as_str(), reason],
            )?;
            Ok(())
        })?;
        self.caches.issues.write().expect("lock").insert((project_id, group_hash), status);
        Ok(())
    }

    fn add_bundle(&mut self, b: &NewBundle) -> Result<()> {
        let ns = &self.ns.clone();
        let now = Utc::now().timestamp_micros();
        self.in_tx(|c| {
            let mut sb =
                c.prepare(&ns.sql("INSERT INTO $S.artifact_bundles VALUES (?, ?, ?, ?, ?, ?, ?, make_timestamp(?))"))?;
            let mut sf =
                c.prepare(&ns.sql("INSERT INTO $S.artifact_files VALUES (?, ?, ?::UUID, ?, ?, ?, ?, ?, ?)"))?;
            for p in &b.project_ids {
                // Idempotent per (bundle, project): the same bundle may be
                // uploaded for several projects, or uploaded again.
                let existing: i64 = c.query_row(
                    &ns.sql("SELECT count(*) FROM $S.artifact_bundles WHERE bundle_id = ? AND project_id = ?"),
                    params![b.bundle_id, *p as i32],
                    |r| r.get(0),
                )?;
                if existing > 0 {
                    continue;
                }
                sb.execute(params![
                    b.bundle_id,
                    *p as i32,
                    b.release,
                    b.dist,
                    b.object_key,
                    b.size_bytes as i64,
                    b.files.len() as i32,
                    now
                ])?;
                for f in &b.files {
                    sf.execute(params![
                        b.bundle_id,
                        *p as i32,
                        f.debug_id.map(|d| d.to_string()),
                        f.url,
                        f.file_type,
                        f.zip_path,
                        f.sourcemap_url,
                        b.release,
                        b.dist
                    ])?;
                }
            }
            Ok(())
        })?;
        let debug_ids = || b.files.iter().filter(|f| f.file_type == "source_map").filter_map(|f| f.debug_id);
        for p in &b.project_ids {
            self.caches.add_bundle(*p, &b.bundle_id, b.release.as_deref(), debug_ids());
        }
        Ok(())
    }

    fn flush_inlined(&mut self) -> Result<()> {
        let started = Instant::now();
        let sql = self.ns.sql("CALL ducklake_flush_inlined_data($C, schema_name => $N)");
        self.conn.execute_batch(&sql).context("ducklake_flush_inlined_data")?;
        metrics::histogram!("sdl_flush_seconds").record(started.elapsed().as_secs_f64());
        tracing::debug!(ms = started.elapsed().as_millis() as u64, "flushed inlined data");
        Ok(())
    }

    fn compact_and_expire(&mut self) -> Result<()> {
        let ns = &self.ns.clone();
        let started = Instant::now();
        let mut errors = Vec::new();
        if self.retention_days > 0 {
            // Day-aligned cutoff: whole daily partitions are dropped without
            // writing delete files.
            let today = Utc::now().date_naive().and_hms_opt(0, 0, 0).expect("midnight").and_utc();
            let cutoff = (today - chrono::Duration::days(self.retention_days as i64)).timestamp_micros();
            match self
                .conn
                .execute(&ns.sql("DELETE FROM $S.events WHERE received_at < make_timestamp(?)"), params![cutoff])
            {
                Ok(0) => {}
                Ok(deleted) => tracing::info!(deleted, "retention removed old events"),
                Err(e) => errors.push(format!("retention: {e}")),
            }
        }
        // Sorted merges need memory per thread: compact with fewer threads,
        // then restore the previous setting.
        let restore_threads: Option<i64> = match self.maintenance_threads {
            0 => None,
            n => {
                let current: Option<i64> =
                    self.conn.query_row("SELECT current_setting('threads')::BIGINT", [], |r| r.get(0)).ok();
                if current.is_some_and(|c| c > n as i64) {
                    let _ = self.conn.execute_batch(&format!("SET threads = {n}"));
                    current
                } else {
                    None
                }
            }
        };
        // Each step runs even if an earlier one failed, so snapshots keep
        // expiring when e.g. one table cannot be compacted.
        for table in COMPACTED_TABLES {
            let sql = ns.sql(&format!(
                "CALL ducklake_merge_adjacent_files($C, '{table}', schema => $N, max_compacted_files => {MAX_COMPACTED_FILES});
                 CALL ducklake_rewrite_data_files($C, '{table}', schema => $N, max_compacted_files => {MAX_COMPACTED_FILES});"
            ));
            if let Err(e) = self.conn.execute_batch(&sql) {
                errors.push(format!("compacting {table}: {e}"));
            }
        }
        if let Some(t) = restore_threads {
            let _ = self.conn.execute_batch(&format!("SET threads = {t}"));
        }
        if self.catalog_wide_maintenance {
            let cutoff = (Utc::now() - chrono::Duration::from_std(self.snapshot_retention).unwrap_or_default())
                .timestamp_micros();
            for (what, sql) in [
                ("expiring snapshots", "CALL ducklake_expire_snapshots($C, older_than => make_timestamp(?))"),
                ("cleaning up old files", "CALL ducklake_cleanup_old_files($C, older_than => make_timestamp(?))"),
            ] {
                if let Err(e) = self.conn.execute(&ns.sql(sql), params![cutoff]) {
                    errors.push(format!("{what}: {e}"));
                }
            }
        }
        metrics::histogram!("sdl_compact_seconds").record(started.elapsed().as_secs_f64());
        if errors.is_empty() {
            tracing::info!(ms = started.elapsed().as_millis() as u64, "compaction and snapshot expiry done");
            Ok(())
        } else {
            metrics::counter!("sdl_maintenance_errors_total").increment(errors.len() as u64);
            Err(anyhow!(errors.join("; ")))
        }
    }
}
