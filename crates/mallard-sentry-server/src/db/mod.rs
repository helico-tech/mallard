//! DuckDB + DuckLake access.
//!
//! - One **writer thread** owns the primary connection. All writes go
//!   through it as commands on a queue, so there are never write conflicts
//!   inside the process, and events are committed in batches. Ingest stops
//!   at `queue_capacity` waiting commands ([`send_below`]).
//! - A **maintenance thread** flushes and compacts on its own connection,
//!   so that does not pause ingest (ADR 0014).
//! - A small **reader pool** of cloned connections serves the query API.
//!   DuckLake snapshots give readers a consistent view while the writer
//!   commits.

pub mod maintenance;
pub mod read;
pub mod schema;
pub mod writer;

use crate::config::Config;
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use crossbeam_channel::{Sender, TrySendError};
use maintenance::MaintCmd;
use mallard_duck::duckdb::{Connection, InterruptHandle};
use mallard_sentry_api::IssueStatus;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, oneshot};
use tokio_util::sync::CancellationToken;
pub use writer::{EventWrite, NewBundle, NewBundleFile, WriteCmd, WriteError};

#[derive(Debug, Clone)]
pub struct ProjectRow {
    pub id: u32,
    pub slug: String,
    pub name: String,
    pub platform: Option<String>,
    pub public_key: String,
    pub created_at: DateTime<Utc>,
}

/// Issues kept in memory. A miss asks the lake, so this bounds memory, not
/// correctness. About 150 bytes per issue.
const ISSUE_CACHE_CAPACITY: u64 = 200_000;

/// In-memory caches shared by the writer and request handlers.
pub struct Caches {
    pub projects: RwLock<HashMap<u32, ProjectRow>>,
    /// Current status per (project, group) of recently seen issues; only the
    /// writer thread changes it. An issue that is not here may still exist
    /// ([`writer::Writer::load_issues`]).
    pub issues: moka::sync::Cache<(u32, u64), IssueState>,
    /// What is stored and indexed per project.
    bundles: RwLock<HashMap<u32, ProjectBundles>>,
}

/// An issue as the writer needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssueState {
    pub status: IssueStatus,
    /// When that status was set, in microseconds. A resolved issue does not
    /// reopen from an event older than this.
    pub changed_us: i64,
}

impl IssueState {
    pub const NEW: IssueState = IssueState { status: IssueStatus::Unresolved, changed_us: 0 };
}

#[derive(Default)]
struct ProjectBundles {
    /// Bundle checksums.
    checksums: HashSet<String>,
    /// Releases with at least one bundle.
    releases: HashSet<String>,
    /// Debug ids of stored source maps.
    debug_ids: HashSet<uuid::Uuid>,
}

impl Default for Caches {
    fn default() -> Self {
        Self::new(Duration::ZERO)
    }
}

impl Caches {
    /// `retention` of the events: an issue without events for that long
    /// leaves the cache (0: only the capacity bounds it).
    pub fn new(retention: Duration) -> Self {
        let issues = moka::sync::Cache::builder().max_capacity(ISSUE_CACHE_CAPACITY);
        let issues = if retention.is_zero() { issues } else { issues.time_to_idle(retention) };
        Caches { projects: Default::default(), issues: issues.build(), bundles: Default::default() }
    }

    pub fn project(&self, id: u32) -> Option<ProjectRow> {
        self.projects.read().expect("lock").get(&id).cloned()
    }
    pub fn project_by_ref(&self, r: &str) -> Option<ProjectRow> {
        let p = self.projects.read().expect("lock");
        r.parse::<u32>().ok().and_then(|id| p.get(&id).cloned()).or_else(|| p.values().find(|p| p.slug == r).cloned())
    }
    fn with_bundles<T>(&self, project: u32, f: impl FnOnce(&ProjectBundles) -> T) -> Option<T> {
        self.bundles.read().expect("lock").get(&project).map(f)
    }
    pub fn bundle_known(&self, checksum: &str, projects: &[u32]) -> bool {
        projects.iter().all(|p| self.with_bundles(*p, |b| b.checksums.contains(checksum)).unwrap_or(false))
    }
    pub fn project_has_release(&self, project: u32, release: &str) -> bool {
        self.with_bundles(project, |b| b.releases.contains(release)).unwrap_or(false)
    }
    pub fn project_has_bundles(&self, project: u32) -> bool {
        self.with_bundles(project, |b| !b.checksums.is_empty()).unwrap_or(false)
    }
    /// Whether a source map with this debug id is stored for the project.
    /// Lets lookups for unknown (or made-up) ids skip the database.
    pub fn has_debug_id(&self, project: u32, id: uuid::Uuid) -> bool {
        self.with_bundles(project, |b| b.debug_ids.contains(&id)).unwrap_or(false)
    }
    /// Record a stored bundle (or one source map of it).
    pub(crate) fn add_bundle(
        &self,
        project: u32,
        checksum: &str,
        release: Option<&str>,
        debug_ids: impl IntoIterator<Item = uuid::Uuid>,
    ) {
        let mut all = self.bundles.write().expect("lock");
        let b = all.entry(project).or_default();
        if !b.checksums.contains(checksum) {
            b.checksums.insert(checksum.to_string());
        }
        if let Some(r) = release
            && !b.releases.contains(r)
        {
            b.releases.insert(r.to_string());
        }
        b.debug_ids.extend(debug_ids);
    }
    fn bundle_count(&self) -> usize {
        self.bundles.read().expect("lock").values().map(|b| b.checksums.len()).sum()
    }
    pub fn projects(&self) -> Vec<ProjectRow> {
        let mut v: Vec<ProjectRow> = self.projects.read().expect("lock").values().cloned().collect();
        v.sort_by_key(|p| p.id);
        v
    }
}

pub struct Db {
    tx: Sender<WriteCmd>,
    readers: Arc<Mutex<Vec<Connection>>>,
    permits: Arc<Semaphore>,
    pub caches: Arc<Caches>,
    pub ns: Ns,
    pub duckdb_version: String,
    pub catalog: String,
    pub queue_capacity: usize,
    writer_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    maintainer: Maintainer,
    /// Cancelled when the writer found the catalog invalidated.
    pub catalog_lost: CancellationToken,
    /// When the projects were last read because of a miss.
    project_refresh: Mutex<Option<Instant>>,
}

/// At most this often an unknown project reads the catalog. Ingest answers
/// anyone, and a made-up project id must not cost a query.
const PROJECT_REFRESH_GAP: Duration = Duration::from_secs(1);

/// The maintenance thread as [`Db`] sees it.
#[derive(Clone)]
struct Maintainer {
    tx: Sender<MaintCmd>,
    stopping: Arc<AtomicBool>,
    interrupt: Arc<InterruptHandle>,
    thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
}

impl Maintainer {
    /// Stop after the current step, interrupting a long merge. A merge rolls
    /// back when interrupted; nothing is lost. Waits for the thread.
    fn stop_and_join(&self) {
        self.stop();
        if let Some(h) = self.thread.lock().expect("lock").take() {
            let _ = h.join();
        }
    }

    fn stop(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.interrupt.interrupt();
        let _ = self.tx.try_send(MaintCmd::Stop);
    }
}

/// Queue `cmd` unless `limit` commands are waiting already.
///
/// Both queues are `crossbeam_channel::unbounded`, with the limit here. A bounded channel of
/// that crate keeps what is queued until its last `Sender` is gone, and [`Db`] holds one: a
/// command queued between a thread's last read and the end of that thread would stay there,
/// its `reply` neither answered nor dropped, and whoever waits for it would wait for ever
/// (issue 0158, and the server that never exited after an invalidated catalog). An unbounded
/// one drops what is queued when its `Receiver` is dropped, also when the thread panics, so a
/// `reply` always ends. A few callers at once can pass the limit by a few.
fn send_below<T>(tx: &Sender<T>, limit: usize, cmd: T) -> Result<(), TrySendError<T>> {
    if tx.len() >= limit {
        return Err(TrySendError::Full(cmd));
    }
    tx.try_send(cmd)
}

/// A job that is due once per interval.
struct Tick {
    interval: Duration,
    last: Instant,
}

