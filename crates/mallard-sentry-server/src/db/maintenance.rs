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
const COMPACTED_TABLES: &[&str] =
    &["events", "issues", "issue_activity", "artifact_files", "releases", "release_activity", "deploys"];

/// How often the inlined-rows counter is checked.
const ROWS_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Settings for maintenance that is not scoped to this schema.
#[derive(Debug, Clone, Copy)]
pub struct Maintenance {
    /// Expire snapshots and delete old files catalog-wide, keeping
    /// snapshots this long (`None`: another component does it).
    pub catalog_wide: Option<Duration>,
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
        // `stopping` too: the `Stop` command waits behind what was queued before it.
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
                        metrics::counter!("mallard_sentry_maintenance_errors_total").increment(1);
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
            if leader
                && let Err(e) = self.compact_and_expire()
                && !self.stopping.load(Ordering::Relaxed)
            {
                tracing::error!(error = format!("{e:#}"), "compaction failed");
            }
            self.compact.done();
        }
    }

    fn renew_lease(&self) -> bool {
        match mallard_duck::try_lease(&self.conn, &self.ns.sql("$S.meta"), &self.holder, self.lease_for) {
            Ok(leader) => leader,
            Err(e) => {
                metrics::counter!("mallard_sentry_maintenance_errors_total").increment(1);
                tracing::warn!(error = format!("{e:#}"), "maintenance lease failed");
                false
            }
        }
    }

    /// Answered only when all of it is done: the one-shot command exits then.
    pub fn run_now(&mut self) -> Result<()> {
        let r = again_on_conflict(&self.stopping, || self.flush_inlined()).and_then(|_| self.compact_and_expire());
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
        metrics::histogram!("mallard_sentry_compact_seconds").record(started.elapsed().as_secs_f64());
        if self.stopping.load(Ordering::Relaxed) {
            // Cut short on purpose; a caller must not take this for done.
            return Err(anyhow!("maintenance stopped by shutdown"));
        }
        if errors.is_empty() {
            tracing::info!(ms = started.elapsed().as_millis() as u64, "compaction and snapshot expiry done");
            Ok(())
        } else {
            metrics::counter!("mallard_sentry_maintenance_errors_total").increment(errors.len() as u64);
            Err(anyhow!(errors.join("; ")))
        }
    }
}

/// How often [`Maintainer::run_now`] tries a flush.
const FLUSH_TRIES: usize = 3;

/// Run `flush`, again if it lost to other commits. Next to a writer that commits all the time, a
/// flush can use up all of DuckLake's own retries. The periodic flush can wait for its next turn,
/// but whoever asked for this run waits for the answer.
fn again_on_conflict(stopping: &AtomicBool, mut flush: impl FnMut() -> Result<()>) -> Result<()> {
    let mut tries = 1;
    loop {
        match flush() {
            Err(e) if tries < FLUSH_TRIES && mallard_duck::is_conflict(&e) && !stopping.load(Ordering::Relaxed) => {
                tracing::debug!(error = format!("{e:#}"), "flush lost to other commits; trying again");
                tries += 1;
            }
            r => return r,
        }
    }
}

/// Write inlined rows of this schema to Parquet.
pub fn flush_inlined(conn: &Connection, ns: &Ns) -> Result<()> {
    let started = Instant::now();
    let sql = ns.sql("CALL ducklake_flush_inlined_data($C, schema_name => $N)");
    conn.execute_batch(&sql).context("ducklake_flush_inlined_data")?;
    metrics::histogram!("mallard_sentry_flush_seconds").record(started.elapsed().as_secs_f64());
    tracing::debug!(ms = started.elapsed().as_millis() as u64, "flushed inlined data");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tries(stopping: bool, error: &str) -> (usize, bool) {
        let mut n = 0;
        let r = again_on_conflict(&AtomicBool::new(stopping), || {
            n += 1;
            Err(anyhow!("{error}"))
        });
        (n, r.is_ok())
    }

    #[test]
    fn a_flush_tries_again_only_after_a_conflict() {
        assert_eq!(tries(false, "Conflict on update"), (FLUSH_TRIES, false));
        assert_eq!(tries(false, "disk full"), (1, false));
        assert_eq!(tries(true, "Conflict on update"), (1, false));
    }

    #[test]
    fn a_flush_stops_trying_once_it_worked() {
        let mut n = 0;
        let r = again_on_conflict(&AtomicBool::new(false), || {
            n += 1;
            if n == 1 { Err(anyhow!("Conflict on update")) } else { Ok(()) }
        });
        assert!(r.is_ok());
        assert_eq!(n, 2);
    }
}
