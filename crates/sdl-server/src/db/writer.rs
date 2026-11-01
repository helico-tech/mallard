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
//! two databases, so the staging append runs before the lake transaction.
//!
//! Flush and compaction run next to this thread ([`super::maintenance`]).
//! A commit that conflicts with another process is tried again.

use super::maintenance::{MaintCmd, flush_inlined};
use super::schema::{OVERFLOW_GROUP, OVERFLOW_TITLE};
use super::{Caches, Ns, ProjectRow, us_to_dt};
use crate::config::Config;
use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use crossbeam_channel::{Receiver, Sender};
use mallard_duck::duckdb::{Connection, params};
use sdl_protocol::ProcessedEvent;
use sdl_types::IssueStatus;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
        reason: String,
        /// Whether the status changed (false: it already had it; nothing written).
        reply: oneshot::Sender<Result<bool, WriteError>>,
    },
    AddBundle {
        bundle: Box<NewBundle>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Run flush + compaction + retention now (used by tests and the CLI).
    /// Handed to the maintenance thread after what is queued before it.
    Maintenance {
        reply: oneshot::Sender<Result<()>>,
    },
    /// Delete events past retention. On this thread: a `DELETE` that removes
    /// rows conflicts with every concurrent insert into the table (ADR 0014).
    Retention {
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
    retention: Duration,
    /// The maintenance thread, for [`WriteCmd::Maintenance`].
    maintainer: Sender<MaintCmd>,
    /// Rows inlined since the last flush; the maintenance thread reads it.
    inlined: Arc<AtomicUsize>,
    new_issues: NewIssueCount,
}

/// New issues per project in the current hour, of this process.
#[derive(Default)]
struct NewIssueCount(HashMap<u32, (i64, u32)>);

impl NewIssueCount {
    fn get(&self, project: u32, hour: i64) -> u32 {
        self.0.get(&project).filter(|(h, _)| *h == hour).map_or(0, |(_, n)| *n)
    }
    fn add(&mut self, project: u32, hour: i64, n: u32) {
        let total = self.get(project, hour) + n;
        self.0.insert(project, (hour, total));
    }
}

/// A drained queue with at least this many events commits without waiting
/// for the batch window.
const EARLY_COMMIT_EVENTS: usize = 200;

/// New issues per project, per hour and process. Events of further new groups
/// go into one "too many issues" issue ([`OVERFLOW_GROUP`]). Far above any
/// real release; a limit for grouping gone wrong (ids in a message or a
/// fingerprint).
pub const MAX_NEW_ISSUES_PER_HOUR: u32 = 1000;

/// Commits tried again after a conflict with another process. Doubling from
/// 20 ms, this waits at most about 2.5 s; then the batch fails and the SDK
/// retries it.
const MAX_COMMIT_RETRIES: u32 = 7;

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

/// Store the schema version once (the latest row wins).
pub fn record_schema_version(conn: &Connection, ns: &Ns) -> Result<()> {
    // Compare as numbers: as text, "10" < "9".
    let current: Option<i32> = conn
        .query_row(
            &ns.sql("SELECT max(TRY_CAST(value AS INTEGER)) FROM $S.meta WHERE key = 'schema_version'"),
            [],
            |r| r.get(0),
        )
        .context("reading the schema version")?;
    let want = super::schema::SCHEMA_VERSION;
    if current != Some(want) {
        conn.execute(
            &ns.sql("INSERT INTO $S.meta VALUES ('schema_version', ?, make_timestamp(?))"),
            params![want.to_string(), Utc::now().timestamp_micros()],
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
        bundles = caches.bundle_count(),
        "loaded caches"
    );
    Ok(())
}

impl Writer {
    pub fn new(
        conn: Connection,
        ns: Ns,
        rx: Receiver<WriteCmd>,
        caches: Arc<Caches>,
        cfg: &Config,
        maintainer: Sender<MaintCmd>,
        inlined: Arc<AtomicUsize>,
    ) -> Result<Self> {
        conn.execute_batch(&ns.sql("ATTACH IF NOT EXISTS ':memory:' AS $T"))?;
        conn.execute_batch(&ns.sql(STAGING))?;
        Ok(Writer {
            conn,
            ns,
            rx,
            caches,
            batch_max: cfg.ingest.batch_max.max(1),
            batch_window: cfg.ingest.batch_window,
            retention: cfg.retention.events,
            maintainer,
            inlined,
            new_issues: NewIssueCount::default(),
        })
    }

    pub fn run(mut self) {
        tracing::info!(schema = %self.ns.schema, "writer started");
        let mut shutdown_reply = None;
        while shutdown_reply.is_none() {
            let cmd = match self.rx.recv() {
                Ok(c) => c,
                Err(_) => break,
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
        // The maintenance thread has stopped by now (`Db::shutdown`).
        if let Err(e) = flush_inlined(&self.conn, &self.ns) {
            tracing::warn!(error = format!("{e:#}"), "final flush failed");
        }
        tracing::info!("writer stopped");
        if let Some(reply) = shutdown_reply {
            let _ = reply.send(());
        }
    }

    fn handle(&mut self, cmd: WriteCmd) {
        match cmd {
            WriteCmd::Event(e) => self.write_events(vec![*e]),
            WriteCmd::CreateProject { name, slug, platform, reply } => {
                let _ = reply.send(self.create_project(name, slug, platform));
            }
            WriteCmd::SetStatus { project_id, group_hash, status, reason, reply } => {
                let _ = reply.send(self.set_status(project_id, group_hash, status, &reason));
            }
            WriteCmd::AddBundle { bundle, reply } => {
                let _ = reply.send(self.add_bundle(&bundle));
            }
            WriteCmd::Shutdown { reply } => {
                // Only reached while draining; run() handles the first one.
                let _ = reply.send(());
            }
            WriteCmd::Maintenance { reply } => {
                // Never wait here: that would pause ingest.
                if let Err(e) = self.maintainer.try_send(MaintCmd::RunNow { reply })
                    && let MaintCmd::RunNow { reply } = e.into_inner()
                {
                    let _ = reply.send(Err(anyhow!("maintenance is busy or stopped")));
                }
            }
            WriteCmd::Retention { reply } => {
                let _ = reply.send(self.retention());
            }
        }
    }

    /// Run `f` in one transaction. Again when the commit conflicts with
    /// another process (or its retention): nothing was written then.
    fn in_tx<T>(&self, f: impl Fn(&Connection) -> Result<T>) -> Result<T> {
        let mut attempt = 0;
        loop {
            match self.try_tx(&f) {
                Err(e) if attempt < MAX_COMMIT_RETRIES && mallard_duck::is_conflict(&e) => {
                    metrics::counter!("sdl_write_conflicts_total").increment(1);
                    tracing::debug!(attempt, error = format!("{e:#}"), "commit conflict, trying again");
                    std::thread::sleep(Duration::from_millis(20 << attempt));
                    attempt += 1;
                }
                r => return r,
            }
        }
    }

    fn try_tx<T>(&self, f: &impl Fn(&Connection) -> Result<T>) -> Result<T> {
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

    fn write_events(&mut self, mut batch: Vec<EventWrite>) {
        let started = Instant::now();
        let n = batch.len();
        let result = self.try_write_events(&mut batch);
        let elapsed = started.elapsed();
        metrics::histogram!("sdl_write_batch_seconds").record(elapsed.as_secs_f64());
        metrics::histogram!("sdl_write_batch_events").record(n as f64);
        match &result {
            Ok(()) => {
                if n <= super::schema::EVENTS_INLINE_ROWS {
                    self.inlined.fetch_add(n, Ordering::Relaxed);
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

    /// Put issues among `keys` that are not cached but exist in the lake into
    /// the cache. The cache is bounded, and other processes create issues.
    fn load_issues(&self, keys: impl IntoIterator<Item = (u32, u64)>) -> Result<()> {
        let missing: HashSet<(u32, u64)> = keys.into_iter().filter(|k| !self.caches.issues.contains_key(k)).collect();
        if missing.is_empty() {
            return Ok(());
        }
        // Numbers only, so building the IN lists is injection-safe.
        let list = |items: HashSet<String>| items.into_iter().collect::<Vec<_>>().join(",");
        let projects = list(missing.iter().map(|k| k.0.to_string()).collect());
        let groups = list(missing.iter().map(|k| k.1.to_string()).collect());
        let mut stmt = self.conn.prepare(&self.ns.sql(&format!(
            "WITH i AS (
                SELECT DISTINCT project_id, group_hash FROM $S.issues
                WHERE project_id IN ({projects}) AND group_hash IN ({groups})
             ), a AS (
                SELECT project_id, group_hash, arg_max(status, changed_at) AS status FROM $S.issue_activity
                WHERE project_id IN ({projects}) AND group_hash IN ({groups}) GROUP BY ALL
             )
             SELECT project_id, group_hash, coalesce(a.status, 'unresolved')
             FROM i LEFT JOIN a USING (project_id, group_hash)"
        )))?;
        let rows =
            stmt.query_map([], |r| Ok((r.get::<_, i32>(0)? as u32, r.get::<_, u64>(1)?, r.get::<_, String>(2)?)))?;
        for row in rows {
            let (p, g, status) = row?;
            if missing.contains(&(p, g)) {
                self.caches.issues.insert((p, g), status.parse().unwrap_or_default());
            }
        }
        Ok(())
    }

    /// Move events of new groups over the limit into the project's overflow
    /// group. Returns how many new issues each project gets (without the
    /// overflow issue itself). Their rows exist only after the commit, so the
    /// count is added then.
    fn apply_issue_limit(&self, batch: &mut [EventWrite], hour: i64) -> HashMap<u32, u32> {
        let mut new_per_project: HashMap<u32, u32> = HashMap::new();
        let mut new: HashSet<(u32, u64)> = HashSet::new();
        let mut over: HashSet<(u32, u64)> = HashSet::new();
        for w in batch.iter_mut() {
            let key = (w.project_id, w.event.group_hash);
            if key.1 == OVERFLOW_GROUP || new.contains(&key) || self.caches.issues.contains_key(&key) {
                continue;
            }
            if !over.contains(&key) {
                let count = new_per_project.entry(key.0).or_default();
                if self.new_issues.get(key.0, hour) + *count < MAX_NEW_ISSUES_PER_HOUR {
                    *count += 1;
                    new.insert(key);
                    continue;
                }
                over.insert(key);
            }
            w.event.group_hash = OVERFLOW_GROUP;
            metrics::counter!("sdl_issue_overflow_events_total").increment(1);
        }
        if !over.is_empty() {
            let projects: HashSet<u32> = over.iter().map(|k| k.0).collect();
            tracing::warn!(?projects, groups = over.len(), "too many new issues this hour; events go to one issue");
        }
        new_per_project
    }

    fn try_write_events(&mut self, batch: &mut [EventWrite]) -> Result<()> {
        let hour = Utc::now().timestamp() / 3600;
        self.load_issues(batch.iter().map(|w| (w.project_id, w.event.group_hash)))?;
        let new_per_project = self.apply_issue_limit(batch, hour);
        self.load_issues(
            batch.iter().filter(|w| w.event.group_hash == OVERFLOW_GROUP).map(|w| (w.project_id, OVERFLOW_GROUP)),
        )?;
        let batch = &*batch;
        let ns = &self.ns;
        // Decide new issues and regressions against the cache, but only
        // apply cache changes after the commit succeeded. A new issue starts
        // at its earliest event (by event time) in the batch. Regressions are
        // logged at commit time: the activity log is ordered by write time.
        let mut new_issues: Vec<&EventWrite> = Vec::new();
        let mut activity: Vec<(u32, u64, IssueStatus, &'static str)> = Vec::new();
        // Group -> its index in `new_issues`, if it is new.
        let mut seen_in_batch: HashMap<(u32, u64), Option<usize>> = HashMap::new();
        for w in batch {
            let key = (w.project_id, w.event.group_hash);
            if let Some(slot) = seen_in_batch.get(&key) {
                if let Some(i) = *slot
                    && w.event.timestamp < new_issues[i].event.timestamp
                {
                    new_issues[i] = w;
                }
                continue;
            }
            let slot = match self.caches.issues.get(&key) {
                None => {
                    new_issues.push(w);
                    Some(new_issues.len() - 1)
                }
                Some(IssueStatus::Resolved) => {
                    activity.push((key.0, key.1, IssueStatus::Unresolved, "regression"));
                    None
                }
                Some(_) => None,
            };
            seen_in_batch.insert(key, slot);
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
                    // The overflow issue does not take after its first event.
                    let overflow = e.group_hash == OVERFLOW_GROUP;
                    stmt.execute(params![
                        w.project_id as i32,
                        e.group_hash,
                        e.timestamp.timestamp_micros(),
                        e.event_id.to_string(),
                        e.release,
                        if overflow { OVERFLOW_TITLE } else { e.title.as_str() },
                        if overflow { None } else { e.culprit.as_deref() },
                        if overflow { "warning" } else { e.level.as_str() },
                        e.platform,
                        if overflow { None } else { e.exception_type.as_deref() },
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

        for w in new_issues {
            self.caches.issues.insert((w.project_id, w.event.group_hash), IssueStatus::Unresolved);
        }
        for (p, g, s, _) in activity {
            self.caches.issues.insert((p, g), s);
        }
        for (project, n) in new_per_project {
            self.new_issues.add(project, hour, n);
        }
        Ok(())
    }

    fn create_project(&self, name: String, slug: String, platform: Option<String>) -> Result<ProjectRow, WriteError> {
        let ns = &self.ns;
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
        &self,
        project_id: u32,
        group_hash: u64,
        status: IssueStatus,
        reason: &str,
    ) -> Result<bool, WriteError> {
        let ns = &self.ns;
        self.load_issues([(project_id, group_hash)])?;
        match self.caches.issues.get(&(project_id, group_hash)) {
            None => return Err(WriteError::IssueNotFound),
            // Idempotent: the same status again writes nothing.
            Some(s) if s == status => return Ok(false),
            Some(_) => {}
        }
        self.in_tx(|c| {
            c.execute(
                &ns.sql("INSERT INTO $S.issue_activity VALUES (?, ?, make_timestamp(?), ?, ?)"),
                params![project_id as i32, group_hash, Utc::now().timestamp_micros(), status.as_str(), reason],
            )?;
            Ok(())
        })?;
        self.caches.issues.insert((project_id, group_hash), status);
        Ok(true)
    }

    fn add_bundle(&self, b: &NewBundle) -> Result<()> {
        let ns = &self.ns;
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

    fn retention(&self) -> Result<()> {
        if self.retention.is_zero() {
            return Ok(());
        }
        // Day-aligned cutoff: whole daily partitions are dropped without
        // writing delete files. Partial days round up (keep more).
        let days = self.retention.as_secs().div_ceil(86_400);
        let today = Utc::now().date_naive().and_hms_opt(0, 0, 0).expect("midnight").and_utc();
        let cutoff = (today - chrono::Duration::days(days as i64)).timestamp_micros();
        let sql = self.ns.sql("DELETE FROM $S.events WHERE received_at < make_timestamp(?)");
        let deleted = self.in_tx(|c| Ok(c.execute(&sql, params![cutoff])?))?;
        if deleted > 0 {
            tracing::info!(deleted, "retention removed old events");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_issue_count_is_per_project_and_hour() {
        let mut c = NewIssueCount::default();
        c.add(1, 100, 2);
        c.add(1, 100, 3);
        assert_eq!(c.get(1, 100), 5);
        assert_eq!(c.get(2, 100), 0);
        assert_eq!(c.get(1, 101), 0, "a new hour starts at zero");
        c.add(1, 101, 1);
        assert_eq!(c.get(1, 101), 1);
        assert_eq!(c.get(1, 100), 0, "only the current hour is kept");
    }
}
