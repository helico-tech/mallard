//! DuckDB + DuckLake access.
//!
//! - One **writer thread** owns the primary connection. All writes go
//!   through it as commands on a bounded channel, so there are never write
//!   conflicts inside the process, and events are committed in batches.
//! - A small **reader pool** of cloned connections serves the query API.
//!   DuckLake snapshots give readers a consistent view while the writer
//!   commits.

pub mod read;
pub mod schema;
pub mod writer;

use crate::config::Config;
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use mallard_duck::duckdb::{Connection, InterruptHandle};
use sdl_types::IssueStatus;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
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

/// In-memory caches shared by the writer and request handlers.
#[derive(Default)]
pub struct Caches {
    pub projects: RwLock<HashMap<u32, ProjectRow>>,
    /// Current status per (project, group). Also the set of known issues.
    pub issues: RwLock<HashMap<(u32, u64), IssueStatus>>,
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

impl Caches {
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
        maintenance: writer::Maintenance,
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

        let caches = Arc::new(Caches::default());
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
        drop(conn);

        let (tx, rx) = crossbeam_channel::bounded(cfg.ingest.queue_capacity.max(1));
        let w = writer::Writer::new(writer_conn, ns.clone(), rx, caches.clone(), cfg, maintenance)?;
        let handle = std::thread::Builder::new().name("ducklake-writer".into()).spawn(move || w.run())?;

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
        })
    }

    /// Write what is queued, flush inlined data and stop the writer thread.
    /// Idempotent. Later writes fail with "writer stopped".
    pub async fn shutdown(&self) {
        let Some(handle) = self.writer_thread.lock().expect("lock").take() else { return };
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
    /// Call `shutdown().await` before dropping. Without it, dropping `tx`
    /// disconnects the queue: the writer writes what is queued, flushes and
    /// stops on its own thread (it owns its connection, so DuckDB stays up
    /// until then). Drop does not wait for that: it may run on an async
    /// worker thread.
    fn drop(&mut self) {
        let running = self.writer_thread.get_mut().map(|h| h.is_some()).unwrap_or(false);
        if running {
            tracing::warn!("Db dropped without shutdown(); the writer finishes in the background");
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
    use std::time::Duration;

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
        let maintenance = writer::Maintenance { catalog_wide: Some(Duration::from_secs(3600)), threads: 1 };
        Db::with_connection(conn, &cfg, maintenance, "test".into()).unwrap()
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
