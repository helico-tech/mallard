//! Background maintenance: compaction, retention, snapshot expiry, cleanup.
//!
//! Schedule (all configurable, see `[maintenance]`):
//! - every `compact_interval`: flush inlined rows to Parquet, then merge files
//!   smaller than `small_file_size` (cheap: only touches small files).
//! - every `full_compact_interval`: merge files up to `target_file_size`.
//! - every `retention_interval`: delete expired rows, expire old snapshots and
//!   delete unreferenced files after `file_cleanup_delay`.
//!
//! Retention is aligned to whole UTC days when the retention is at least one
//! day. Tables are partitioned by day, so the delete drops whole files and
//! writes no delete files. Data is kept at least `retention` and at most one
//! day longer.
//!
//! Merges and deletes are scoped to this schema's tables. Snapshot expiry and
//! file cleanup are catalog-wide and only run when `catalog_wide = true`.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use duckdb::Connection;
use odl_api::MaintenanceInfo;
use tracing::{info, warn};

use super::{CATALOG, Lake, schema::TABLES, sql_str};
use crate::{
    config::{MaintenanceConfig, RetentionConfig},
    stats::Stats,
};

pub struct Maintenance {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Maintenance {
    pub fn start(lake: &Lake, m: MaintenanceConfig, r: RetentionConfig, stats: Arc<Stats>) -> Result<Self> {
        let conn = lake.connect()?;
        let schema = lake.schema.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = std::thread::Builder::new()
            .name("odl-maintenance".into())
            .spawn(move || run(conn, schema, m, r, stats, stop2))?;
        Ok(Self { stop, handle: Some(handle) })
    }

    /// Stop after the current task and wait for the thread.
    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for Maintenance {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.thread().unpark();
            let _ = h.join();
        }
    }
}

fn run(
    conn: Connection,
    schema: String,
    s: MaintenanceConfig,
    r: RetentionConfig,
    stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
) {
    let start = Instant::now();
    // Stagger the first runs a little so start-up stays fast.
    // Random offset so replicas sharing a catalog don't compact in lock-step
    // (concurrent merges of the same files conflict and waste work).
    let jitter = jitter(s.compact_interval / 2);
    let mut next_compact = start + s.compact_interval.min(Duration::from_secs(60)) + jitter;
    let mut next_full = start + s.full_compact_interval + jitter;
    let mut next_retention = start + Duration::from_secs(30).min(s.retention_interval);
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= next_retention {
            task(&stats, "retention", || retention(&conn, &schema, &s, &r));
            next_retention = Instant::now() + s.retention_interval;
        } else if now >= next_full {
            task(&stats, "full_compaction", || compact(&conn, &schema, None));
            next_full = Instant::now() + s.full_compact_interval;
            next_compact = Instant::now() + s.compact_interval;
        } else if now >= next_compact {
            task(&stats, "compaction", || compact(&conn, &schema, Some(s.small_file_size.0)));
            next_compact = Instant::now() + s.compact_interval;
        } else {
            // Woken early by `stop`.
            std::thread::park_timeout(next_retention.min(next_full).min(next_compact) - now);
        }
    }
    info!("maintenance stopped");
}

/// Pseudo-random duration in `[0, max)`, without a RNG dependency.
fn jitter(max: Duration) -> Duration {
    let ms = max.as_millis() as u64;
    if ms == 0 {
        return Duration::ZERO;
    }
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_nanos() as u64;
    let seed = nanos ^ ((std::process::id() as u64) << 20);
    Duration::from_millis(xxhash_rust::xxh3::xxh3_64(&seed.to_le_bytes()) % ms)
}

/// Run one task and record the outcome. A panic counts as a failure, so the
/// maintenance thread keeps running.
fn task(stats: &Stats, name: &str, f: impl FnOnce() -> Result<String>) {
    let t0 = Instant::now();
    let res = catch_unwind(AssertUnwindSafe(f))
        .unwrap_or_else(|p| Err(anyhow::anyhow!("panicked: {}", super::writer::panic_message(&*p))));
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let (ok, detail) = match res {
        Ok(d) => {
            info!(task = name, ms, detail = %d, "maintenance done");
            (true, d)
        }
        Err(e) => {
            // Conflicts with a concurrent writer are expected occasionally; next run retries.
            let msg = super::redact(&format!("{e:#}"));
            warn!(task = name, ms, error = %msg, "maintenance failed");
            (false, msg)
        }
    };
    stats.maintenance(MaintenanceInfo { task: name.to_string(), at: Utc::now(), duration_ms: ms, ok, detail });
}

