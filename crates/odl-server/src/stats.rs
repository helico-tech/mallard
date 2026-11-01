//! Process-local counters exposed by `GET /api/v1/stats`, mirrored as
//! Prometheus metrics (`odl_*`, through the `metrics` facade; the binary
//! installs the recorder).

use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering::Relaxed},
};

use chrono::Utc;
use odl_api::{FlushInfo, IngestStats, MaintenanceInfo, SignalIngestStats};

use crate::store::schema::Signal;

#[derive(Default)]
struct SignalCounters {
    requests: AtomicU64,
    accepted: AtomicU64,
    throttled: AtomicU64,
    throttled_requests: AtomicU64,
    invalid: AtomicU64,
}

#[derive(Default)]
pub struct Stats {
    traces: SignalCounters,
    logs: SignalCounters,
    metrics: SignalCounters,
    buffered_rows: AtomicU64,
    buffered_bytes: AtomicU64,
    flushes: AtomicU64,
    flush_errors: AtomicU64,
    dropped_rows: AtomicU64,
    last_flush: Mutex<Option<FlushInfo>>,
    last_flush_error: Mutex<Option<String>>,
    last_maintenance: Mutex<Option<MaintenanceInfo>>,
}

impl Stats {
    fn signal(&self, s: Signal) -> &SignalCounters {
        match s {
            Signal::Traces => &self.traces,
            Signal::Logs => &self.logs,
            Signal::Metrics => &self.metrics,
        }
    }

    /// One request: items `accepted`, `throttled` (back-pressure; the client
    /// retries) and `invalid` (refused for good).
    pub fn record_request(&self, s: Signal, accepted: u64, throttled: u64, invalid: u64) {
        let c = self.signal(s);
        c.requests.fetch_add(1, Relaxed);
        c.accepted.fetch_add(accepted, Relaxed);
        c.throttled.fetch_add(throttled, Relaxed);
        c.invalid.fetch_add(invalid, Relaxed);
        let signal = s.as_str();
        metrics::counter!("odl_ingest_requests_total", "signal" => signal).increment(1);
        metrics::counter!("odl_ingest_items_total", "signal" => signal).increment(accepted);
        metrics::counter!("odl_ingest_rejected_items_total", "signal" => signal, "reason" => "throttled")
            .increment(throttled);
        metrics::counter!("odl_ingest_rejected_items_total", "signal" => signal, "reason" => "invalid")
            .increment(invalid);
    }

    /// One request refused with back-pressure (HTTP 429 / gRPC
    /// UNAVAILABLE), in addition to [`Stats::record_request`].
    pub fn throttled_request(&self, s: Signal) {
        self.signal(s).throttled_requests.fetch_add(1, Relaxed);
        metrics::counter!("odl_ingest_throttled_requests_total", "signal" => s.as_str()).increment(1);
    }

    pub fn set_buffered(&self, rows: u64, bytes: u64) {
        self.buffered_rows.store(rows, Relaxed);
        self.buffered_bytes.store(bytes, Relaxed);
        metrics::gauge!("odl_buffered_rows").set(rows as f64);
        metrics::gauge!("odl_buffered_bytes").set(bytes as f64);
    }

    pub fn flush_ok(&self, rows: u64, duration_ms: f64) {
        self.flushes.fetch_add(1, Relaxed);
        metrics::counter!("odl_flushes_total").increment(1);
        metrics::histogram!("odl_flush_seconds").record(duration_ms / 1000.0);
        metrics::histogram!("odl_flush_rows").record(rows as f64);
        // A success clears the alarm; `flush_errors` keeps the count.
        *self.last_flush_error.lock().unwrap() = None;
        *self.last_flush.lock().unwrap() = Some(FlushInfo { at: Utc::now(), rows, duration_ms });
    }

    pub fn flush_err(&self, msg: String) {
        self.flush_errors.fetch_add(1, Relaxed);
        metrics::counter!("odl_flush_errors_total").increment(1);
        *self.last_flush_error.lock().unwrap() = Some(msg);
    }

    /// Rows of `table` that could not be written and were dropped.
    pub fn dropped(&self, table: &'static str, rows: u64) {
        self.dropped_rows.fetch_add(rows, Relaxed);
        metrics::counter!("odl_dropped_rows_total", "table" => table).increment(rows);
    }

    pub fn maintenance(&self, info: MaintenanceInfo) {
        let task = info.task.clone();
        metrics::histogram!("odl_maintenance_seconds", "task" => task.clone()).record(info.duration_ms / 1000.0);
        if !info.ok {
            metrics::counter!("odl_maintenance_errors_total", "task" => task).increment(1);
        }
        *self.last_maintenance.lock().unwrap() = Some(info);
    }

    /// The error of the last flush, when it failed (cleared by a success).
    pub fn last_flush_error(&self) -> Option<String> {
        self.last_flush_error.lock().unwrap().clone()
    }

    pub fn last_maintenance(&self) -> Option<MaintenanceInfo> {
        self.last_maintenance.lock().unwrap().clone()
    }

    pub fn ingest(&self, max_buffer_bytes: u64) -> IngestStats {
        let sig = |s: Signal| {
            let c = self.signal(s);
            SignalIngestStats {
                signal: s,
                requests: c.requests.load(Relaxed),
                accepted: c.accepted.load(Relaxed),
                throttled: c.throttled.load(Relaxed),
                throttled_requests: c.throttled_requests.load(Relaxed),
                invalid: c.invalid.load(Relaxed),
            }
        };
        IngestStats {
            signals: [Signal::Traces, Signal::Logs, Signal::Metrics].map(sig).to_vec(),
            buffered_rows: self.buffered_rows.load(Relaxed),
            buffered_bytes: self.buffered_bytes.load(Relaxed),
            max_buffer_bytes,
            flushes: self.flushes.load(Relaxed),
            flush_errors: self.flush_errors.load(Relaxed),
            dropped_rows: self.dropped_rows.load(Relaxed),
            last_flush: self.last_flush.lock().unwrap().clone(),
            last_flush_error: self.last_flush_error.lock().unwrap().clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_reach_the_metrics_recorder() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            let s = Stats::default();
            s.record_request(Signal::Logs, 3, 2, 1);
            s.throttled_request(Signal::Logs);
            s.flush_ok(4, 12.0);
            s.flush_err("boom".into());
            s.set_buffered(5, 500);
        });
        let text = handle.render();
        for line in [
            "odl_ingest_requests_total{signal=\"logs\"} 1",
            "odl_ingest_items_total{signal=\"logs\"} 3",
            "odl_ingest_rejected_items_total{signal=\"logs\",reason=\"throttled\"} 2",
            "odl_ingest_rejected_items_total{signal=\"logs\",reason=\"invalid\"} 1",
            "odl_ingest_throttled_requests_total{signal=\"logs\"} 1",
            "odl_flush_errors_total 1",
            "odl_buffered_rows 5",
        ] {
            assert!(text.contains(line), "{line} missing in\n{text}");
        }
        assert!(text.contains("odl_flush_seconds"), "{text}");
    }
}
