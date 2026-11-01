//! Read side: a small pool of DuckDB connections plus the query functions
//! behind the HTTP API. All user input reaches SQL as bound parameters; only
//! fixed identifiers are formatted into SQL text.

pub mod attributes;
pub mod exemplars;
pub mod logs;
pub mod metrics;
pub mod sql;
pub mod storage;
pub mod traces;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{DateTime, NaiveDateTime, Utc};
pub use mallard_common::window::Window;
use mallard_common::window::WindowError;
use mallard_duck::duckdb::{self, Connection, InterruptHandle, types::Value};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

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

impl From<WindowError> for QueryError {
    fn from(e: WindowError) -> Self {
        QueryError::BadRequest(e.to_string())
    }
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

pub fn ts_value(t: NaiveDateTime) -> Value {
    Value::Timestamp(duckdb::types::TimeUnit::Microsecond, t.and_utc().timestamp_micros())
}

/// A UTC time as a SQL parameter (e.g. a window bound).
pub fn ts(t: DateTime<Utc>) -> Value {
    ts_value(t.naive_utc())
}

/// Epoch microseconds from DuckDB. Out of range (corrupt data) is logged
/// and shown as 1970-01-01.
pub fn from_micros(us: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(us).unwrap_or_else(|| {
        tracing::warn!(us, "timestamp out of range in query result");
        DateTime::UNIX_EPOCH
    })
}

/// Validate a 32-char hex trace id; returns it lowercased.
pub fn parse_trace_id(s: &str) -> QResult<String> {
    let s = s.trim().to_ascii_lowercase();
    if s.len() != 32 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(bad("trace id must be 32 hex characters"));
    }
    Ok(s)
}

/// Validate a 16-char hex span id; returns it as the stored UBIGINT.
pub fn parse_span_id(s: &str) -> QResult<u64> {
    let s = s.trim();
    if s.len() != 16 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(bad("span id must be 16 hex characters"));
    }
    u64::from_str_radix(s, 16).map_err(|_| bad("span id must be 16 hex characters"))
}

/// A paging cursor: `"<time in epoch micros>:<id>"`, where the id breaks
/// ties between rows of the same time (a trace id, a row id). Clients treat
/// it as opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub us: i64,
    pub id: String,
}

impl Cursor {
    pub fn parse(s: &str) -> QResult<Cursor> {
        let invalid = || bad("invalid cursor; pass the x-next-cursor header of the previous page");
        let (us, id) = s.split_once(':').ok_or_else(invalid)?;
        let us = us.parse().map_err(|_| invalid())?;
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        Ok(Cursor { us, id: id.to_ascii_lowercase() })
    }

    pub fn time(&self) -> Value {
        Value::Timestamp(duckdb::types::TimeUnit::Microsecond, self.us)
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.us, self.id)
    }
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
        self.add(format!("{col} >= ? AND {col} < ?"), [ts(w.from), ts(w.to)]);
    }

    /// `key=value` filters on span/log/metric attributes or resource attributes.
    pub fn attributes(&mut self, spec: Option<&str>) -> QResult<()> {
        if let Some(spec) = spec {
            for (k, v) in odl_api::parse_attribute_filter(spec).map_err(|e| bad(e.to_string()))? {
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

/// Read a text column holding one value of a closed set (e.g. a span kind).
/// NULL reads as the empty string. An unknown value is an error.
pub fn enum_col<T: std::str::FromStr<Err = odl_api::UnknownValue>>(r: &duckdb::Row, i: usize) -> duckdb::Result<T> {
    let s: Option<String> = r.get(i)?;
    s.unwrap_or_default()
        .parse()
        .map_err(|e| duckdb::Error::FromSqlConversionFailure(i, duckdb::types::Type::Text, Box::new(e)))
}

/// Parse a JSON column produced by `to_json(...)` into a value. NULL is the
/// default value; invalid JSON (corrupt data) is logged and also the default.
pub fn json_col<T: serde::de::DeserializeOwned + Default>(s: Option<String>) -> T {
    json_str(s.as_deref())
}

/// [`json_col`] on a borrowed string.
pub fn json_str<T: serde::de::DeserializeOwned + Default>(s: Option<&str>) -> T {
    let Some(s) = s else { return T::default() };
    serde_json::from_str(s).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "unexpected JSON in query result");
        T::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_ids() {
        assert!(parse_trace_id("0AF7651916CD43DD8448EB211C80319C").is_ok());
        assert!(parse_trace_id("xyz").is_err());
    }

    #[test]
    fn span_ids() {
        assert_eq!(parse_span_id("ff00000000000002").unwrap(), 0xff00_0000_0000_0002);
        assert!(parse_span_id("ff0000000000002").is_err(), "15 chars");
        assert!(parse_span_id("+f00000000000002").is_err(), "sign");
        assert!(parse_span_id("zz00000000000002").is_err());
    }

    #[test]
    fn cursors() {
        let c = Cursor::parse("1730000000000000:0AF7").unwrap();
        assert_eq!(c, Cursor { us: 1_730_000_000_000_000, id: "0af7".into() });
        assert_eq!(Cursor::parse(&c.to_string()).unwrap(), c);
        for bad in ["", "1", "x:ab", "1:", "1:zz", "1:a'b"] {
            assert!(Cursor::parse(bad).is_err(), "{bad}");
        }
    }
}