/// Flush inlined data and merge adjacent files. `max_file_size = None` merges
/// everything below the table's target file size.
pub fn compact(conn: &Connection, schema: &str, max_file_size: Option<u64>) -> Result<String> {
    let flushed: i64 = conn
        .query_row(
            &format!(
                "SELECT coalesce(sum(rows_flushed), 0)::BIGINT FROM ducklake_flush_inlined_data({}, schema_name => {})",
                sql_str(CATALOG),
                sql_str(schema)
            ),
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| {
            warn!(error = %super::redact(&e.to_string()), "flushing inlined rows failed; will retry");
            -1
        });
    let mut merged = 0i64;
    let mut created = 0i64;
    for t in TABLES {
        let size = max_file_size.map(|b| format!(", max_file_size => {b}")).unwrap_or_default();
        let sql = format!(
            "SELECT coalesce(sum(files_processed), 0)::BIGINT, coalesce(sum(files_created), 0)::BIGINT \
             FROM ducklake_merge_adjacent_files({}, {}, schema => {}{size})",
            sql_str(CATALOG),
            sql_str(t.name),
            sql_str(schema)
        );
        let (p, c): (i64, i64) = conn.query_row(&sql, [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        merged += p;
        created += c;
    }
    let flushed = if flushed < 0 { "FAILED to flush".to_string() } else { format!("flushed {flushed}") };
    Ok(format!("{flushed} inlined rows; merged {merged} files into {created}"))
}

/// `now - d`, or `None` when that is before the earliest representable time.
fn before(now: DateTime<Utc>, d: Duration) -> Option<DateTime<Utc>> {
    TimeDelta::from_std(d).ok().and_then(|d| now.checked_sub_signed(d))
}

/// Cut-off for a retention: aligned to the UTC day when retention >= 1 day.
/// `None` when the retention reaches back further than any timestamp: keep
/// everything.
pub fn retention_cutoff(now: DateTime<Utc>, retention: Duration) -> Option<DateTime<Utc>> {
    let raw = before(now, retention)?;
    Some(if retention >= Duration::from_secs(86_400) {
        raw.duration_trunc(TimeDelta::days(1)).unwrap_or(raw)
    } else {
        raw
    })
}

pub fn retention(conn: &Connection, schema: &str, s: &MaintenanceConfig, r: &RetentionConfig) -> Result<String> {
    let now = Utc::now();
    let mut deleted = 0usize;
    for t in TABLES {
        let keep = r.of(t.signal);
        if keep.is_zero() {
            continue;
        }
        let Some(cutoff) = retention_cutoff(now, keep) else {
            continue;
        };
        let cutoff = cutoff.naive_utc();
        let n =
            conn.execute(&format!("DELETE FROM {CATALOG}.{schema}.{} WHERE {} < ?", t.name, t.time_column), [cutoff])?;
        deleted += n;
    }
    let mut detail = format!("deleted {deleted} expired rows");
    if s.catalog_wide {
        // Absolute instants (epoch seconds -> TIMESTAMPTZ), so the session
        // time zone can't shift the cut-off. A delay reaching back before the
        // earliest representable time keeps everything (cut-off 1970).
        let epoch =
            |d: Duration| -> f64 { before(now, d).unwrap_or(DateTime::UNIX_EPOCH).timestamp_micros() as f64 / 1e6 };
        let snap_cut = epoch(s.snapshot_retention);
        let expired: i64 = conn.query_row(
            &format!(
                "SELECT count(*) FROM ducklake_expire_snapshots({}, older_than => to_timestamp(?::DOUBLE))",
                sql_str(CATALOG)
            ),
            [snap_cut],
            |r| r.get(0),
        )?;
        let file_cut = epoch(s.file_cleanup_delay);
        let cleaned: i64 = conn.query_row(
            &format!(
                "SELECT count(*) FROM ducklake_cleanup_old_files({}, older_than => to_timestamp(?::DOUBLE))",
                sql_str(CATALOG)
            ),
            [file_cut],
            |r| r.get(0),
        )?;
        // Files no snapshot references, e.g. from a flush that failed after
        // writing Parquet. Only older than a day, so files that another
        // replica is writing right now (not yet committed) are never touched.
        let orphan_cut = epoch(Duration::from_secs(86_400).max(s.file_cleanup_delay));
        let orphans: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM ducklake_delete_orphaned_files({}, older_than => to_timestamp(?::DOUBLE))",
                    sql_str(CATALOG)
                ),
                [orphan_cut],
                |r| r.get(0),
            )
            .unwrap_or_else(|e| {
                warn!(error = %super::redact(&e.to_string()), "deleting orphaned files failed");
                0
            });
        detail += &format!("; expired {expired} snapshots; removed {cleaned} old and {orphans} orphaned files");
    }
    Ok(detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn cutoff_aligns_to_days() {
        let now = Utc.with_ymd_and_hms(2026, 10, 31, 13, 45, 0).unwrap();
        let c = retention_cutoff(now, Duration::from_secs(7 * 86400));
        assert_eq!(c, Some(Utc.with_ymd_and_hms(2026, 10, 24, 0, 0, 0).unwrap()));
        let c = retention_cutoff(now, Duration::from_secs(3600));
        assert_eq!(c, Some(Utc.with_ymd_and_hms(2026, 10, 31, 12, 45, 0).unwrap()));
    }

    #[test]
    fn a_panicking_task_is_recorded_as_failed() {
        let stats = Stats::default();
        task(&stats, "compaction", || panic!("boom"));
        let m = stats.last_maintenance().unwrap();
        assert!(!m.ok);
        assert!(m.detail.contains("boom"), "{}", m.detail);
    }

    #[test]
    fn huge_retention_keeps_everything_instead_of_panicking() {
        let now = Utc.with_ymd_and_hms(2026, 10, 31, 13, 45, 0).unwrap();
        let huge = humantime::parse_duration("300000y").unwrap();
        assert_eq!(retention_cutoff(now, huge), None);
        assert_eq!(retention_cutoff(now, Duration::MAX), None);
    }
}
