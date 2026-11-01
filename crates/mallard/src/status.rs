//! `mallard status`: is each side up, and what does the server allow?
//! Exit code 0 when both sides answer, 3 for a wrong token, 6 otherwise.

use anyhow::Result;
use mallard_client::mallard_otel_client::api::Stats;
use mallard_client::mallard_sentry_client::types::ServerInfo;
use mallard_term::error::{Described, Kind};
use mallard_term::text::clean;
use serde::Serialize;

use crate::{ERRORS_HINTS, Run, TELEMETRY_HINTS};

/// The JSON document of `mallard status`.
#[derive(Debug, Serialize)]
pub(crate) struct Status {
    /// Both sides answered.
    pub ok: bool,
    pub url: String,
    pub sentry: Side<ServerInfo>,
    pub otel: Side<Stats>,
    pub capabilities: Capabilities,
}

#[derive(Debug, Serialize)]
pub(crate) struct Side<T> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<mallard_term::error::ErrorBody>,
}

/// What a client should know before it queries or sends data. `None` when
/// the side that knows it is down (or older than these fields).
#[derive(Debug, Default, Serialize, PartialEq)]
pub(crate) struct Capabilities {
    /// `mallard sql` works.
    pub sql: Option<bool>,
    /// `mallard sql --errors` works.
    pub sql_errors: Option<bool>,
    /// Longest `--since`/`--from` window per side.
    pub max_window_secs: Windows,
    /// How long data is kept (0 = forever).
    pub retention_secs: Retention,
    /// New telemetry can take this long to show up in queries.
    pub ingest_lag_secs: Option<f64>,
    /// OTLP ingest needs a token (`OTEL_EXPORTER_OTLP_HEADERS`).
    pub ingest_auth: Option<bool>,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub(crate) struct Windows {
    pub errors: Option<u64>,
    pub telemetry: Option<u64>,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub(crate) struct Retention {
    pub errors: Option<u64>,
    pub traces: Option<u64>,
    pub logs: Option<u64>,
    pub metrics: Option<u64>,
}

fn capabilities(s: Option<&ServerInfo>, o: Option<&Stats>) -> Capabilities {
    let oc = o.and_then(|o| o.capabilities.as_ref());
    Capabilities {
        sql: oc.map(|c| c.sql),
        sql_errors: s.and_then(|s| s.sql),
        max_window_secs: Windows {
            errors: s.and_then(|s| s.max_window_secs),
            telemetry: oc.map(|c| c.max_window_secs),
        },
        retention_secs: Retention {
            errors: s.and_then(|s| s.retention_secs),
            traces: oc.map(|c| c.retention_secs.traces),
            logs: oc.map(|c| c.retention_secs.logs),
            metrics: oc.map(|c| c.retention_secs.metrics),
        },
        ingest_lag_secs: oc.map(|c| c.flush_interval_secs),
        ingest_auth: oc.map(|c| c.ingest_auth),
    }
}

pub(crate) async fn run(r: &Run) -> Result<()> {
    let c = &r.client;
    let (s, o) = tokio::join!(c.sentry.info(), c.otel.stats());
    let s = s.map_err(|e| mallard_sentry_cli::classify(&e, c.sentry.base_url(), &ERRORS_HINTS));
    let o = o.map_err(|e| mallard_otel_cli::classify(&e, c.otel.base_url(), &TELEMETRY_HINTS));
    let caps = capabilities(s.as_ref().ok(), o.as_ref().ok());
    // The command fails when a side does: a wrong token wins (exit 3),
    // else "unavailable" (exit 6).
    let kinds: Vec<Kind> = [s.as_ref().err(), o.as_ref().err()].into_iter().flatten().map(|d| d.kind).collect();
    let failure = match () {
        _ if kinds.is_empty() => None,
        _ if kinds.contains(&Kind::Auth) => Some(Kind::Auth),
        _ => Some(Kind::Unavailable),
    };
    if r.output.text() {
        r.output.out.line(&status_text(c.base_url(), &s, &o, &caps))?;
    } else {
        fn side<T>(r: Result<T, Described>) -> Side<T> {
            match r {
                Ok(v) => Side { ok: true, info: Some(v), error: None },
                Err(d) => Side { ok: false, info: None, error: Some(d.body()) },
            }
        }
        let st = Status {
            ok: failure.is_none(),
            url: c.base_url().to_string(),
            sentry: side(s),
            otel: side(o),
            capabilities: caps,
        };
        r.output.object(&st)?;
    }
    match failure {
        None => Ok(()),
        Some(kind) => {
            let what = if kind == Kind::Auth { "token rejected" } else { "a side is down or not ready" };
            Err(anyhow::Error::new(Described::new(kind, format!("status: {what}"))))
        }
    }
}

/// The text of `mallard status`: one line per side, server text cleaned.
fn status_text(
    base_url: &str,
    s: &Result<ServerInfo, Described>,
    o: &Result<Stats, Described>,
    caps: &Capabilities,
) -> String {
    use mallard_term::fmt::{bytes, compact};
    let mut text = format!("server    {}\n", clean(base_url));
    let error = |e: &Described| format!("{} {}", mallard_term::color::red("ERROR"), clean(&e.to_string()));
    match s {
        Ok(i) => text.push_str(&format!(
            "errors    ok  v{}  duckdb {}  queue {}/{}\n",
            clean(&i.version),
            clean(&i.duckdb_version),
            i.queue_depth,
            i.queue_capacity
        )),
        Err(e) => text.push_str(&format!("errors    {}\n", error(e))),
    }
    match o {
        Ok(st) => {
            let rows: i64 = st.storage.tables.iter().map(|t| t.rows + t.inlined_rows).sum();
            let size: i64 = st.storage.tables.iter().map(|t| t.bytes).sum();
            text.push_str(&format!(
                "telemetry ok  v{}  up {}s  buffered {} rows  stored {} rows / {}  flush errors {}",
                clean(&st.version),
                st.uptime_secs,
                compact(st.ingest.buffered_rows as f64),
                compact(rows as f64),
                bytes(size.max(0) as u64),
                st.ingest.flush_errors
            ));
        }
        Err(e) => text.push_str(&format!("telemetry {}", error(e))),
    }
    if let Some(sql) = caps.sql {
        let secs = |s: Option<u64>| match s {
            Some(0) => "forever".to_string(),
            Some(s) => humantime_short(s),
            None => "-".into(),
        };
        text.push_str(&format!(
            "\nlimits    sql {}  sql --errors {}  max window {}  keep errors {} traces {} logs {} metrics {}",
            if sql { "on" } else { "off" },
            match caps.sql_errors {
                Some(true) => "on",
                Some(false) => "off",
                None => "-",
            },
            secs(caps.max_window_secs.telemetry),
            secs(caps.retention_secs.errors),
            secs(caps.retention_secs.traces),
            secs(caps.retention_secs.logs),
            secs(caps.retention_secs.metrics),
        ));
    }
    text
}

/// `7d`, `12h`, `90s`: the largest whole unit.
fn humantime_short(secs: u64) -> String {
    for (unit, n) in [("d", 86_400), ("h", 3600), ("m", 60)] {
        if secs >= n && secs.is_multiple_of(n) {
            return format!("{}{unit}", secs / n);
        }
    }
    format!("{secs}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_names_each_side_once_and_cleans_server_text() {
        let down = Described::new(Kind::Unavailable, "cannot reach server at http://h/sentry")
            .hint("is it running? set MALLARD_URL");
        let text = status_text(
            "http://h",
            &Err(down),
            &Err(Described::new(Kind::Internal, "down\x1b[2J")),
            &Capabilities::default(),
        );
        let plain = mallard_term::color::strip_ansi(&text);
        assert_eq!(
            plain,
            "server    http://h\nerrors    ERROR cannot reach server at http://h/sentry (is it running? set MALLARD_URL)\ntelemetry ERROR down\\u{1b}[2J"
        );
    }

    #[test]
    fn short_durations() {
        assert_eq!(humantime_short(7 * 86_400), "7d");
        assert_eq!(humantime_short(36 * 3600), "36h");
        assert_eq!(humantime_short(90), "90s");
    }

    #[test]
    fn capabilities_from_missing_sides_are_null() {
        let c = capabilities(None, None);
        assert_eq!(c, Capabilities::default());
        let v = serde_json::to_value(&c).unwrap();
        assert!(v["sql"].is_null() && v["retention_secs"]["traces"].is_null());
    }
}
