//! The maintenance thread: flush, compaction and snapshot expiry on their
//! own connection, so they do not pause ingest (ADR 0014).
//!
//! DuckLake lets these run next to the writer: measured with a writer
//! committing all the time, flushes, merges, rewrites, expiry and cleanup
//! caused no conflict on either side. A `DELETE` that removes rows does
//! conflict with every concurrent insert into the table, so retention is the
//! one step that stays on the writer thread ([`WriteCmd::Retention`]).
//!
//! With several processes on one catalog, the holder of a lease in `meta` runs
//! maintenance (ADR 0021). Overlapping runs are safe, only wasteful: losers of
//! a conflict roll back.

use super::writer::WriteCmd;
use super::{Ns, Tick};
use crate::config::Config;
use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use mallard_duck::duckdb::{Connection, params};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Files merged per table and maintenance run (bounds the work of one run;
/// the rest is merged in later runs).
const MAX_COMPACTED_FILES: usize = 1000;

/// Tables compacted by periodic maintenance.
const COMPACTED_TABLES: &[&str] = &["events", "issues", "issue_activity", "artifact_files"];

/// How often the inlined-rows counter is checked.
const ROWS_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// The `meta` row that says which process runs maintenance.
pub const LEASE: &str = "maintenance_lease";

/// Settings for maintenance that is not scoped to this schema.
#[derive(Debug, Clone, Copy)]
pub struct Maintenance {
    /// Expire snapshots and delete old files catalog-wide, keeping
    /// snapshots this long (`None`: another component does it).
    pub catalog_wide: Option<Duration>,
    /// `SET threads` while compacting (0 = leave alone).
    pub threads: usize,
}

pub enum MaintCmd {
    /// Flush and compact now.
    RunNow {
        reply: oneshot::Sender<Result<()>>,
    },
    Stop,
}

pub struct Maintainer {
    conn: Connection,
    ns: Ns,
    rx: Receiver<MaintCmd>,
    /// For retention, which runs on the writer thread.
    writer: Sender<WriteCmd>,
    /// Rows inlined by the writer since the last flush.
    inlined: Arc<AtomicUsize>,
    /// Set on shutdown: stop between steps instead of finishing the run.
    stopping: Arc<AtomicBool>,
    settings: Maintenance,
    flush_rows: usize,
    flush: Tick,
    compact: Tick,
    /// Who this process is in the lease.
    holder: String,
    /// How long a lease holds: a few flushes, so a stopped process is
    /// replaced soon, and a long run rarely outlives it.
    lease_for: Duration,
}

impl Maintainer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        conn: Connection,
        ns: Ns,
        rx: Receiver<MaintCmd>,
        writer: Sender<WriteCmd>,
        inlined: Arc<AtomicUsize>,
        stopping: Arc<AtomicBool>,
        cfg: &Config,
        settings: Maintenance,
    ) -> Self {
        Maintainer {
            conn,
            ns,
            rx,
            writer,
            inlined,
            stopping,
            settings,
            flush_rows: cfg.maintenance.flush_rows.max(1),
            flush: Tick::new(cfg.maintenance.flush_interval),
            compact: Tick::new(cfg.maintenance.compact_interval),
            holder: uuid::Uuid::new_v4().simple().to_string(),
            lease_for: cfg.maintenance.flush_interval * 3,
        }
    }

    pub fn run(mut self) {
        // `stopping` too: a full queue can lose the `Stop` command.
        while !self.stopping.load(Ordering::Relaxed) {
            let wait = self.flush.due_in().min(self.compact.due_in()).min(ROWS_CHECK_INTERVAL);
            match self.rx.recv_timeout(wait) {
                Ok(MaintCmd::RunNow { reply }) => {
                    let _ = reply.send(self.run_now());
                }
                Ok(MaintCmd::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    // A panic must not end maintenance for good.
                    if catch_unwind(AssertUnwindSafe(|| self.periodic())).is_err() {
                        metrics::counter!("sdl_maintenance_errors_total").increment(1);
                        tracing::error!("a maintenance run panicked");
                    }
                }
            }
        }
        tracing::debug!("maintenance stopped");
    }

    fn periodic(&mut self) {
        let flush_due = self.flush.is_due();
        let compact_due = self.compact.is_due();
        // Every process flushes what it inlined past the limit: only it knows
        // the count, and its rows must not pile up in the catalog.
        let too_many_rows = self.inlined.load(Ordering::Relaxed) >= self.flush_rows;
        if !flush_due && !compact_due && !too_many_rows {
            return;
        }
        let leader = (flush_due || compact_due) && self.renew_lease();
        if too_many_rows || (flush_due && leader) {
            match self.flush_inlined() {
                Ok(()) => {}
                // Another process flushed these rows.
                Err(e) if mallard_duck::is_conflict(&e) || self.stopping.load(Ordering::Relaxed) => {
                    tracing::debug!(error = format!("{e:#}"), "flush did not finish");
                }
                Err(e) => tracing::error!(error = format!("{e:#}"), "flushing inlined data failed"),
            }
        }
        if flush_due {
            self.flush.done();
        }
        if compact_due {
            if leader && let Err(e) = self.compact_and_expire() {
                tracing::error!(error = format!("{e:#}"), "compaction failed");
            }
            self.compact.done();
        }
    }

    fn renew_lease(&self) -> bool {
        match try_lease(&self.conn, &self.ns, &self.holder, self.lease_for) {
            Ok(leader) => leader,
            Err(e) => {
                metrics::counter!("sdl_maintenance_errors_total").increment(1);
                tracing::warn!(error = format!("{e:#}"), "maintenance lease failed");
                false
            }
        }
    }

    fn run_now(&mut self) -> Result<()> {
        let r = self.flush_inlined().and_then(|_| self.compact_and_expire());
        self.flush.done();
        self.compact.done();
        r
    }

    fn flush_inlined(&self) -> Result<()> {
        // Before the flush: rows inlined while it runs count for the next one.
        self.inlined.store(0, Ordering::Relaxed);
        flush_inlined(&self.conn, &self.ns)
    }

    /// Retention on the writer thread, so it never overlaps a commit of this
    /// process. Waits for it: a merge of the same files would conflict.
    fn retention(&self) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.writer.send(WriteCmd::Retention { reply }).map_err(|_| anyhow!("writer stopped"))?;
        rx.blocking_recv().map_err(|_| anyhow!("writer stopped"))?
    }

    fn compact_and_expire(&self) -> Result<()> {
        let ns = &self.ns;
        let started = Instant::now();
        let mut errors = Vec::new();
        if let Err(e) = self.retention() {
            errors.push(format!("retention: {e:#}"));
        }
        // Sorted merges need memory per thread: compact with fewer threads,
        // then restore the previous setting.
        let restore_threads: Option<i64> = match self.settings.threads {
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
            if self.stopping.load(Ordering::Relaxed) {
                break;
            }
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
        if let Some(keep) = self.settings.catalog_wide {
            let cutoff = (Utc::now() - chrono::Duration::from_std(keep).unwrap_or_default()).timestamp_micros();
            for (what, sql) in [
                ("expiring snapshots", "CALL ducklake_expire_snapshots($C, older_than => make_timestamp(?))"),
                ("cleaning up old files", "CALL ducklake_cleanup_old_files($C, older_than => make_timestamp(?))"),
            ] {
                if self.stopping.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = self.conn.execute(&ns.sql(sql), params![cutoff]) {
                    errors.push(format!("{what}: {e}"));
                }
            }
        }
        metrics::histogram!("sdl_compact_seconds").record(started.elapsed().as_secs_f64());
        if self.stopping.load(Ordering::Relaxed) {
            // Interrupted on purpose; the next run starts over.
            return Ok(());
        }
        if errors.is_empty() {
            tracing::info!(ms = started.elapsed().as_millis() as u64, "compaction and snapshot expiry done");
            Ok(())
        } else {
            metrics::counter!("sdl_maintenance_errors_total").increment(errors.len() as u64);
            Err(anyhow!(errors.join("; ")))
        }
    }
}

