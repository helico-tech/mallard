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
//! file cleanup are catalog-wide: otel owns them on a shared catalog (ADR 0001).
//!
//! With several processes on one catalog, the holder of a lease in `meta` runs
//! the tasks (ADR 0021, issue 0051): each task due tries the lease first. It is
//! valid for 3 of the shortest interval, so a stopped process is replaced soon.
//! Overlapping runs stay safe (losers of a conflict retry next run), only wasteful.
//! In one process retention's deletes and the flushes take turns ([`super::Locks::turn`]).
//! The one-shot [`run_once`] ignores the lease.

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
use mallard_duck::duckdb::Connection;
use mallard_otel_api::MaintenanceInfo;
use tracing::{info, warn};

use super::{CATALOG, Lake, Locks, schema::TABLES, sql_str};
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
        let (schema, locks) = (lake.schema.clone(), lake.locks.clone());
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = std::thread::Builder::new()
            .name("otel-maintenance".into())
            .spawn(move || run(conn, schema, locks, m, r, stats, stop2))?;
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
    locks: Locks,
    s: MaintenanceConfig,
    r: RetentionConfig,
    stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
) {
    let start = Instant::now();
    let holder = uuid::Uuid::new_v4().simple().to_string();
    let lease_for = lease_duration(&s);
    let leader = || holds_lease(&conn, &schema, &holder, lease_for);
    // Stagger the first runs a little so start-up stays fast.
    let mut next_compact = start + s.compact_interval.min(Duration::from_secs(60));
    let mut next_full = start + s.full_compact_interval;
    let mut next_retention = start + Duration::from_secs(30).min(s.retention_interval);
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= next_retention {
            if leader() {
                task(&stats, "retention", || retention(&conn, &schema, &locks, &s, &r));
            }
            next_retention = Instant::now() + s.retention_interval;
        } else if now >= next_full {
            if leader() {
                task(&stats, "full_compaction", || compact(&conn, &schema, None));
            }
            next_full = Instant::now() + s.full_compact_interval;
            next_compact = Instant::now() + s.compact_interval;
        } else if now >= next_compact {
            if leader() {
                task(&stats, "compaction", || compact(&conn, &schema, Some(s.small_file_size.0)));
            }
            next_compact = Instant::now() + s.compact_interval;
        } else {
            // Woken early by `stop`.
            std::thread::park_timeout(next_retention.min(next_full).min(next_compact) - now);
        }
    }
    info!("maintenance stopped");
}

/// How long a lease holds: the holder renews it at every task, so a few of the
/// shortest interval. A longer run may outlive it; that overlap is safe.
fn lease_duration(s: &MaintenanceConfig) -> Duration {
    s.compact_interval.min(s.full_compact_interval).min(s.retention_interval).saturating_mul(3)
}

