//! Process-local counters exposed by `GET /api/v1/stats`.

use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering::Relaxed},
};

use chrono::Utc;
use odl_api::{FlushInfo, IngestStats, MaintenanceInfo, SignalIngestStats};

use crate::store::schema::Signal;

#[derive(Default)]
pub struct SignalCounters {
    pub requests: AtomicU64,
    pub accepted: AtomicU64,
    pub rejected: AtomicU64,
}

#[derive(Default)]
pub struct Stats {
    pub traces: SignalCounters,
    pub logs: SignalCounters,
    pub metrics: SignalCounters,
    buffered_rows: AtomicU64,
    buffered_bytes: AtomicU64,
    flushes: AtomicU64,
    flush_errors: AtomicU64,
    last_flush: Mutex<Option<FlushInfo>>,
    last_flush_error: Mutex<Option<String>>,
    last_maintenance: Mutex<Option<MaintenanceInfo>>,
}

impl Stats {
    pub fn signal(&self, s: Signal) -> &SignalCounters {
        match s {
            Signal::Traces => &self.traces,
            Signal::Logs => &self.logs,
            Signal::Metrics => &self.metrics,
        }
    }

    pub fn record_request(&self, s: Signal, accepted: u64, rejected: u64) {
        let c = self.signal(s);
        c.requests.fetch_add(1, Relaxed);
        c.accepted.fetch_add(accepted, Relaxed);
        c.rejected.fetch_add(rejected, Relaxed);
    }

    pub fn set_buffered(&self, rows: u64, bytes: u64) {
        self.buffered_rows.store(rows, Relaxed);
        self.buffered_bytes.store(bytes, Relaxed);
    }

    pub fn flush_ok(&self, rows: u64, duration_ms: f64) {
        self.flushes.fetch_add(1, Relaxed);
        *self.last_flush.lock().unwrap() = Some(FlushInfo {
            at: Utc::now(),
            rows,
            duration_ms,
        });
    }

    pub fn flush_err(&self, msg: String) {
        self.flush_errors.fetch_add(1, Relaxed);
        *self.last_flush_error.lock().unwrap() = Some(msg);
    }

    pub fn maintenance(&self, info: MaintenanceInfo) {
        *self.last_maintenance.lock().unwrap() = Some(info);
    }

    pub fn last_maintenance(&self) -> Option<MaintenanceInfo> {
        self.last_maintenance.lock().unwrap().clone()
    }

    pub fn ingest(&self, max_buffer_bytes: u64) -> IngestStats {
        let sig = |name: &str, c: &SignalCounters| SignalIngestStats {
            signal: name.to_string(),
            requests: c.requests.load(Relaxed),
            accepted: c.accepted.load(Relaxed),
            rejected: c.rejected.load(Relaxed),
        };
        IngestStats {
            signals: vec![
                sig("traces", &self.traces),
                sig("logs", &self.logs),
                sig("metrics", &self.metrics),
            ],
            buffered_rows: self.buffered_rows.load(Relaxed),
            buffered_bytes: self.buffered_bytes.load(Relaxed),
            max_buffer_bytes,
            flushes: self.flushes.load(Relaxed),
            flush_errors: self.flush_errors.load(Relaxed),
            last_flush: self.last_flush.lock().unwrap().clone(),
            last_flush_error: self.last_flush_error.lock().unwrap().clone(),
        }
    }
}