impl Tick {
    fn new(interval: Duration) -> Self {
        Tick { interval, last: Instant::now() }
    }
    fn due_in(&self) -> Duration {
        self.interval.saturating_sub(self.last.elapsed()).max(Duration::from_millis(10))
    }
    fn is_due(&self) -> bool {
        self.last.elapsed() >= self.interval
    }
    fn done(&mut self) {
        self.last = Instant::now();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EnqueueError {
    #[error("write queue is full")]
    Full,
    #[error("writer stopped")]
    Stopped,
}

/// Table namespace: `$S` -> `<alias>.<schema>`, `$C` -> `'<alias>'`,
/// `$N` -> `'<schema>'`, `$T` -> this writer's in-memory staging database.
#[derive(Debug, Clone)]
pub struct Ns {
    pub alias: String,
    pub schema: String,
    pub staging: String,
}

impl Ns {
    pub fn new(alias: &str, schema: &str) -> Self {
        Ns { alias: alias.into(), schema: schema.into(), staging: format!("{schema}_staging") }
    }
    pub fn sql(&self, s: &str) -> String {
        s.replace("$S", &format!("{}.{}", self.alias, self.schema))
            .replace("$C", &format!("'{}'", self.alias))
            .replace("$N", &format!("'{}'", self.schema))
            .replace("$T", &self.staging)
    }
}

impl Db {
    /// `conn` has the DuckLake catalog attached as `lake` (and any secrets
    /// set). We only create our schema. `catalog_desc` is shown by `/v1/info`.
    pub fn with_connection(
        conn: Connection,
        cfg: &Config,
        settings: maintenance::Maintenance,
        commit_lock: mallard_duck::CommitLock,
        catalog_desc: String,
    ) -> Result<Db> {
        let ns = Ns::new(mallard_duck::CATALOG, crate::SCHEMA);
        let duckdb_version: String = conn.query_row("SELECT version()", [], |r| r.get(0))?;
        migrate(&conn, &ns)?;

        let caches = Arc::new(Caches::new(cfg.retention.events));
        writer::load_caches(&conn, &ns, &caches)?;

        let n = cfg.query.readers.max(1);
        let mut readers = Vec::with_capacity(n);
        for _ in 0..n {
            let r = conn.try_clone()?;
            // Default schema of the readers, for `POST /v1/sql` (`FROM events`).
            // Our own queries name `$S` in full.
            r.execute_batch(&ns.sql("USE $S"))?;
            readers.push(r);
        }
        let writer_conn = conn.try_clone()?;
        let maintenance_conn = conn.try_clone()?;
        drop(conn);

        // Unbounded: see `send_below`.
        let (tx, rx) = crossbeam_channel::unbounded();
        let (maint_tx, maint_rx) = crossbeam_channel::unbounded();
        let inlined = Arc::new(AtomicUsize::new(0));
        let catalog_lost = CancellationToken::new();
        let w = writer::Writer::new(
            writer_conn,
            ns.clone(),
            rx,
            caches.clone(),
            cfg,
            maint_tx.clone(),
            inlined.clone(),
            commit_lock,
            catalog_lost.clone(),
        )?;
        let handle = std::thread::Builder::new().name("ducklake-writer".into()).spawn(move || w.run())?;
        let stopping = Arc::new(AtomicBool::new(false));
        let interrupt = maintenance_conn.interrupt_handle();
        let m = maintenance::Maintainer::new(
            maintenance_conn,
            ns.clone(),
            maint_rx,
            tx.clone(),
            inlined,
            stopping.clone(),
            cfg,
            settings,
        );
        let maint_handle = std::thread::Builder::new().name("ducklake-maintenance".into()).spawn(move || m.run())?;

        Ok(Db {
            tx,
            readers: Arc::new(Mutex::new(readers)),
            permits: Arc::new(Semaphore::new(n)),
            caches,
            ns,
            duckdb_version,
            catalog: catalog_desc,
            queue_capacity: cfg.ingest.queue_capacity,
            writer_thread: Mutex::new(Some(handle)),
            maintainer: Maintainer {
                tx: maint_tx,
                stopping,
                interrupt,
                thread: Arc::new(Mutex::new(Some(maint_handle))),
            },
            catalog_lost,
            project_refresh: Mutex::new(None),
        })
    }

    /// The project with this id or slug. Not cached: another process may
    /// have created it, so read the projects again.
    pub async fn project_by_ref(&self, r: &str) -> Option<ProjectRow> {
        if let Some(p) = self.caches.project_by_ref(r) {
            return Some(p);
        }
        self.refresh_projects().await;
        self.caches.project_by_ref(r)
    }

    /// Read the projects from the catalog, unless that just happened.
    pub async fn refresh_projects(&self) {
        {
            let mut last = self.project_refresh.lock().expect("lock");
            if last.is_some_and(|t| t.elapsed() < PROJECT_REFRESH_GAP) {
                return;
            }
            *last = Some(Instant::now());
        }
        let caches = self.caches.clone();
        if let Err(e) = self.read(move |c, ns| writer::load_projects(c, ns, &caches)).await {
            tracing::warn!(error = format!("{e:#}"), "reading the projects failed");
        }
    }

    /// Read the projects from the catalog now, and say when that fails. `None`:
    /// not done in time.
    pub async fn load_projects(&self, timeout: Duration) -> Result<Option<()>> {
        let caches = self.caches.clone();
        self.read_for(timeout, move |c, ns| writer::load_projects(c, ns, &caches)).await
    }

    /// Stop maintenance, write what is queued, flush inlined data and stop
    /// the writer thread. Idempotent. Later writes fail with "writer stopped".
    pub async fn shutdown(&self) {
        // Before taking the writer's handle: if this future is dropped while
        // waiting here, `Drop` still has both threads to stop and join.
        let m = self.maintainer.clone();
        let _ = tokio::task::spawn_blocking(move || m.stop_and_join()).await;
        let Some(handle) = self.writer_thread.lock().expect("lock").take() else { return };
        let (tx, rx) = oneshot::channel();
        if self.tx.send(WriteCmd::Shutdown { reply: tx }).is_ok() {
            let _ = rx.await;
        }
        let _ = tokio::task::spawn_blocking(move || handle.join()).await;
    }

    /// False once the writer thread has stopped (crash or shutdown).
    pub fn writer_alive(&self) -> bool {
        self.writer_thread.lock().expect("lock").as_ref().is_some_and(|h| !h.is_finished())
    }

    pub fn queue_depth(&self) -> usize {
        self.tx.len()
    }

    /// Queue a command without waiting. Fails fast when the queue is full so
    /// ingest can answer 429 instead of piling up memory.
    pub fn try_send(&self, cmd: WriteCmd) -> Result<(), EnqueueError> {
        send_below(&self.tx, self.queue_capacity.max(1), cmd).map_err(|e| match e {
            TrySendError::Full(_) => EnqueueError::Full,
            TrySendError::Disconnected(_) => EnqueueError::Stopped,
        })
    }

    /// Send a command and wait for its reply (management operations).
    pub async fn call<T, E: From<anyhow::Error>>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, E>>) -> WriteCmd,
    ) -> Result<T, E> {
        let (tx, rx) = oneshot::channel();
        // Past the limit if need be: management calls do not fail on a full queue.
        if self.tx.send(make(tx)).is_err() {
            return Err(anyhow!("writer stopped").into());
        }
        rx.await.map_err(|_| anyhow!("writer stopped"))?
    }

    /// Run a read query on a pooled connection in a blocking task. The
    /// closure gets the namespace to expand `$S` in its SQL.
    pub async fn read<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection, &Ns) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let permit = self.permits.clone().acquire_owned().await?;
        let readers = self.readers.clone();
        let ns = self.ns.clone();
        tokio::task::spawn_blocking(move || {
            // The permit lives in the blocking task, not in this future: if
            // the caller is cancelled, the connection stays taken until the
            // query ends.
            let _permit = permit;
            let conn = readers.lock().expect("lock").pop();
            let conn = conn.ok_or_else(|| anyhow!("no free reader connection"))?;
            // One transaction per API call: all its queries see one snapshot
            // (and the catalog is read once, not per statement).
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                conn.execute_batch("BEGIN TRANSACTION")?;
                f(&conn, &ns)
            }));
            // Also after a panic, so the connection goes back clean.
            let _ = conn.execute_batch("ROLLBACK");
            readers.lock().expect("lock").push(conn);
            result.unwrap_or_else(|_| Err(anyhow!("query panicked")))
        })
        .await?
    }
}

