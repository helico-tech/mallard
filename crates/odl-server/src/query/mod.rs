//! Read side: a small pool of DuckDB connections plus the query functions
//! behind the HTTP API. All user input reaches SQL as bound parameters; only
//! fixed identifiers are formatted into SQL text.

pub mod logs;
pub mod metrics;
pub mod sql;
pub mod storage;
pub mod traces;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use duckdb::InterruptHandle;
use duckdb::{Connection, types::Value};
use odl_api::TimeWindow;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::parse_duration;

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("query exceeded the {0:?} time limit")]
    Timeout(Duration),
    #[error("too many concurrent queries")]
    Busy,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<duckdb::Error> for QueryError {
    fn from(e: duckdb::Error) -> Self {
        QueryError::Internal(e.into())
    }
}

pub type QResult<T> = Result<T, QueryError>;

pub fn bad(msg: impl Into<String>) -> QueryError {
    QueryError::BadRequest(msg.into())
}

/// Fixed-size pool of read connections.
///
/// A connection and its semaphore permit always travel together inside the
/// blocking task and are returned by a drop guard, so neither a panic in the
/// query code nor a cancelled request (client disconnect) can shrink the pool.
pub struct Reader {
    conns: Arc<Mutex<Vec<Connection>>>,
    sem: Arc<Semaphore>,
    timeout: Duration,
    pub max_window: Duration,
}

/// Returns the connection to the pool when dropped (also during unwinding).
struct Pooled {
    conn: Option<Connection>,
    pool: Arc<Mutex<Vec<Connection>>>,
    _permit: OwnedSemaphorePermit,
}

impl Drop for Pooled {
    fn drop(&mut self) {
        if let Some(c) = self.conn.take() {
            self.pool.lock().unwrap_or_else(|p| p.into_inner()).push(c);
        }
    }
}

/// Keeps interrupting a running query until it finishes. Started when the
/// request times out or is cancelled: DuckDB only interrupts the statement
/// that is running, so a multi-statement query function needs repeated
/// interrupts.
/// `done` and the interrupt share one lock, so an interrupt can never hit
/// the next query that reuses the connection.
fn keep_interrupting(handle: Arc<InterruptHandle>, done: Arc<Mutex<bool>>) {
    tokio::spawn(async move {
        loop {
            {
                let d = done.lock().unwrap_or_else(|p| p.into_inner());
                if *d {
                    return;
                }
                handle.interrupt();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
}

fn mark_done(done: &Mutex<bool>) {
    *done.lock().unwrap_or_else(|p| p.into_inner()) = true;
}

fn is_done(done: &Mutex<bool>) -> bool {
    *done.lock().unwrap_or_else(|p| p.into_inner())
}

/// Interrupts the query if the request future is dropped before it finished.
struct CancelGuard {
    handle: Arc<InterruptHandle>,
    done: Arc<Mutex<bool>>,
    armed: bool,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if self.armed && !is_done(&self.done) {
            keep_interrupting(self.handle.clone(), self.done.clone());
        }
    }
}

impl Reader {
    pub fn new(conns: Vec<Connection>, timeout: Duration, max_window: Duration) -> Self {
        let n = conns.len();
        Self { conns: Arc::new(Mutex::new(conns)), sem: Arc::new(Semaphore::new(n)), timeout, max_window }
    }

    /// Run `f` on a pooled connection in a blocking thread, with a timeout.
    /// On timeout or cancellation the query is interrupted; the connection
    /// returns to the pool when the blocking work has actually stopped.
    pub async fn run<T, F>(&self, f: F) -> QResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> QResult<T> + Send + 'static,
    {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let permit = tokio::time::timeout_at(deadline, self.sem.clone().acquire_owned())
            .await
            .map_err(|_| QueryError::Busy)?
            .map_err(|_| QueryError::Busy)?;
        let conn = self.conns.lock().unwrap_or_else(|p| p.into_inner()).pop().ok_or(QueryError::Busy)?;
        let handle = conn.interrupt_handle();
        let done = Arc::new(Mutex::new(false));
        let mut guard = CancelGuard { handle: handle.clone(), done: done.clone(), armed: true };
        let pooled = Pooled { conn: Some(conn), pool: self.conns.clone(), _permit: permit };
        let done2 = done.clone();
        let mut task = tokio::task::spawn_blocking(move || {
            let pooled = pooled;
            // Mark done even if `f` panics, before the connection goes back.
            struct Done(Arc<Mutex<bool>>);
            impl Drop for Done {
                fn drop(&mut self) {
                    mark_done(&self.0);
                }
            }
            let r = {
                let _d = Done(done2);
                f(pooled.conn.as_ref().expect("connection present"))
            };
            drop(pooled);
            r
        });
        let res = match tokio::time::timeout_at(deadline, &mut task).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "query task failed");
                Err(QueryError::Internal(anyhow::anyhow!("query task failed")))
            }
            Err(_) => {
                // The task keeps the connection until it stops; it is
                // returned to the pool then.
                keep_interrupting(handle, done.clone());
                Err(QueryError::Timeout(self.timeout))
            }
        };
        guard.armed = false;
        res
    }
}

/// A resolved time window in UTC.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

impl Window {
    pub fn resolve(w: &TimeWindow, default: Duration, max: Duration) -> QResult<Self> {
        let to = w.to.unwrap_or_else(Utc::now);
        let from = match (w.from, w.since.as_deref()) {
            (Some(f), _) => f,
            (None, Some(s)) => {
                let d = parse_duration(s).map_err(|e| bad(format!("invalid since={s:?}: {e}")))?;
                to - TimeDelta::from_std(d).map_err(|_| bad("since is too large"))?
            }
            (None, None) => to - TimeDelta::from_std(default).unwrap(),
        };
        if from >= to {
            return Err(bad("`from` must be before `to`"));
        }
        if (to - from).to_std().unwrap_or(Duration::MAX) > max {
            return Err(bad(format!("time window larger than query.max_window ({})", humantime::format_duration(max))));
        }
        Ok(Self { from, to })
    }

