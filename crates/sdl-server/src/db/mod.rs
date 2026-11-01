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
use crate::storage::{Storage, sql_str};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use duckdb::Connection;
use sdl_types::IssueStatus;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{Semaphore, oneshot};
pub use writer::{EventWrite, NewBundle, NewBundleFile, WriteCmd};

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
    /// (bundle checksum, project) pairs that are stored and indexed.
    pub bundles: RwLock<HashSet<(String, u32)>>,
}

impl Caches {
    pub fn project(&self, id: u32) -> Option<ProjectRow> {
        self.projects.read().expect("lock").get(&id).cloned()
    }
    pub fn project_by_ref(&self, r: &str) -> Option<ProjectRow> {
        let p = self.projects.read().expect("lock");
        r.parse::<u32>().ok().and_then(|id| p.get(&id).cloned()).or_else(|| p.values().find(|p| p.slug == r).cloned())
    }
    pub fn bundle_known(&self, checksum: &str, projects: &[u32]) -> bool {
        let b = self.bundles.read().expect("lock");
        projects.iter().all(|p| b.contains(&(checksum.to_string(), *p)))
    }
    pub fn project_has_bundles(&self, project: u32) -> bool {
        self.bundles.read().expect("lock").iter().any(|(_, p)| *p == project)
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

/// Prepare a fresh connection: settings and extensions.
fn configure(conn: &Connection, cfg: &Config) -> Result<()> {
    if let Some(dir) = &cfg.extension_dir {
        conn.execute_batch(&format!("SET extension_directory = {}", sql_str(&dir.display().to_string())))?;
    }
    if let Some(t) = cfg.duckdb_threads {
        conn.execute_batch(&format!("SET threads = {t}"))?;
    }
    if let Some(d) = &cfg.duckdb_temp_dir {
        conn.execute_batch(&format!("SET temp_directory = {}", sql_str(&d.display().to_string())))?;
    }
    if let Some(m) = &cfg.duckdb_memory_limit {
        conn.execute_batch(&format!("SET memory_limit = {}", sql_str(m)))?;
    }
    Ok(())
}

/// Extensions needed for a catalog string.
fn catalog_extensions(catalog: &str) -> &'static [&'static str] {
    if catalog.starts_with("postgres:") {
        &["postgres"]
    } else if catalog.starts_with("sqlite:") {
        &["sqlite"]
    } else if catalog.starts_with("mysql:") {
        &["mysql"]
    } else {
        &[]
    }
}

/// Install (download) every extension any configuration may need. Used at
/// Docker build time so the image works without internet access.
pub fn install_all_extensions(cfg: &Config) -> Result<()> {
    let conn = Connection::open_in_memory()?;
    configure(&conn, cfg)?;
    // json and parquet are linked in; mysql is not pre-installed (DuckLake
    // advises against it as a catalog) and is downloaded on first use.
    for ext in ["ducklake", "httpfs", "aws", "azure", "postgres", "sqlite"] {
        conn.execute_batch(&format!("INSTALL {ext}")).with_context(|| format!("installing extension {ext}"))?;
        tracing::info!(ext, "installed");
    }
    Ok(())
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
    /// Standalone: open DuckDB, load extensions, attach the catalog.
    pub fn open(cfg: &Config, storage: &Storage) -> Result<Db> {
        let conn = Connection::open_in_memory()?;
        configure(&conn, cfg)?;

        let mut exts: Vec<&str> = vec!["ducklake"];
        exts.extend(catalog_extensions(&cfg.catalog));
        exts.extend(storage.duckdb_extensions());
        if storage.duckdb_secret_sql().is_some_and(|s| s.contains("credential_chain")) {
            exts.push(if storage.kind == crate::storage::Kind::Azure { "azure" } else { "aws" });
        }
        for ext in exts {
            conn.execute_batch(&format!("INSTALL {ext}; LOAD {ext};"))
                .with_context(|| format!("loading DuckDB extension {ext}"))?;
        }
        if let Some(secret) = storage.duckdb_secret_sql() {
            conn.execute_batch(&secret).context("creating storage secret")?;
        }

        if catalog_extensions(&cfg.catalog).is_empty()
            && let Some(parent) = std::path::Path::new(&cfg.catalog).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let attach = format!(
            "ATTACH {} AS {} (DATA_PATH {})",
            sql_str(&format!("ducklake:{}", cfg.catalog)),
            cfg.lake_alias,
            sql_str(&storage.lake_data_path())
        );
        conn.execute_batch(&attach)
            .with_context(|| format!("attaching DuckLake catalog {:?}", redact(&cfg.catalog)))?;
        Self::with_connection(conn, cfg, redact(&cfg.catalog))
    }

    /// Embedded: `conn` already has the DuckLake catalog attached as
    /// `cfg.lake_alias` (and any secrets set). We only create our schema.
    pub fn with_connection(conn: Connection, cfg: &Config, catalog_desc: String) -> Result<Db> {
        let ns = Ns::new(&cfg.lake_alias, &cfg.schema);
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

        let n = cfg.readers.max(1);
        let mut readers = Vec::with_capacity(n);
        for _ in 0..n {
            readers.push(conn.try_clone()?);
        }
        let writer_conn = conn.try_clone()?;
        drop(conn);

        let (tx, rx) = crossbeam_channel::bounded(cfg.queue_capacity.max(1));
        let w = writer::Writer::new(writer_conn, ns.clone(), rx, caches.clone(), cfg)?;
        let handle = std::thread::Builder::new().name("ducklake-writer".into()).spawn(move || w.run())?;

        Ok(Db {
            tx,
            readers: Arc::new(Mutex::new(readers)),
            permits: Arc::new(Semaphore::new(n)),
            caches,
            ns,
            duckdb_version,
            catalog: catalog_desc,
            queue_capacity: cfg.queue_capacity,
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
    pub async fn call<T>(&self, make: impl FnOnce(oneshot::Sender<Result<T>>) -> WriteCmd) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        let cmd = make(tx);
        let sender = self.tx.clone();
        // Management calls may wait for queue space instead of failing.
        tokio::task::spawn_blocking(move || sender.send(cmd)).await?.map_err(|_| anyhow!("writer stopped"))?;
        rx.await.map_err(|_| anyhow!("writer stopped"))?
    }

    /// Run a read query on a pooled connection in a blocking task. The
    /// closure gets the namespace to expand `$S` in its SQL.
    pub async fn read<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection, &Ns) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let _permit = self.permits.clone().acquire_owned().await?;
        let readers = self.readers.clone();
        let ns = self.ns.clone();
        tokio::task::spawn_blocking(move || {
            let conn = readers.lock().expect("lock").pop().expect("permit guarantees a connection");
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&conn, &ns)));
            readers.lock().expect("lock").push(conn);
            match result {
                Ok(r) => r,
                Err(_) => Err(anyhow!("query panicked")),
            }
        })
        .await?
    }
}

impl Drop for Db {
    /// Safety net for embedders that forget `shutdown()`: stop and join the
    /// writer so DuckDB is not torn down under a running thread.
    fn drop(&mut self) {
        if let Some(handle) = self.writer_thread.lock().expect("lock").take() {
            let (tx, _rx) = oneshot::channel();
            if self.tx.send(WriteCmd::Shutdown { reply: tx }).is_ok() {
                let _ = handle.join();
            }
        }
    }
}

/// Hide passwords in catalog strings for logs and /v1/info.
pub fn redact(catalog: &str) -> String {
    catalog
        .split_whitespace()
        .map(|part| if part.to_ascii_lowercase().starts_with("password=") { "password=***" } else { part })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn us_to_dt(us: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(us).unwrap_or_default()
}
