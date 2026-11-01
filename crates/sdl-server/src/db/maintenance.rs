//! The maintenance thread: flush, compaction and snapshot expiry on their
//! own connection, so they do not pause ingest (ADR 0014).
//!
//! DuckLake lets these run next to the writer: measured with a writer
//! committing all the time, flushes, merges, rewrites, expiry and cleanup
//! caused no conflict on either side. A `DELETE` that removes rows does
//! conflict with every concurrent insert into the table, so retention is the
//! one step that stays on the writer thread ([`WriteCmd::Retention`]).

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
        if self.flush.is_due() || self.inlined.load(Ordering::Relaxed) >= self.flush_rows {
            if let Err(e) = self.flush_inlined()
                && !self.stopping.load(Ordering::Relaxed)
            {
                tracing::error!(error = format!("{e:#}"), "flushing inlined data failed");
            }
            self.flush.done();
        }
        if self.compact.is_due() {
            if let Err(e) = self.compact_and_expire() {
                tracing::error!(error = format!("{e:#}"), "compaction failed");
            }
            self.compact.done();
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

/// Write inlined rows of this schema to Parquet.
pub fn flush_inlined(conn: &Connection, ns: &Ns) -> Result<()> {
    let started = Instant::now();
    let sql = ns.sql("CALL ducklake_flush_inlined_data($C, schema_name => $N)");
    conn.execute_batch(&sql).context("ducklake_flush_inlined_data")?;
    metrics::histogram!("sdl_flush_seconds").record(started.elapsed().as_secs_f64());
    tracing::debug!(ms = started.elapsed().as_millis() as u64, "flushed inlined data");
    Ok(())
}