/// Take or renew the maintenance lease for `lease_for`. False when another
/// process holds it, or takes it at this moment: its `DELETE` of the row makes
/// the second commit conflict.
pub fn try_lease(conn: &Connection, ns: &Ns, holder: &str, lease_for: Duration) -> Result<bool> {
    let now = Utc::now().timestamp_micros();
    let until = now + lease_for.as_micros() as i64;
    conn.execute_batch("BEGIN TRANSACTION")?;
    let attempt = (|| -> Result<bool> {
        let mut stmt = conn.prepare(&ns.sql("SELECT value FROM $S.meta WHERE key = ?"))?;
        let rows = stmt.query_map([LEASE], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        let taken = rows
            .iter()
            .filter_map(|v| v.split_once(' '))
            .any(|(who, till)| who != holder && till.parse::<i64>().is_ok_and(|till| till > now));
        if taken {
            return Ok(false);
        }
        conn.execute(&ns.sql("DELETE FROM $S.meta WHERE key = ?"), [LEASE])?;
        conn.execute(
            &ns.sql("INSERT INTO $S.meta VALUES (?, ?, make_timestamp(?))"),
            params![LEASE, format!("{holder} {until}"), now],
        )?;
        conn.execute_batch("COMMIT")?;
        Ok(true)
    })();
    match attempt {
        Ok(true) => Ok(true),
        Ok(false) => {
            let _ = conn.execute_batch("ROLLBACK");
            Ok(false)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            if mallard_duck::is_conflict(&e) { Ok(false) } else { Err(e) }
        }
    }
}

/// Write inlined rows of this schema to Parquet.
pub fn flush_inlined(conn: &Connection, ns: &Ns) -> Result<()> {
    let started = Instant::now();
    let sql = ns.sql("CALL ducklake_flush_inlined_data($C, schema_name => $N)");
    conn.execute_batch(&sql).context("ducklake_flush_inlined_data")?;
    metrics::histogram!("sdl_flush_seconds").record(started.elapsed().as_secs_f64());
    tracing::debug!(ms = started.elapsed().as_millis() as u64, "flushed inlined data");
    Ok(())
}