    pub fn secs(&self) -> f64 {
        (self.to - self.from).num_microseconds().unwrap_or(i64::MAX) as f64 / 1e6
    }

    pub fn from_param(&self) -> Value {
        ts_value(self.from.naive_utc())
    }

    pub fn to_param(&self) -> Value {
        ts_value(self.to.naive_utc())
    }

    /// Bucket width in seconds: explicit `step`, else ~60 buckets on a nice boundary.
    pub fn step(&self, step: Option<&str>) -> QResult<u64> {
        let secs = self.secs();
        let s = match step {
            Some(s) => {
                let d = parse_duration(s).map_err(|e| bad(format!("invalid step={s:?}: {e}")))?;
                // At most one bucket for the whole window (also bounds all
                // arithmetic on the step below).
                d.as_secs().clamp(1, (secs.ceil() as u64).clamp(1, 366 * 86400))
            }
            None => {
                const NICE: [u64; 17] =
                    [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 10800, 21600, 86400];
                let raw = (secs / 60.0).ceil() as u64;
                *NICE.iter().find(|n| **n >= raw).unwrap_or(&86400)
            }
        };
        if secs / s as f64 > 5000.0 {
            return Err(bad("step too small: more than 5000 buckets"));
        }
        Ok(s)
    }

    /// All bucket starts (epoch seconds) for a step.
    pub fn buckets(&self, step: u64) -> Vec<i64> {
        let step = step as i64;
        let first = self.from.timestamp().div_euclid(step) * step;
        let last = (self.to.timestamp() - 1).div_euclid(step) * step;
        (0..=((last - first) / step).max(0)).map(|i| first + i * step).collect()
    }
}

pub fn ts_value(t: NaiveDateTime) -> Value {
    Value::Timestamp(duckdb::types::TimeUnit::Microsecond, t.and_utc().timestamp_micros())
}

pub fn from_micros(us: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(us).unwrap_or_default()
}

/// Validate a 32-char hex trace id; returns it lowercased.
pub fn parse_trace_id(s: &str) -> QResult<String> {
    let s = s.trim().to_ascii_lowercase();
    if s.len() != 32 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(bad("trace id must be 32 hex characters"));
    }
    Ok(s)
}

/// SQL expression: UUID column -> 32 hex chars.
pub fn trace_hex(col: &str) -> String {
    format!("replace(CAST({col} AS VARCHAR), '-', '')")
}

/// SQL expression: UBIGINT span id -> 16 hex chars.
pub fn span_hex(col: &str) -> String {
    format!("lpad(lower(to_hex({col})), 16, '0')")
}

/// WHERE clause builder with positional parameters.
#[derive(Default)]
pub struct Filter {
    pub clauses: Vec<String>,
    pub params: Vec<Value>,
}

impl Filter {
    pub fn add(&mut self, clause: impl Into<String>, params: impl IntoIterator<Item = Value>) {
        self.clauses.push(clause.into());
        self.params.extend(params);
    }

    pub fn window(&mut self, col: &str, w: &Window) {
        self.add(format!("{col} >= ? AND {col} < ?"), [w.from_param(), w.to_param()]);
    }

    /// `key=value` filters on span/log/metric attributes or resource attributes.
    pub fn attributes(&mut self, spec: Option<&str>) -> QResult<()> {
        if let Some(spec) = spec {
            for (k, v) in odl_api::parse_attribute_filter(spec).map_err(bad)? {
                self.add(
                    "(attributes[?] = ? OR resource_attributes[?] = ?)",
                    [Value::Text(k.clone()), Value::Text(v.clone()), Value::Text(k), Value::Text(v)],
                );
            }
        }
        Ok(())
    }

    pub fn sql(&self) -> String {
        if self.clauses.is_empty() { "TRUE".into() } else { self.clauses.join(" AND ") }
    }
}

/// Parse a JSON column produced by `to_json(...)` into a value.
pub fn json_col<T: serde::de::DeserializeOwned + Default>(s: Option<String>) -> T {
    s.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn windows() {
        let w = TimeWindow { since: Some("1h".into()), ..Default::default() };
        let r = Window::resolve(&w, Duration::from_secs(60), Duration::from_secs(7200)).unwrap();
        assert!((r.secs() - 3600.0).abs() < 1.0);
        assert_eq!(r.step(None).unwrap(), 60);
        let too_big = TimeWindow { since: Some("3h".into()), ..Default::default() };
        assert!(Window::resolve(&too_big, Duration::from_secs(60), Duration::from_secs(7200)).is_err());
        assert!(r.step(Some("100ms")).is_ok());
        assert!(r.step(Some("0s")).is_ok());
    }

    #[test]
    fn buckets_cover_window() {
        let w = Window {
            from: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 30).unwrap(),
            to: Utc.with_ymd_and_hms(2026, 1, 1, 0, 3, 0).unwrap(),
        };
        let b = w.buckets(60);
        assert_eq!(b.len(), 3);
        assert_eq!(b[0] % 60, 0);
    }

    #[test]
    fn trace_ids() {
        assert!(parse_trace_id("0AF7651916CD43DD8448EB211C80319C").is_ok());
        assert!(parse_trace_id("xyz").is_err());
    }
}
