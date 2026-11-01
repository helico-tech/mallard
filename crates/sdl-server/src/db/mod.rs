//! DuckDB + DuckLake access.
//!
//! - One **writer thread** owns the primary connection. All writes go
//!   through it as commands on a bounded channel, so there are never write
//!   conflicts inside the process, and events are committed in batches.
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
use maintenance::MaintCmd;
use mallard_duck::duckdb::{Connection, InterruptHandle};
use sdl_types::IssueStatus;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, oneshot};
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
    pub issues: moka::sync::Cache<(u32, u64), IssueStatus>,
    /// What is stored and indexed per project.
    bundles: RwLock<HashMap<u32, ProjectBundles>>,
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
    tx: crossbeam_channel::Sender<WriteCmd>,
    readers: Arc<Mutex<Vec<Connection>>>,
    permits: Arc<Semaphore>,
    pub caches: Arc<Caches>,
    pub ns: Ns,
    pub duckdb_version: String,
    pub catalog: String,
    pub queue_capacity: usize,
    writer_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    maintainer: Maintainer,
}

/// The maintenance thread as [`Db`] sees it.
#[derive(Clone)]
struct Maintainer {
    tx: crossbeam_channel::Sender<MaintCmd>,
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
        catalog_desc: String,
    ) -> Result<Db> {
        let ns = Ns::new(mallard_duck::CATALOG, crate::SCHEMA);
        let duckdb_version: String = conn.query_row("SELECT version()", [], |r| r.get(0))?;
        for stmt in schema::CREATE {
            conn.execute_batch(&ns.sql(stmt)).with_context(|| format!("creating schema: {stmt}"))?;
        }
        for stmt in schema::table_options() {
            conn.execute_batch(&ns.sql(&stmt)).with_context(|| format!("setting table option: {stmt}"))?;
        }
        writer::record_schema_version(&conn, &ns)?;

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

        let (tx, rx) = crossbeam_channel::bounded(cfg.ingest.queue_capacity.max(1));
        let (maint_tx, maint_rx) = crossbeam_channel::bounded(16);
        let inlined = Arc::new(AtomicUsize::new(0));
        let w =
            writer::Writer::new(writer_conn, ns.clone(), rx, caches.clone(), cfg, maint_tx.clone(), inlined.clone())?;
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
        })
    }

    /// Stop maintenance, write what is queued, flush inlined data and stop
    /// the writer thread. Idempotent. Later writes fail with "writer stopped".
    pub async fn shutdown(&self) {
        let Some(handle) = self.writer_thread.lock().expect("lock").take() else { return };
        let m = self.maintainer.clone();
        let _ = tokio::task::spawn_blocking(move || m.stop_and_join()).await;
        let (tx, rx) = oneshot::channel();
        let sender = self.tx.clone();
        let sent = tokio::task::spawn_blocking(move || sender.send(WriteCmd::Shutdown { reply: tx }).is_ok())
            .await
            .unwrap_or(false);
        if sent {
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
        self.tx.try_send(cmd).map_err(|e| match e {
            crossbeam_channel::TrySendError::Full(_) => EnqueueError::Full,
            crossbeam_channel::TrySendError::Disconnected(_) => EnqueueError::Stopped,
        })
    }

    /// Send a command and wait for its reply (management operations).
    pub async fn call<T, E: From<anyhow::Error>>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, E>>) -> WriteCmd,
    ) -> Result<T, E> {
        let (tx, rx) = oneshot::channel();
        let cmd = make(tx);
        let sender = self.tx.clone();
        // Management calls may wait for queue space instead of failing.
        let sent = tokio::task::spawn_blocking(move || sender.send(cmd).is_ok()).await.map_err(anyhow::Error::from)?;
        if !sent {
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
    /// Call `shutdown().await` before dropping. Without it, maintenance is
    /// told to stop; once it has, and `tx` is gone, the queue disconnects:
    /// the writer writes what is queued, flushes and stops on its own thread
    /// (it owns its connection, so DuckDB stays up until then). Drop does not
    /// wait for that: it may run on an async worker thread.
    fn drop(&mut self) {
        let running = self.writer_thread.get_mut().map(|h| h.is_some()).unwrap_or(false);
        if running {
            tracing::warn!("Db dropped without shutdown(); the writer finishes in the background");
            self.maintainer.stop();
        }
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

    /// Issue 0002: the issue cache has a size limit, and an issue without
    /// events for the retention period leaves it.
    #[test]
    fn issue_cache_is_bounded() {
        let c = Caches::default();
        for g in 0..ISSUE_CACHE_CAPACITY + 1000 {
            c.issues.insert((1, g), IssueStatus::Unresolved);
        }
        c.issues.run_pending_tasks();
        assert!(c.issues.entry_count() <= ISSUE_CACHE_CAPACITY);

        let c = Caches::new(Duration::from_millis(20));
        c.issues.insert((1, 1), IssueStatus::Unresolved);
        assert!(c.issues.get(&(1, 1)).is_some());
        std::thread::sleep(Duration::from_millis(100));
        assert!(c.issues.get(&(1, 1)).is_none());
    }

    /// A Db on a local lake in `dir`.
    pub(crate) fn open_db(dir: &std::path::Path, readers: usize) -> Db {
        let mut cfg = Config::default();
        cfg.query.readers = readers;
        cfg.ingest.batch_window = Duration::from_millis(10);
        let st = mallard_duck::StorageConfig {
            catalog: dir.join("catalog.ducklake").display().to_string(),
            data_path: dir.join("lake").display().to_string(),
            ..Default::default()
        };
        let conn = mallard_duck::open_connection(&st, &Default::default()).unwrap();
        let settings = maintenance::Maintenance { catalog_wide: Some(Duration::from_secs(3600)), threads: 1 };
        Db::with_connection(conn, &cfg, settings, "test".into()).unwrap()
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
        use sdl_protocol::{NormalizeContext, ProcessedEvent, normalize_event};
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
            let q = sdl_types::EventQuery { limit: Some(2), ..Default::default() };
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
        use sdl_protocol::{NormalizeContext, ProcessedEvent, normalize_event};
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
                let q = sdl_types::IssueQuery::default();
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
        use sdl_protocol::{NormalizeContext, ProcessedEvent, normalize_event};
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

    /// Issue 0003: compaction and flushes run next to the writer. Events
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
        use sdl_protocol::{NormalizeContext, ProcessedEvent, normalize_event};
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

    async fn issues_of(db: &Db, project: u32) -> Vec<sdl_types::IssueSummary> {
        let w = issue_window();
        db.read(move |c, ns| {
            let q = sdl_types::IssueQuery { limit: Some(500), ..Default::default() };
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

    /// Issue 0002: after the limit, new groups of that project share one
    /// issue. Existing groups and other projects go on as usual.
    #[tokio::test(flavor = "multi_thread")]
    async fn new_issues_over_the_limit_share_one_issue() {
        use sdl_protocol::{NormalizeContext, ProcessedEvent, normalize_event};
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

    /// Issue 0002: an issue that left the cache is found in the lake: no
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
        store(&db, 1, body, now).await;
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