/// Whether this process may run a task now. On an error it waits for the next
/// task: the lease saves work, it is not worth more than a warning.
fn holds_lease(conn: &Connection, schema: &str, holder: &str, lease_for: Duration) -> bool {
    match mallard_duck::try_lease(conn, &format!("{CATALOG}.{schema}.meta"), holder, lease_for) {
        Ok(leader) => leader,
        Err(e) => {
            metrics::counter!("mallard_otel_maintenance_errors_total", "task" => "lease").increment(1);
            warn!(error = %super::redact(&format!("{e:#}")), "maintenance lease failed");
            false
        }
    }
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

/// Maintenance once ([`crate::run_maintenance`]): compaction of
/// small files (of all files with `full`), then retention. Ignores the lease, as
/// on the Sentry side: next to a running holder it can lose a conflict, which
/// is an error here and safe to run again.
pub fn run_once(
    conn: &Connection,
    schema: &str,
    locks: &Locks,
    m: &MaintenanceConfig,
    r: &RetentionConfig,
    full: bool,
) -> Result<Vec<String>> {
    let mut done = vec![format!("compaction: {}", compact(conn, schema, Some(m.small_file_size.0))?)];
    if full {
        done.push(format!("full compaction: {}", compact(conn, schema, None)?));
    }
    done.push(format!("retention: {}", retention(conn, schema, locks, m, r)?));
    Ok(done)
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

pub fn retention(
    conn: &Connection,
    schema: &str,
    locks: &Locks,
    s: &MaintenanceConfig,
    r: &RetentionConfig,
) -> Result<String> {
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
        let _no_flush_meanwhile = locks.turn();
        let tx = conn.unchecked_transaction()?;
        deleted +=
            conn.execute(&format!("DELETE FROM {CATALOG}.{schema}.{} WHERE {} < ?", t.name, t.time_column), [cutoff])?;
        let _one_at_a_time = locks.commit();
        tx.commit()?;
    }
    let mut detail = format!("deleted {deleted} expired rows");
    // Absolute instants (epoch seconds -> TIMESTAMPTZ), so the session
    // time zone can't shift the cut-off. A delay reaching back before the
    // earliest representable time keeps everything (cut-off 1970).
    let epoch = |d: Duration| -> f64 { before(now, d).unwrap_or(DateTime::UNIX_EPOCH).timestamp_micros() as f64 / 1e6 };
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
    Ok(detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use chrono::TimeZone;
    use std::path::Path;

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
    fn a_lease_holds_for_three_of_the_shortest_interval() {
        let mut m = MaintenanceConfig::default();
        assert_eq!(lease_duration(&m), Duration::from_secs(30 * 60));
        m.retention_interval = Duration::from_secs(60);
        assert_eq!(lease_duration(&m), Duration::from_secs(3 * 60));
        m = MaintenanceConfig {
            compact_interval: Duration::MAX,
            full_compact_interval: Duration::MAX,
            retention_interval: Duration::MAX,
            ..m
        };
        assert_eq!(lease_duration(&m), Duration::MAX);
    }

    fn config_on(catalog: &str, dir: &Path) -> Config {
        let mut cfg = Config::default();
        cfg.storage.catalog = catalog.to_string();
        cfg.storage.data_path = format!("{}/", dir.join("lake").display());
        let every = Duration::from_millis(100);
        cfg.maintenance.compact_interval = every;
        cfg.maintenance.full_compact_interval = every;
        cfg.maintenance.retention_interval = every;
        cfg
    }

    fn lease_row(lake: &Lake) -> String {
        let conn = lake.connect().unwrap();
        conn.query_row("SELECT value FROM meta WHERE key = ?", [mallard_duck::LEASE], |r| r.get(0)).unwrap()
    }

    /// Issue 0051: while another process holds the lease this one runs no task,
    /// but the one-shot command works; when the lease runs out this one takes over.
    fn waits_for_the_lease_of_another_process(catalog: &str, dir: &Path) {
        let cfg = config_on(catalog, dir);
        let (other, this) = (Lake::open(&cfg).unwrap(), Lake::open(&cfg).unwrap());
        let other_conn = other.connect().unwrap();
        let lease = |d| mallard_duck::try_lease(&other_conn, "lake.otel.meta", "other", d).unwrap();
        assert!(lease(Duration::from_secs(600)));

        let stats = Arc::new(Stats::default());
        let m = Maintenance::start(&this, cfg.maintenance.clone(), cfg.retention.clone(), stats.clone()).unwrap();
        std::thread::sleep(Duration::from_secs(1));
        assert!(stats.last_maintenance().is_none(), "ran a task under another's lease");

        let done = crate::run_maintenance(&cfg, false).unwrap();
        assert!(done.iter().any(|d| d.starts_with("retention")), "{done:?}");
        assert!(lease_row(&other).starts_with("other "), "the one-shot run leaves the lease alone");

        assert!(lease(Duration::from_millis(1)), "the holder renews, briefly");
        for _ in 0..300 {
            if stats.last_maintenance().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        drop(m);
        assert!(stats.last_maintenance().is_some(), "never took over");
        assert!(!lease_row(&other).starts_with("other "), "this process holds it now");
    }

    #[test]
    fn waits_for_the_lease_on_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = format!("sqlite:{}", dir.path().join("catalog.sqlite").display());
        waits_for_the_lease_of_another_process(&catalog, dir.path());
    }

    /// Needs an empty Postgres database of its own, e.g.
    /// `MALLARD_TEST_POSTGRES='host=127.0.0.1 port=5432 dbname=lake user=postgres password=pw'`.
    #[test]
    #[ignore]
    fn waits_for_the_lease_on_postgres() {
        let dir = tempfile::tempdir().unwrap();
        let libpq = std::env::var("MALLARD_TEST_POSTGRES").expect("MALLARD_TEST_POSTGRES");
        waits_for_the_lease_of_another_process(&format!("postgres:{libpq}"), dir.path());
    }

    /// The very first lease of a catalog has one winner too: the row exists
    /// from the start, so both `DELETE`s remove it and the second commit loses.
    #[test]
    fn the_first_lease_has_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config_on(&format!("sqlite:{}", dir.path().join("catalog.sqlite").display()), dir.path());
        let (a, b) = (Lake::open(&cfg).unwrap(), Lake::open(&cfg).unwrap());
        assert_eq!(lease_row(&a), "", "one row, however many processes started");
        let claim = |c: &Connection, who: &str| {
            c.execute_batch("BEGIN TRANSACTION").unwrap();
            c.execute("DELETE FROM meta WHERE key = ?", [mallard_duck::LEASE]).unwrap();
            c.execute("INSERT INTO meta VALUES (?, ?, now()::TIMESTAMP)", [mallard_duck::LEASE, who]).unwrap();
        };
        let (ca, cb) = (a.connect().unwrap(), b.connect().unwrap());
        claim(&ca, "a 0");
        claim(&cb, "b 0");
        let results = [ca.execute_batch("COMMIT"), cb.execute_batch("COMMIT")];
        assert_eq!(results.iter().filter(|r| r.is_err()).count(), 1, "{results:?}");
    }

    #[test]
    fn huge_retention_keeps_everything_instead_of_panicking() {
        let now = Utc.with_ymd_and_hms(2026, 10, 31, 13, 45, 0).unwrap();
        let huge = humantime::parse_duration("300000y").unwrap();
        assert_eq!(retention_cutoff(now, huge), None);
        assert_eq!(retention_cutoff(now, Duration::MAX), None);
    }
}