/// Create the schema, tables and options. Several processes starting at once
/// race on these (DuckLake reports a conflict for the loser). All steps are
/// idempotent, so try again.
fn migrate(conn: &Connection, ns: &Ns) -> Result<()> {
    let once = || -> Result<()> {
        for stmt in schema::CREATE {
            conn.execute_batch(&ns.sql(stmt)).with_context(|| format!("creating schema: {stmt}"))?;
        }
        for stmt in schema::table_options() {
            conn.execute_batch(&ns.sql(&stmt)).with_context(|| format!("setting table option: {stmt}"))?;
        }
        writer::record_schema_version(conn, ns)?;
        writer::ensure_meta_rows(conn, ns)
    };
    let mut attempt = 0;
    loop {
        match once() {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 10 && mallard_duck::is_conflict(&e) => {
                attempt += 1;
                tracing::warn!(attempt, error = format!("{e:#}"), "creating the schema conflicted; trying again");
                std::thread::sleep(Duration::from_millis(100 * attempt));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Where a [`Db::read_for`] query is, for interrupting it.
enum Run {
    Waiting,
    Running(Arc<InterruptHandle>),
    Done,
    /// Given up before it started: it must not start.
    Cancelled,
}

/// Interrupts the query of a [`Db::read_for`] when the request ends first
/// (timeout or a dropped request). DuckDB only stops the running statement,
/// so it keeps interrupting until the closure is done; `Done` is set under
/// the same lock before the connection goes back, so an interrupt never
/// reaches the next query on it.
struct StopOnDrop(Arc<Mutex<Run>>);

/// A panic in a query closure poisons the lock; the state is still valid.
fn lock(m: &Mutex<Run>) -> std::sync::MutexGuard<'_, Run> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let mut run = lock(&self.0);
        match &*run {
            Run::Waiting => *run = Run::Cancelled,
            Run::Running(_) => {
                drop(run);
                let state = self.0.clone();
                tokio::spawn(async move {
                    loop {
                        match &*lock(&state) {
                            Run::Running(h) => h.interrupt(),
                            _ => return,
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                });
            }
            Run::Done | Run::Cancelled => {}
        }
    }
}

impl Db {
    /// [`Db::read`] that gives up after `timeout` (`Ok(None)`): the query is
    /// interrupted, and its connection returns to the pool once it stopped.
    pub async fn read_for<T: Send + 'static>(
        &self,
        timeout: std::time::Duration,
        f: impl FnOnce(&Connection, &Ns) -> Result<T> + Send + 'static,
    ) -> Result<Option<T>> {
        let state = Arc::new(Mutex::new(Run::Waiting));
        let s = state.clone();
        let read = self.read(move |c, ns| {
            {
                let mut run = lock(&s);
                if matches!(*run, Run::Cancelled) {
                    return Err(anyhow!("query cancelled"));
                }
                *run = Run::Running(c.interrupt_handle());
            }
            // `Done` also when `f` panics.
            struct SetDone(Arc<Mutex<Run>>);
            impl Drop for SetDone {
                fn drop(&mut self) {
                    *lock(&self.0) = Run::Done;
                }
            }
            let _done = SetDone(s.clone());
            f(c, ns)
        });
        let _stop = StopOnDrop(state);
        match tokio::time::timeout(timeout, read).await {
            Ok(r) => r.map(Some),
            Err(_) => Ok(None),
        }
    }
}

impl Drop for Db {
    /// Call `shutdown().await` before dropping: it does not block an async
    /// thread. Without it (a dropped server future, a failed start) stop both
    /// threads here and wait: the writer writes what is queued and flushes.
    /// Neither must outlive us detached: when the process then exits, C++ static
    /// destructors run under a thread that is still inside DuckDB (SIGSEGV,
    /// issue 0018). Like the otel side's `Flusher`.
    fn drop(&mut self) {
        // Maintenance first: it may be waiting for the writer (retention), and
        // the writer's final flush must not overlap it.
        self.maintainer.stop_and_join();
        let handle = self.writer_thread.get_mut().unwrap_or_else(|p| p.into_inner()).take();
        let Some(handle) = handle else { return };
        tracing::warn!("Db dropped without shutdown(); waiting for the writer");
        let (reply, _) = oneshot::channel();
        // Fails only when the writer is gone already.
        let _ = self.tx.send(WriteCmd::Shutdown { reply });
        let _ = handle.join();
    }
}

pub fn us_to_dt(us: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(us).unwrap_or_else(|| {
        tracing::warn!(us, "stored timestamp is out of range");
        DateTime::default()
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn bundle_caches_per_project() {
        let c = Caches::default();
        let id = uuid::Uuid::new_v4();
        assert!(!c.project_has_bundles(1));
        c.add_bundle(1, "abc", Some("1.0"), [id]);
        c.add_bundle(2, "abc", None, []);
        assert!(c.project_has_bundles(1) && c.project_has_bundles(2) && !c.project_has_bundles(3));
        assert!(c.bundle_known("abc", &[1, 2]) && !c.bundle_known("abc", &[1, 3]));
        assert!(c.project_has_release(1, "1.0") && !c.project_has_release(2, "1.0"));
        assert!(c.has_debug_id(1, id) && !c.has_debug_id(2, id) && !c.has_debug_id(1, uuid::Uuid::new_v4()));
        assert_eq!(c.bundle_count(), 2);
    }

    /// Sentry issue 0002: the issue cache has a size limit, and an issue without
    /// events for the retention period leaves it.
    #[test]
    fn issue_cache_is_bounded() {
        let c = Caches::default();
        for g in 0..ISSUE_CACHE_CAPACITY + 1000 {
            c.issues.insert((1, g), IssueState::NEW);
        }
        c.issues.run_pending_tasks();
        assert!(c.issues.entry_count() <= ISSUE_CACHE_CAPACITY);

        let c = Caches::new(Duration::from_millis(20));
        c.issues.insert((1, 1), IssueState::NEW);
        assert!(c.issues.get(&(1, 1)).is_some());
        std::thread::sleep(Duration::from_millis(100));
        assert!(c.issues.get(&(1, 1)).is_none());
    }

    /// A Db on a local lake in `dir`.
    pub(crate) fn open_db(dir: &std::path::Path, readers: usize) -> Db {
        open_db_on(&dir.join("catalog.ducklake").display().to_string(), dir, readers, |_| {})
    }

    /// A Db on `catalog`, as one more process would open it (its own DuckDB).
    fn open_db_on(catalog: &str, dir: &std::path::Path, readers: usize, change: impl FnOnce(&mut Config)) -> Db {
        let mut cfg = Config::default();
        cfg.query.readers = readers;
        cfg.ingest.batch_window = Duration::from_millis(10);
        change(&mut cfg);
        let st = mallard_duck::StorageConfig {
            catalog: catalog.to_string(),
            data_path: dir.join("lake").display().to_string(),
            ..Default::default()
        };
        let conn = mallard_duck::open_connection(&st, &Default::default()).unwrap();
        let settings = maintenance::Maintenance { catalog_wide: Some(Duration::from_secs(3600)) };
        Db::with_connection(conn, &cfg, settings, Default::default(), "test".into()).unwrap()
    }

    /// Dropping without `shutdown()` stops the writer and waits for it
    /// (issue 0018): the writer's reference to the caches is gone by then.
    #[tokio::test]
    async fn drop_stops_the_writer_and_waits() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 1);
        let caches = db.caches.clone();
        assert!(Arc::strong_count(&caches) > 2, "Db and the writer hold it too");
        drop(db);
        assert_eq!(Arc::strong_count(&caches), 1);
    }

    /// Dropping without `shutdown()` stops the maintenance thread too and
    /// waits for it (issue 0018), also in the middle of a run.
    #[tokio::test(flavor = "multi_thread")]
    async fn drop_stops_maintenance_and_waits() {
        let dir = tempfile::tempdir().unwrap();
        for running in [false, true] {
            let db = open_db(dir.path(), 1);
            let stopping = db.maintainer.stopping.clone();
            assert_eq!(Arc::strong_count(&stopping), 3, "the thread holds it too");
            if running {
                queue_event(&db, "x", Utc::now()).await.unwrap().unwrap();
                let (reply, _rx) = oneshot::channel();
                db.try_send(WriteCmd::Maintenance { reply }).unwrap();
            }
            drop(db);
            assert_eq!(Arc::strong_count(&stopping), 1, "the thread ended");
        }
    }

    /// `mallard maintenance` exits when `Server::run_maintenance`
    /// answers. So by then flush, retention (writer thread) and compaction
    /// (maintenance thread) are done.
    #[tokio::test(flavor = "multi_thread")]
    async fn maintenance_answers_after_all_of_its_work() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 2);
        let now = Utc::now();
        // Files: one past retention, two to merge. Then one inlined row to flush.
        let conn = db.readers.lock().unwrap().pop().unwrap();
        for at in [now - chrono::Duration::days(200), now, now] {
            queue_event(&db, "x", at).await.unwrap().unwrap();
            maintenance::flush_inlined(&conn, &db.ns).unwrap();
        }
        db.readers.lock().unwrap().push(conn);
        queue_event(&db, "x", now).await.unwrap().unwrap();
        let files = async || -> i64 {
            db.read(|c, ns| {
                Ok(c.query_row(
                    &ns.sql("SELECT count(*) FROM ducklake_list_files($C, 'events', schema => $N)"),
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap()
        };
        assert_eq!(files().await, 3, "the last row is inlined");

        let r: Result<()> = db.call(|reply| WriteCmd::Maintenance { reply }).await;
        r.unwrap();
        assert_eq!(count_events(&db).await, 3, "retention removed the old day");
        assert_eq!(files().await, 1, "flushed and merged");
        db.shutdown().await;
    }

    /// A run cut short by a shutdown must not answer "done".
    #[tokio::test(flavor = "multi_thread")]
    async fn stopped_maintenance_does_not_report_success() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 2);
        let conn = db.readers.lock().unwrap().pop().unwrap();
        let (_tx, rx) = crossbeam_channel::bounded(1);
        let mut m = maintenance::Maintainer::new(
            conn,
            db.ns.clone(),
            rx,
            db.tx.clone(),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(true)),
            &Config::default(),
            maintenance::Maintenance { catalog_wide: None },
        );
        let e = tokio::task::spawn_blocking(move || m.run_now()).await.unwrap().unwrap_err();
        assert!(format!("{e:#}").contains("stopped"), "{e:#}");
        db.shutdown().await;
    }

    async fn select_one(db: &Db) -> Result<i32> {
        db.read(|c, _| Ok(c.query_row("SELECT 1", [], |r| r.get(0))?)).await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_read_keeps_the_pool_usable() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(open_db(dir.path(), 1));
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (started_tx, started_rx) = oneshot::channel();
        let slow = db.read(move |_, _| {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
            Ok(())
        });
        // Drop the read future while its query still runs.
        tokio::select! {
            _ = slow => panic!("the slow read finished early"),
            _ = started_rx => {}
        }
        let db2 = db.clone();
        let next = tokio::spawn(async move { select_one(&db2).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        release_tx.send(()).unwrap();
        assert_eq!(next.await.unwrap().unwrap(), 1);
        for _ in 0..3 {
            assert_eq!(select_one(&db).await.unwrap(), 1);
        }
        db.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn event_pages_break_ties_on_received_at() {
        use mallard_sentry_protocol::{NormalizeContext, ProcessedEvent, normalize_event};
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 1);
        // Five events with the same received_at.
        let received_at = Utc::now();
        let ctx = NormalizeContext::new(received_at);
        let mut replies = Vec::new();
        for _ in 0..5 {
            let ev = normalize_event(serde_json::json!({"message": "tie"}), &ctx);
            let (tx, rx) = oneshot::channel();
            let event = ProcessedEvent::from_normalized(&ev, received_at);
            let w = EventWrite { project_id: 1, event, payload_json: ev.to_string(), reply: tx };
            db.try_send(WriteCmd::Event(Box::new(w))).unwrap();
            replies.push(rx);
        }
        for rx in replies {
            rx.await.unwrap().unwrap();
        }
        let mut seen = HashSet::new();
        let mut before = None;
        for _ in 0..5 {
            let q = mallard_sentry_api::EventQuery { limit: Some(2), ..Default::default() };
            let page = db.read(move |c, ns| read::list_events(c, ns, 1, None, &q, &[], None, before)).await.unwrap();
            for e in &page.events {
                assert!(seen.insert(e.event_id.clone()), "event {} on two pages", e.event_id);
            }
            match page.next_cursor {
                Some(c) => before = Some(read::Cursor::parse(&c).unwrap()),
                None => break,
            }
        }
        assert_eq!(seen.len(), 5);
        db.shutdown().await;
    }

    /// Issue 0003: first/last seen, trends and stats follow the event's
    /// `timestamp` (clamped at ingest), not the receive time.
    #[tokio::test(flavor = "multi_thread")]
    async fn issues_and_stats_use_event_time() {
        use mallard_sentry_protocol::{NormalizeContext, ProcessedEvent, normalize_event};
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 1);
        let now = Utc::now();
        let ctx = NormalizeContext::new(now);
        let ms = |t: DateTime<Utc>| t.timestamp_millis();
        // Whole seconds, so the stored value equals `past` exactly.
        let past = DateTime::from_timestamp(now.timestamp() - 50 * 60, 0).unwrap();
        let future = now + chrono::Duration::days(1);
        let mut replies = Vec::new();
        // Two "past" events, the later one sent first: the issue starts at
        // the earliest. One event far in the future: clamped to now.
        for (msg, ts) in [("past", past + chrono::Duration::minutes(10)), ("past", past), ("future", future)] {
            let ev = normalize_event(serde_json::json!({"message": msg, "timestamp": ts.timestamp() as f64}), &ctx);
            let (tx, rx) = oneshot::channel();
            let event = ProcessedEvent::from_normalized(&ev, now);
            let w = EventWrite { project_id: 1, event, payload_json: ev.to_string(), reply: tx };
            db.try_send(WriteCmd::Event(Box::new(w))).unwrap();
            replies.push(rx);
        }
        for rx in replies {
            rx.await.unwrap().unwrap();
        }
        // 2h window, 5 min trend buckets.
        let w = read::Window { from: now - chrono::Duration::hours(2), to: now + chrono::Duration::seconds(1) };
        let list = db
            .read(move |c, ns| {
                let q = mallard_sentry_api::IssueQuery::default();
                read::list_issues(c, ns, 1, &q, &[], &w, read::StatusFilter::All, read::IssueSort::LastSeen, None)
            })
            .await
            .unwrap();
        let by_title = |t: &str| list.issues.iter().find(|i| i.title == t).unwrap().clone();
        let p = by_title("past");
        assert_eq!(p.count, 2);
        assert_eq!(p.first_seen, past);
        assert_eq!(p.last_seen, past + chrono::Duration::minutes(10));
        // About 70 and 80 minutes into the window: buckets 13/14 and 15/16
        // of 24, not the last one (receive time).
        let span = (w.to - w.from).num_microseconds().unwrap();
        let idx = |t: DateTime<Utc>| ((t - w.from).num_microseconds().unwrap() / (span / 24)) as usize;
        let mut want = vec![0u64; 24];
        want[idx(past)] += 1;
        want[idx(past + chrono::Duration::minutes(10))] += 1;
        assert_eq!(p.trend, want);
        assert_eq!(p.trend[23], 0);
        let f = by_title("future");
        assert_eq!(ms(f.first_seen), ms(now));
        assert_eq!(ms(f.last_seen), ms(now));
        assert_eq!(f.trend[23], 1);

        let stats = db.read(move |c, ns| read::project_stats(c, ns, 1, &w, None)).await.unwrap();
        assert_eq!(stats.total_events, 3);
        assert_eq!(stats.new_issues, 2);
        let in_bucket = |t: DateTime<Utc>| {
            stats.buckets.iter().rposition(|b| b.start <= t).map(|i| stats.buckets[i].events).unwrap_or(0)
        };
        assert_eq!(in_bucket(past), 1);
        assert_eq!(in_bucket(past + chrono::Duration::minutes(10)), 1);
        assert_eq!(in_bucket(now), 1);
        assert_eq!(stats.buckets.iter().map(|b| b.events).sum::<u64>(), 3);
        db.shutdown().await;
    }

    /// Queue one event with its own message (= its own issue) at `received_at`.
    fn queue_event(
        db: &Db,
        message: &str,
        received_at: DateTime<Utc>,
    ) -> oneshot::Receiver<Result<(), Arc<anyhow::Error>>> {
        use mallard_sentry_protocol::{NormalizeContext, ProcessedEvent, normalize_event};
        let ctx = NormalizeContext::new(received_at);
        let ev = normalize_event(serde_json::json!({"message": message}), &ctx);
        let (tx, rx) = oneshot::channel();
        let event = ProcessedEvent::from_normalized(&ev, received_at);
        let w = EventWrite { project_id: 1, event, payload_json: ev.to_string(), reply: tx };
        db.try_send(WriteCmd::Event(Box::new(w))).unwrap();
        rx
    }

    async fn count_events(db: &Db) -> i64 {
        db.read(|c, ns| Ok(c.query_row(&ns.sql("SELECT count(*) FROM $S.events"), [], |r| r.get(0))?)).await.unwrap()
    }

    /// Issue 0132, with a real allocation error. Memory that comes back within the retries
    /// costs no event; memory that stays short fails the batch once the retries run out.
    #[tokio::test(flavor = "multi_thread")]
    async fn short_memory_does_not_fail_a_batch() {
        let dir = tempfile::tempdir().unwrap();
        let st = mallard_duck::StorageConfig {
            catalog: dir.path().join("catalog.ducklake").display().to_string(),
            data_path: dir.path().join("lake").display().to_string(),
            ..Default::default()
        };
        let conn = mallard_duck::open_connection(&st, &Default::default()).unwrap();
        let limits = conn.try_clone().unwrap();
        let mut cfg = Config::default();
        cfg.ingest.batch_window = Duration::from_millis(10);
        let settings = maintenance::Maintenance { catalog_wide: None };
        let db = Db::with_connection(conn, &cfg, settings, Default::default(), "test".into()).unwrap();
        // The lowest limit DuckDB takes: about what it uses right now.
        let short = || {
            [4, 8, 16, 32, 64, 128]
                .into_iter()
                .find(|mb| limits.execute_batch(&format!("SET memory_limit = '{mb}MB'")).is_ok())
                .expect("a limit near what is in use");
        };
        let enough = || limits.execute_batch("SET memory_limit = '1GB'").unwrap();

        short();
        let reply = queue_event(&db, "comes back", Utc::now());
        tokio::time::sleep(Duration::from_millis(100)).await;
        enough();
        reply.await.unwrap().unwrap();
        assert_eq!(count_events(&db).await, 1);

        short();
        let t0 = Instant::now();
        let failed = queue_event(&db, "stays short", Utc::now()).await.unwrap();
        let waited = t0.elapsed();
        enough();
        assert!(mallard_duck::is_out_of_memory(&failed.unwrap_err()));
        assert!(waited >= Duration::from_secs(2), "tried again for about 2.5 s, not {waited:?}");
        queue_event(&db, "later", Utc::now()).await.unwrap().unwrap();
        assert_eq!(count_events(&db).await, 2);
        db.shutdown().await;
    }

    /// Issue 0130, with DuckDB's own invalidation, as a failed checkpoint of the catalog causes
    /// it (not through an out-of-memory error): the writer says so, without waiting for retries.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_writer_notices_an_invalidated_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let st = mallard_duck::StorageConfig {
            catalog: dir.path().join("catalog.ducklake").display().to_string(),
            data_path: dir.path().join("lake").display().to_string(),
            ..Default::default()
        };
        let conn = mallard_duck::open_connection(&st, &Default::default()).unwrap();
        let other = conn.try_clone().unwrap();
        let settings = maintenance::Maintenance { catalog_wide: None };
        let db = Db::with_connection(conn, &Config::default(), settings, Default::default(), "test".into()).unwrap();
        assert!(!db.catalog_lost.is_cancelled());
        other.execute_batch("SET debug_checkpoint_abort = 'before_header_non_fatal'").unwrap();
        other.execute_batch("CHECKPOINT __ducklake_metadata_lake").unwrap_err();

        let t0 = Instant::now();
        let failed = queue_event(&db, "not stored", Utc::now()).await.unwrap();
        assert!(mallard_duck::is_invalidated(&failed.unwrap_err()));
        assert!(t0.elapsed() < Duration::from_secs(2), "no retries: {:?}", t0.elapsed());
        assert!(db.catalog_lost.is_cancelled());
        db.shutdown().await;
    }

    /// With nothing to write, the regular read of what other processes wrote notices it.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_idle_writer_notices_an_invalidated_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let st = mallard_duck::StorageConfig {
            catalog: dir.path().join("catalog.ducklake").display().to_string(),
            data_path: dir.path().join("lake").display().to_string(),
            ..Default::default()
        };
        let conn = mallard_duck::open_connection(&st, &Default::default()).unwrap();
        let other = conn.try_clone().unwrap();
        let settings = maintenance::Maintenance { catalog_wide: None };
        let db = Db::with_connection(conn, &Config::default(), settings, Default::default(), "test".into()).unwrap();
        other.execute_batch("SET debug_checkpoint_abort = 'before_header_non_fatal'").unwrap();
        other.execute_batch("CHECKPOINT __ducklake_metadata_lake").unwrap_err();
        // `SYNC_INTERVAL` is 100 ms in tests.
        tokio::time::timeout(Duration::from_secs(30), db.catalog_lost.cancelled()).await.unwrap();
        db.shutdown().await;
    }

    /// Issue 0170: a command queued after the writer's last read, during its final flush, is never
    /// read. Its `reply` has to end all the same, or whoever waits for it never stops, and the
    /// process never exits.
    #[tokio::test(flavor = "multi_thread")]
    async fn what_is_queued_during_the_stop_gets_an_answer() {
        use mallard_sentry_protocol::{NormalizeContext, ProcessedEvent, normalize_event};
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(open_db(dir.path(), 1));
        let stop = {
            let db = db.clone();
            tokio::spawn(async move { db.shutdown().await })
        };
        let mut replies = Vec::new();
        loop {
            let at = Utc::now();
            let ev = normalize_event(serde_json::json!({"message": "late"}), &NormalizeContext::new(at));
            let (reply, rx) = oneshot::channel();
            let event = ProcessedEvent::from_normalized(&ev, at);
            let w = EventWrite { project_id: 1, event, payload_json: ev.to_string(), reply };
            match db.try_send(WriteCmd::Event(Box::new(w))) {
                Ok(()) => replies.push(rx),
                Err(EnqueueError::Stopped) => break,
                Err(EnqueueError::Full) => tokio::time::sleep(Duration::from_millis(1)).await,
            }
        }
        let all_ended = async {
            for rx in replies {
                let _ = rx.await;
            }
        };
        tokio::time::timeout(Duration::from_secs(20), all_ended).await.expect("a reply that never ends");
        stop.await.unwrap();
    }

    /// Issue 0144: events commit one at a time with the other side of the process.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_batch_waits_for_a_commit_of_the_other_side() {
        let dir = tempfile::tempdir().unwrap();
        let st = mallard_duck::StorageConfig {
            catalog: dir.path().join("catalog.ducklake").display().to_string(),
            data_path: format!("{}/lake/", dir.path().display()),
            ..Default::default()
        };
        let conn = mallard_duck::open_connection(&st, &Default::default()).unwrap();
        let mut cfg = Config::default();
        cfg.ingest.batch_window = Duration::from_millis(10);
        let settings = maintenance::Maintenance { catalog_wide: None };
        let lock = mallard_duck::CommitLock::default();
        let db = Db::with_connection(conn, &cfg, settings, lock.clone(), "test".into()).unwrap();
        let other_side = lock.hold();
        let mut rx = queue_event(&db, "waits", Utc::now());
        std::thread::sleep(Duration::from_millis(300));
        assert!(rx.try_recv().is_err(), "committed during a commit of the other side");
        drop(other_side);
        rx.await.unwrap().unwrap();
        db.shutdown().await;
    }

    /// Sentry issue 0003: compaction and flushes run next to the writer. Events
    /// are committed while they run, none get lost, and retention (on the
    /// writer thread) removes old days without failing a batch.
    #[tokio::test(flavor = "multi_thread")]
    async fn ingest_continues_during_maintenance() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(open_db(dir.path(), 1));
        // Data to work on: batches above the inline limit become Parquet
        // files, one of them older than the retention of 90 days.
        for i in 0..8 {
            let at = if i == 0 { Utc::now() - chrono::Duration::days(200) } else { Utc::now() };
            let replies: Vec<_> = (0..schema::EVENTS_INLINE_ROWS + 100).map(|_| queue_event(&db, "old", at)).collect();
            for rx in replies {
                rx.await.unwrap().unwrap();
            }
        }
        let preloaded = count_events(&db).await;

        let done = Arc::new(AtomicBool::new(false));
        let committed = Arc::new(AtomicUsize::new(0));
        let ingest = {
            let (db, done, committed) = (db.clone(), done.clone(), committed.clone());
            tokio::spawn(async move {
                let mut slowest = Duration::ZERO;
                while !done.load(Ordering::Relaxed) {
                    let started = Instant::now();
                    queue_event(&db, "new", Utc::now()).await.unwrap().unwrap();
                    slowest = slowest.max(started.elapsed());
                    committed.fetch_add(1, Ordering::Relaxed);
                }
                slowest
            })
        };
        let mut during_maintenance = 0;
        for _ in 0..3 {
            // Inlined rows for the flush to pick up.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let before = committed.load(Ordering::Relaxed);
            let r: Result<()> = db.call(|reply| WriteCmd::Maintenance { reply }).await;
            r.unwrap();
            during_maintenance += committed.load(Ordering::Relaxed) - before;
        }
        done.store(true, Ordering::Relaxed);
        let slowest = ingest.await.unwrap();

        assert!(during_maintenance > 0, "no event was committed while maintenance ran");
        assert!(slowest < Duration::from_secs(5), "ingest paused for {slowest:?}");
        // Everything acknowledged is stored; the old batch is gone.
        let old_batch = (schema::EVENTS_INLINE_ROWS + 100) as i64;
        assert_eq!(count_events(&db).await, preloaded - old_batch + committed.load(Ordering::Relaxed) as i64);
        db.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_stops_a_running_maintenance() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 1);
        queue_event(&db, "x", Utc::now()).await.unwrap().unwrap();
        // Still answered: maintenance stopped between steps or never started.
        let (reply, rx) = oneshot::channel();
        db.try_send(WriteCmd::Maintenance { reply }).unwrap();
        db.shutdown().await;
        let _ = rx.await;
        assert!(!db.writer_alive());
        // Idempotent, also for the maintenance thread.
        db.shutdown().await;
    }

    /// Queue a raw event body for `project` and wait until it is stored.
    async fn store(db: &Db, project: u32, body: serde_json::Value, received_at: DateTime<Utc>) {
        use mallard_sentry_protocol::{NormalizeContext, ProcessedEvent, normalize_event};
        let ev = normalize_event(body, &NormalizeContext::new(received_at));
        let (tx, rx) = oneshot::channel();
        let event = ProcessedEvent::from_normalized(&ev, received_at);
        let w = EventWrite { project_id: project, event, payload_json: ev.to_string(), reply: tx };
        db.try_send(WriteCmd::Event(Box::new(w))).unwrap();
        rx.await.unwrap().unwrap();
    }

    fn issue_window() -> read::Window {
        read::Window { from: Utc::now() - chrono::Duration::hours(1), to: Utc::now() + chrono::Duration::minutes(1) }
    }

    async fn issues_of(db: &Db, project: u32) -> Vec<mallard_sentry_api::IssueSummary> {
        let w = issue_window();
        db.read(move |c, ns| {
            let q = mallard_sentry_api::IssueQuery { limit: Some(500), ..Default::default() };
            read::list_issues(c, ns, project, &q, &[], &w, read::StatusFilter::All, read::IssueSort::Count, None)
        })
        .await
        .unwrap()
        .issues
    }

    async fn count_issue_rows(db: &Db, project: u32) -> i64 {
        db.read(move |c, ns| {
            Ok(c.query_row(&ns.sql("SELECT count(*) FROM $S.issues WHERE project_id = ?"), [project as i32], |r| {
                r.get(0)
            })?)
        })
        .await
        .unwrap()
    }

    /// Sentry issue 0002: after the limit, new groups of that project share one
    /// issue. Existing groups and other projects go on as usual.
    #[tokio::test(flavor = "multi_thread")]
    async fn new_issues_over_the_limit_share_one_issue() {
        use mallard_sentry_protocol::{NormalizeContext, ProcessedEvent, normalize_event};
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 1);
        let now = Utc::now();
        let ctx = NormalizeContext::new(now);
        let extra = 20;
        // One batch over the limit; two events for a group over it.
        let mut replies = Vec::new();
        for i in 0..writer::MAX_NEW_ISSUES_PER_HOUR as usize + extra {
            let ev = normalize_event(
                serde_json::json!({"message": format!("m{i}"), "fingerprint": [format!("group-{i}")]}),
                &ctx,
            );
            let (tx, rx) = oneshot::channel();
            let event = ProcessedEvent::from_normalized(&ev, now);
            let w = EventWrite { project_id: 1, event, payload_json: ev.to_string(), reply: tx };
            db.try_send(WriteCmd::Event(Box::new(w))).unwrap();
            replies.push(rx);
        }
        for rx in replies {
            rx.await.unwrap().unwrap();
        }
        let group = |name: &str| serde_json::json!({"message": name, "fingerprint": [name]});
        store(&db, 1, group("group-0"), now).await; // known: stays its own issue
        store(&db, 1, group("group-new-after-the-limit"), now).await;
        store(&db, 2, group("other-project"), now).await;

        let issues = issues_of(&db, 1).await;
        // The limit plus the overflow issue.
        assert_eq!(count_issue_rows(&db, 1).await, writer::MAX_NEW_ISSUES_PER_HOUR as i64 + 1);
        let top = &issues[0];
        assert_eq!(top.id, read::group_id(schema::OVERFLOW_GROUP));
        assert_eq!(top.title, schema::OVERFLOW_TITLE, "not the title of its latest event");
        assert_eq!(top.count, extra as u64 + 1);
        assert_eq!(issues[1].count, 2, "a known group is not affected by the limit");
        assert_eq!(issues_of(&db, 2).await.len(), 1);
        db.shutdown().await;
    }

    /// Sentry issue 0002: an issue that left the cache is found in the lake: no
    /// second row, and a resolved one reopens.
    #[tokio::test(flavor = "multi_thread")]
    async fn issue_outside_the_cache_is_found_in_the_lake() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 1);
        let now = Utc::now();
        let body = serde_json::json!({"message": "m", "fingerprint": ["g"]});
        store(&db, 1, body.clone(), now).await;
        let id = issues_of(&db, 1).await[0].id.clone();
        let group_hash = read::parse_group_id(&id).unwrap();

        db.caches.issues.invalidate_all();
        let set = |status| {
            db.call(move |reply| WriteCmd::SetStatus {
                project_id: 1,
                group_hash,
                status,
                reason: "user".into(),
                reply,
            })
        };
        assert!(set(IssueStatus::Resolved).await.unwrap(), "known although not cached");
        db.caches.issues.invalidate_all();
        assert!(!set(IssueStatus::Resolved).await.unwrap(), "already resolved");

        db.caches.issues.invalidate_all();
        store(&db, 1, body, Utc::now()).await;
        assert_eq!(count_issue_rows(&db, 1).await, 1);
        let issue = &issues_of(&db, 1).await[0];
        assert_eq!((issue.count, issue.status), (2, IssueStatus::Unresolved));

        let missing = db
            .call(move |reply| WriteCmd::SetStatus {
                project_id: 1,
                group_hash: group_hash ^ 1,
                status: IssueStatus::Resolved,
                reason: "user".into(),
                reply,
            })
            .await;
        assert!(matches!(missing, Err(WriteError::IssueNotFound)));
        db.shutdown().await;
    }

    async fn create(db: &Db, slug: &str) -> Result<ProjectRow, WriteError> {
        let (name, slug) = (slug.to_string(), slug.to_string());
        db.call(move |reply| WriteCmd::CreateProject { name, slug, platform: None, reply }).await
    }

    /// Wait until `f` holds, for what another process wrote to reach us.
    async fn eventually(what: &str, mut f: impl AsyncFnMut() -> bool) {
        for _ in 0..200 {
            if f().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("never happened: {what}");
    }

    /// Sentry issue 0001: two processes on one catalog. Run on SQLite, which needs
    /// no server; on Postgres too by [`two_processes_on_postgres`].
    async fn two_processes(catalog: &str, dir: &std::path::Path) {
        let a = Arc::new(open_db_on(catalog, dir, 2, |_| {}));
        let b = Arc::new(open_db_on(catalog, dir, 2, |_| {}));

        // A project of one process is known to the other at once, by id and
        // by slug, for ingest and API. Ids do not collide.
        let p1 = create(&a, "p1").await.unwrap();
        assert_eq!(p1.id, 1);
        tokio::time::sleep(PROJECT_REFRESH_GAP).await;
        assert_eq!(b.project_by_ref("1").await.unwrap().public_key, p1.public_key);
        assert_eq!(create(&b, "p2").await.unwrap().id, 2);
        eventually("a learns of p2", async || a.caches.project_by_ref("p2").is_some()).await;

        // Creating at once: no id twice; one slug is one project.
        let mut tasks = Vec::new();
        for i in 0..6 {
            for db in [a.clone(), b.clone()] {
                let db2 = db.clone();
                tasks.push(tokio::spawn(async move { create(&db, &format!("same-{i}")).await }));
                let slug = format!("own-{i}-{}", tasks.len());
                tasks.push(tokio::spawn(async move { create(&db2, &slug).await }));
            }
        }
        let mut created = 0;
        for t in tasks {
            match t.await.unwrap() {
                Ok(_) => created += 1,
                Err(WriteError::SlugTaken(_)) => {}
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(created, 12 + 6);
        let ids: (i64, i64, i64) = a
            .read(|c, ns| {
                Ok(c.query_row(
                    &ns.sql("SELECT count(*), count(DISTINCT id), count(DISTINCT slug) FROM $S.projects"),
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(ids, (20, 20, 20));

        // Both store the first event of one group at once: maybe two rows in
        // `issues`, one issue in every answer. No event lost, under maintenance
        // of both.
        let now = Utc::now();
        let body = serde_json::json!({"message": "m", "fingerprint": ["g"]});
        let mut tasks = Vec::new();
        for db in [a.clone(), b.clone()] {
            let body = body.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..40 {
                    store(&db, 1, body.clone(), now).await;
                }
            }));
        }
        for db in [a.clone(), b.clone()] {
            tasks.push(tokio::spawn(async move {
                let r: Result<()> = db.call(|reply| WriteCmd::Maintenance { reply }).await;
                if let Err(e) = r {
                    assert!(format!("{e:#}").to_lowercase().contains("conflict"), "{e:#}");
                }
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let issues = issues_of(&a, 1).await;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].count, 80);
        let group_hash = read::parse_group_id(&issues[0].id).unwrap();
        let w = issue_window();
        let detail = b.read(move |c, ns| read::issue_detail(c, ns, 1, group_hash, &w)).await.unwrap().unwrap();
        assert_eq!(detail.summary.count, 80);
        let stats = a.read(move |c, ns| read::project_stats(c, ns, 1, &w, None)).await.unwrap();
        assert_eq!(stats.new_issues, 1, "counted once, also with two rows");

        // A resolve in one process reaches the writer of the other, so an
        // event there reopens the issue.
        let set = a.call(move |reply| WriteCmd::SetStatus {
            project_id: 1,
            group_hash,
            status: IssueStatus::Resolved,
            reason: "user".into(),
            reply,
        });
        assert!(set.await.unwrap());
        eventually("b learns of the resolve", async || {
            b.caches.issues.get(&(1, group_hash)).is_some_and(|i| i.status == IssueStatus::Resolved)
        })
        .await;
        store(&b, 1, body, Utc::now()).await;
        assert_eq!(issues_of(&a, 1).await[0].status, IssueStatus::Unresolved);

        // A bundle uploaded through one process is used by the other.
        let bundle = NewBundle {
            bundle_id: "abc".into(),
            project_ids: vec![1],
            release: Some("1.0".into()),
            dist: None,
            object_key: "bundles/abc.zip".into(),
            size_bytes: 1,
            files: vec![],
        };
        let r: Result<()> = a.call(|reply| WriteCmd::AddBundle { bundle: Box::new(bundle), reply }).await;
        r.unwrap();
        eventually("b learns of the bundle", async || b.caches.project_has_release(1, "1.0")).await;

        a.shutdown().await;
        b.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_processes_on_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        two_processes(&format!("sqlite:{}", dir.path().join("catalog.sqlite").display()), dir.path()).await;
    }

    /// Two spellings of one app at once, also from two processes: one project (ADR 0031).
    #[tokio::test(flavor = "multi_thread")]
    async fn two_spellings_of_one_app_make_one_project() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = format!("sqlite:{}", dir.path().join("catalog.sqlite").display());
        let a = Arc::new(open_db_on(&catalog, dir.path(), 2, |_| {}));
        let b = Arc::new(open_db_on(&catalog, dir.path(), 2, |_| {}));
        for i in 0..4 {
            let (one, other) = (format!("app_{i}"), format!("app-{i}"));
            let (a2, b2) = (a.clone(), b.clone());
            let t1 = tokio::spawn(async move { create(&a2, &one).await });
            let t2 = tokio::spawn(async move { create(&b2, &other).await });
            let results = [t1.await.unwrap(), t2.await.unwrap()];
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "app {i}: {results:?}");
        }
        for db in [a, b] {
            Arc::into_inner(db).unwrap().shutdown().await;
        }
    }

    /// A catalog that already has both spellings keeps both, and the exact slug wins (ADR 0029).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_with_both_spellings_keeps_answering() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 1);
        create(&db, "my_app").await.unwrap();
        let conn = db.readers.lock().unwrap().pop().unwrap();
        conn.execute_batch(&db.ns.sql(
            "INSERT INTO $S.projects SELECT 2, 'my-app', name, platform, public_key, created_at FROM $S.projects",
        ))
        .unwrap();
        db.readers.lock().unwrap().push(conn);
        let taken = |r: Result<ProjectRow, WriteError>| match r {
            Err(WriteError::SlugTaken(s)) => format!("slug {s}"),
            Err(WriteError::SameApp(s)) => format!("app {s}"),
            other => panic!("{other:?}"),
        };
        assert_eq!(taken(create(&db, "my_app").await), "slug my_app");
        assert_eq!(taken(create(&db, "my-app").await), "slug my-app");
        assert_eq!(taken(create(&db, "my__app").await), "app my-app");
        db.shutdown().await;
    }

    /// Needs an empty Postgres database of its own, e.g.
    /// `MALLARD_TEST_POSTGRES='host=127.0.0.1 port=5432 dbname=lake user=postgres password=pw'`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn two_processes_on_postgres() {
        let dir = tempfile::tempdir().unwrap();
        let libpq = std::env::var("MALLARD_TEST_POSTGRES").expect("MALLARD_TEST_POSTGRES");
        two_processes(&format!("postgres:{libpq}"), dir.path()).await;
    }

    /// Issue 0007: an event from before the resolve (a replay, an offline
    /// client) does not reopen the issue; a newer one does, also when both are
    /// in one batch.
    #[tokio::test(flavor = "multi_thread")]
    async fn only_events_after_the_resolve_reopen_an_issue() {
        use mallard_sentry_protocol::{NormalizeContext, ProcessedEvent, normalize_event};
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 1);
        let at = |minutes_ago: i64| Utc::now() - chrono::Duration::minutes(minutes_ago);
        let body = |t: DateTime<Utc>| serde_json::json!({"message": "m", "fingerprint": ["g"], "timestamp": t.timestamp() as f64});
        store(&db, 1, body(at(30)), Utc::now()).await;
        let group_hash = read::parse_group_id(&issues_of(&db, 1).await[0].id).unwrap();
        let status = async || issues_of(&db, 1).await[0].status;
        let set = db.call(move |reply| WriteCmd::SetStatus {
            project_id: 1,
            group_hash,
            status: IssueStatus::Resolved,
            reason: "user".into(),
            reply,
        });
        assert!(set.await.unwrap());
        store(&db, 1, body(at(10)), Utc::now()).await;
        assert_eq!(status().await, IssueStatus::Resolved, "older than the resolve");
        // Also when the cache has to read the resolve time back.
        db.caches.issues.invalidate_all();
        store(&db, 1, body(at(9)), Utc::now()).await;
        assert_eq!(status().await, IssueStatus::Resolved, "older than the resolve, not cached");

        // One batch, old event first: the newer one still reopens.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let now = Utc::now();
        let ctx = NormalizeContext::new(now);
        let mut replies = Vec::new();
        for t in [at(8), now] {
            let ev = normalize_event(body(t), &ctx);
            let (tx, rx) = oneshot::channel();
            let event = ProcessedEvent::from_normalized(&ev, now);
            let w = EventWrite { project_id: 1, event, payload_json: ev.to_string(), reply: tx };
            db.try_send(WriteCmd::Event(Box::new(w))).unwrap();
            replies.push(rx);
        }
        for rx in replies {
            rx.await.unwrap().unwrap();
        }
        assert_eq!(status().await, IssueStatus::Unresolved);
        db.shutdown().await;
    }

    /// Issue 0007: an event older than the first one of the issue moves
    /// first seen back in the issue list and detail, for every sort and filter.
    #[tokio::test(flavor = "multi_thread")]
    async fn older_events_lower_first_seen_when_read() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 1);
        let later = DateTime::from_timestamp(Utc::now().timestamp() - 600, 0).unwrap();
        let earlier = later - chrono::Duration::minutes(20);
        let body = |t: DateTime<Utc>, g: &str| serde_json::json!({"message": g, "fingerprint": [g], "timestamp": t.timestamp() as f64});
        store(&db, 1, body(later, "g"), Utc::now()).await;
        store(&db, 1, body(later - chrono::Duration::minutes(5), "h"), Utc::now()).await;
        store(&db, 1, body(earlier, "g"), Utc::now()).await; // a later batch

        let list = issues_of(&db, 1).await;
        let g = list.iter().find(|i| i.title == "g").unwrap();
        assert_eq!(g.first_seen, earlier);
        let group = read::parse_group_id(&g.id).unwrap();
        let w = issue_window();
        let detail = db.read(move |c, ns| read::issue_detail(c, ns, 1, group, &w)).await.unwrap().unwrap();
        assert_eq!(detail.summary.first_seen, earlier);

        // Sort by first seen, newest first: h (5 min before g's first row, but
        // after g's older event) comes first.
        let sorted = db
            .read(move |c, ns| {
                let q = mallard_sentry_api::IssueQuery::default();
                read::list_issues(c, ns, 1, &q, &[], &w, read::StatusFilter::All, read::IssueSort::FirstSeen, None)
            })
            .await
            .unwrap()
            .issues;
        assert_eq!(sorted[0].title, "h");

        let new = db
            .read(move |c, ns| {
                let q = mallard_sentry_api::IssueQuery { new: Some(true), ..Default::default() };
                read::list_issues(c, ns, 1, &q, &[], &w, read::StatusFilter::All, read::IssueSort::LastSeen, None)
            })
            .await
            .unwrap();
        assert_eq!(new.issues.len(), 2, "both start inside the window");
        db.shutdown().await;
    }

    /// Sentry issue 0001: a second `issues` row of a group (two processes wrote one)
    /// changes no answer.
    #[tokio::test(flavor = "multi_thread")]
    async fn duplicate_issue_rows_count_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 1);
        let now = Utc::now();
        let body = serde_json::json!({"message": "m", "fingerprint": ["g"], "release": "1.0"});
        store(&db, 1, body, now).await;
        let conn = db.readers.lock().unwrap().pop().unwrap();
        // Later first_seen and another title: the earlier row must win.
        conn.execute_batch(&db.ns.sql(
            "INSERT INTO $S.issues SELECT project_id, group_hash, first_seen + INTERVAL 1 SECOND, first_event_id,
             '2.0', 'later title', culprit, level, platform, exception_type FROM $S.issues",
        ))
        .unwrap();
        db.readers.lock().unwrap().push(conn);
        assert_eq!(count_issue_rows(&db, 1).await, 2);

        let w = issue_window();
        let issues = issues_of(&db, 1).await;
        assert_eq!(issues.len(), 1);
        let first_seen = issues[0].first_seen;
        let stats = db.read(move |c, ns| read::project_stats(c, ns, 1, &w, None)).await.unwrap();
        assert_eq!(stats.new_issues, 1);
        let releases = db.read(move |c, ns| read::list_releases(c, ns, 1, &w, 10)).await.unwrap().releases;
        assert_eq!((releases[0].version.as_str(), releases[0].new_issues), ("1.0", 1));
        let group = read::parse_group_id(&issues[0].id).unwrap();
        let detail = db.read(move |c, ns| read::issue_detail(c, ns, 1, group, &w)).await.unwrap().unwrap();
        assert_eq!((detail.summary.first_seen, detail.first_release.as_deref()), (first_seen, Some("1.0")));
        db.shutdown().await;
    }

    /// Sentry issue 0001: one process holds the maintenance lease; the other takes
    /// over when it runs out. Holders renew theirs.
    #[tokio::test(flavor = "multi_thread")]
    async fn one_process_holds_the_maintenance_lease() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 1);
        let long = Duration::from_secs(600);
        let lease = |who: &'static str, d: Duration| {
            db.read(move |c, ns| {
                c.execute_batch("ROLLBACK")?;
                mallard_duck::try_lease(c, &ns.sql("$S.meta"), who, d)
            })
        };
        assert!(lease("a", long).await.unwrap());
        assert!(!lease("b", long).await.unwrap(), "a holds it");
        assert!(lease("a", Duration::from_millis(1)).await.unwrap(), "a renews, briefly");
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(lease("b", long).await.unwrap(), "a's has run out");
        assert!(!lease("a", long).await.unwrap(), "b holds it now");
        db.shutdown().await;
    }

    /// The timed flush goes through the lease, on a catalog with one process
    /// too: inlined rows reach Parquet without anyone asking.
    #[tokio::test(flavor = "multi_thread")]
    async fn timed_flush_takes_the_lease() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = dir.path().join("catalog.ducklake").display().to_string();
        let db = open_db_on(&catalog, dir.path(), 1, |c| c.maintenance.flush_interval = Duration::from_millis(100));
        store(&db, 1, serde_json::json!({"message": "m"}), Utc::now()).await;
        eventually("the flush wrote a file", async || {
            db.read(|c, ns| {
                let n: i64 = c.query_row(
                    &ns.sql("SELECT count(*) FROM ducklake_list_files($C, 'events', schema => $N)"),
                    [],
                    |r| r.get(0),
                )?;
                Ok(n > 0)
            })
            .await
            .unwrap()
        })
        .await;
        let lease: String = db
            .read(|c, ns| {
                Ok(c.query_row(&ns.sql("SELECT value FROM $S.meta WHERE key = ?"), [mallard_duck::LEASE], |r| {
                    r.get(0)
                })?)
            })
            .await
            .unwrap();
        assert!(lease.contains(' '), "lease held: {lease:?}");
        db.shutdown().await;
    }

    /// Two processes that both see a free lease: the second commit loses,
    /// also on the very first lease of a catalog.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_lease_race_has_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = format!("sqlite:{}", dir.path().join("catalog.sqlite").display());
        let a = open_db_on(&catalog, dir.path(), 1, |_| {});
        let b = open_db_on(&catalog, dir.path(), 1, |_| {});
        let ns = a.ns.clone();
        let (ca, cb) = (a.readers.lock().unwrap().pop().unwrap(), b.readers.lock().unwrap().pop().unwrap());
        let claim = |c: &Connection, who: &str| -> Result<()> {
            c.execute_batch("BEGIN TRANSACTION")?;
            let _: i64 =
                c.query_row(&ns.sql("SELECT count(*) FROM $S.meta WHERE key = ?"), [mallard_duck::LEASE], |r| {
                    r.get(0)
                })?;
            c.execute(&ns.sql("DELETE FROM $S.meta WHERE key = ?"), [mallard_duck::LEASE])?;
            c.execute(&ns.sql("INSERT INTO $S.meta VALUES (?, ?, make_timestamp(0))"), [mallard_duck::LEASE, who])?;
            Ok(())
        };
        claim(&ca, "a 0").unwrap();
        claim(&cb, "b 0").unwrap();
        let results = [ca.execute_batch("COMMIT"), cb.execute_batch("COMMIT")];
        let _ = ca.execute_batch("ROLLBACK");
        let _ = cb.execute_batch("ROLLBACK");
        let errors: Vec<_> = results.into_iter().filter_map(Result::err).collect();
        assert_eq!(errors.len(), 1, "exactly one commit loses: {errors:?}");
        a.readers.lock().unwrap().push(ca);
        b.readers.lock().unwrap().push(cb);
        a.shutdown().await;
        b.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn schema_version_is_recorded_once() {
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..2 {
            let db = open_db(dir.path(), 1);
            db.shutdown().await;
        }
        let db = open_db(dir.path(), 1);
        let n: i64 = db
            .read(|c, ns| {
                Ok(c.query_row(&ns.sql("SELECT count(*) FROM $S.meta WHERE key = 'schema_version'"), [], |r| r.get(0))?)
            })
            .await
            .unwrap();
        assert_eq!(n, 1);
        db.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_for_interrupts_a_slow_query() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 1);
        let slow = db
            .read_for(Duration::from_millis(200), |c, _| {
                Ok(c.query_row("SELECT sum(i) FROM range(1000000000000) t(i)", [], |r| r.get::<_, i128>(0))?)
            })
            .await
            .unwrap();
        assert!(slow.is_none(), "timed out");
        // The only reader comes back once the query stopped, and is not
        // interrupted again.
        let next = tokio::time::timeout(Duration::from_secs(20), select_one(&db)).await;
        assert_eq!(next.expect("the reader came back").unwrap(), 1);
        let fast = db.read_for(Duration::from_secs(5), |c, _| Ok(c.query_row("SELECT 2", [], |r| r.get::<_, i32>(0))?));
        assert_eq!(fast.await.unwrap(), Some(2));
        db.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn panicking_read_returns_a_clean_connection() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(dir.path(), 1);
        let r: Result<()> = db.read(|_, _| panic!("boom")).await;
        assert!(r.is_err());
        for _ in 0..3 {
            assert_eq!(select_one(&db).await.unwrap(), 1, "the reader must not be left in a transaction");
        }
        db.shutdown().await;
    }
}
