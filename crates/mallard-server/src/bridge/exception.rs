//! OTel exceptions → Sentry events. The mapping table is in
//! docs/adr/0009-bridge.md.
//!
//! An exception is a span event named `exception` or a log record with an
//! `exception.type` or `exception.message` attribute (OTel semantic
//! conventions).

use opentelemetry_proto::tonic::{
    collector::{logs::v1::ExportLogsServiceRequest, trace::v1::ExportTraceServiceRequest},
    common::v1::{AnyValue, KeyValue, any_value},
    resource::v1::Resource,
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::stacktrace;

/// Where an exception was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    SpanEvent,
    Log,
}

/// One exception found in an OTLP request, owned so it can wait in a queue.
#[derive(Debug, Clone)]
pub struct OtelException {
    pub origin: Origin,
    pub trace_id: Option<Uuid>,
    pub span_id: Option<u64>,
    pub time_unix_nano: u64,
    pub exception_type: Option<String>,
    pub message: Option<String>,
    pub stacktrace: Option<String>,
    /// `exception.escaped`: it left the span (unhandled).
    pub escaped: Option<bool>,
    pub service: String,
    pub resource: Map<String, Value>,
    /// The span's or log record's attributes, without `exception.*`.
    pub attributes: Map<String, Value>,
    /// The span's name (span events).
    pub span_name: Option<String>,
    /// The instrumentation scope (logs).
    pub logger: Option<String>,
    /// The log body, when it says more than the message.
    pub body: Option<String>,
    pub level: &'static str,
}

impl OtelException {
    pub fn language(&self) -> Option<&str> {
        self.resource.get("telemetry.sdk.language").and_then(Value::as_str)
    }

    /// Sentry `platform` for the SDK language.
    pub fn platform(&self) -> &'static str {
        match self.language().unwrap_or_default() {
            "python" => "python",
            "java" => "java",
            "nodejs" => "node",
            "webjs" => "javascript",
            "go" => "go",
            "dotnet" => "csharp",
            "rust" => "rust",
            "ruby" => "ruby",
            "php" => "php",
            "erlang" => "elixir",
            "swift" => "cocoa",
            "cpp" => "native",
            _ => "other",
        }
    }
}

/// Exceptions recorded as span events.
pub fn from_traces(req: &ExportTraceServiceRequest) -> Vec<OtelException> {
    let mut out = Vec::new();
    for rs in &req.resource_spans {
        for span in rs.scope_spans.iter().flat_map(|s| &s.spans) {
            for ev in span.events.iter().filter(|e| e.name == "exception") {
                let time = if ev.time_unix_nano == 0 { span.start_time_unix_nano } else { ev.time_unix_nano };
                let ids = (trace_id(&span.trace_id), span_id(&span.span_id));
                let mut attrs = to_map(&span.attributes);
                attrs.extend(to_map(&ev.attributes));
                if let Some(mut e) = exception(Origin::SpanEvent, rs.resource.as_ref(), attrs, ids, time) {
                    e.span_name = Some(span.name.clone()).filter(|n| !n.is_empty());
                    out.push(e);
                }
            }
        }
    }
    out
}

/// Exceptions recorded as log records.
pub fn from_logs(req: &ExportLogsServiceRequest) -> Vec<OtelException> {
    let mut out = Vec::new();
    for rl in &req.resource_logs {
        for sl in &rl.scope_logs {
            for r in &sl.log_records {
                if !r.attributes.iter().any(|kv| kv.key == "exception.type" || kv.key == "exception.message") {
                    continue;
                }
                let time = if r.time_unix_nano == 0 { r.observed_time_unix_nano } else { r.time_unix_nano };
                let ids = (trace_id(&r.trace_id), span_id(&r.span_id));
                let Some(mut e) = exception(Origin::Log, rl.resource.as_ref(), to_map(&r.attributes), ids, time) else {
                    continue;
                };
                e.logger = sl.scope.as_ref().map(|s| s.name.clone()).filter(|n| !n.is_empty());
                e.level = level(r.severity_number);
                e.body = r
                    .body
                    .as_ref()
                    .map(|b| match json_of(b) {
                        Value::String(s) => s,
                        other => other.to_string(),
                    })
                    .filter(|b| !b.is_empty() && Some(b) != e.message.as_ref());
                out.push(e);
            }
        }
    }
    out
}

/// Split `exception.*` off `attrs`; `None` without a type or a message.
fn exception(
    origin: Origin,
    resource: Option<&Resource>,
    mut attrs: Map<String, Value>,
    (trace_id, span_id): (Option<Uuid>, Option<u64>),
    time_unix_nano: u64,
) -> Option<OtelException> {
    let mut take = |k: &str| match attrs.remove(k) {
        Some(Value::String(s)) if s.trim().is_empty() => None,
        Some(Value::String(s)) => Some(s),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    };
    let exception_type = take("exception.type");
    let message = take("exception.message");
    let stacktrace = take("exception.stacktrace");
    let escaped = take("exception.escaped").map(|e| e == "true");
    if exception_type.is_none() && message.is_none() {
        return None;
    }
    let resource = resource.map(|r| to_map(&r.attributes)).unwrap_or_default();
    let service = resource
        .get("service.name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown_service")
        .to_string();
    Some(OtelException {
        origin,
        trace_id,
        span_id,
        time_unix_nano,
        exception_type,
        message,
        stacktrace,
        escaped,
        service,
        resource,
        attributes: attrs,
        span_name: None,
        logger: None,
        body: None,
        level: "error",
    })
}

/// The Sentry event for an exception. Its `event_id` is derived from the
/// exception, so an OTLP retry of the same data is stored once.
pub fn to_sentry_event(e: &OtelException) -> Value {
    let frames = e.stacktrace.as_deref().map(|s| stacktrace::parse(s, e.language())).unwrap_or_default();
    let mut exc = json!({
        "type": e.exception_type.as_deref().unwrap_or("Error"),
        "mechanism": {"type": "otel"},
    });
    if let Some(m) = &e.message {
        exc["value"] = json!(m);
    }
    if let Some(escaped) = e.escaped {
        exc["mechanism"]["handled"] = json!(!escaped);
    }
    if !frames.is_empty() {
        exc["stacktrace"] = json!({ "frames": frames });
    }
    let origin = match e.origin {
        Origin::SpanEvent => "span_event",
        Origin::Log => "log",
    };
    let mut ev = json!({
        "event_id": event_id(e).simple().to_string(),
        "timestamp": e.time_unix_nano as f64 / 1e9,
        "platform": e.platform(),
        "level": e.level,
        "exception": {"values": [exc]},
        "tags": {"service.name": e.service, "otel.source": origin},
        "contexts": {"otel": {"resource": e.resource, "attributes": e.attributes}},
    });
    let res = |k: &str| e.resource.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let mut set = |k: &str, v: Option<&str>| {
        if let Some(v) = v {
            ev[k] = json!(v);
        }
    };
    set("environment", res("deployment.environment.name").or(res("deployment.environment")));
    set("release", res("service.version"));
    set("server_name", res("host.name"));
    set("transaction", e.span_name.as_deref());
    set("logger", e.logger.as_deref());
    if let Some(name) = res("telemetry.sdk.name") {
        ev["sdk"] = json!({"name": name, "version": res("telemetry.sdk.version").unwrap_or_default()});
    }
    if let Some(t) = e.trace_id {
        let mut trace = json!({"type": "trace", "trace_id": t.simple().to_string()});
        if let Some(s) = e.span_id {
            trace["span_id"] = json!(format!("{s:016x}"));
        }
        ev["contexts"]["trace"] = trace;
    }
    if let Some(body) = &e.body {
        ev["logentry"] = json!({ "formatted": body });
    }
    if frames.is_empty()
        && let Some(raw) = &e.stacktrace
    {
        ev["extra"] = json!({ "exception.stacktrace": raw });
    }
    ev
}

fn event_id(e: &OtelException) -> Uuid {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    h.update(e.trace_id.unwrap_or_default().as_bytes());
    h.update(&e.span_id.unwrap_or_default().to_be_bytes());
    h.update(&e.time_unix_nano.to_be_bytes());
    for part in [&e.exception_type, &e.message, &e.stacktrace] {
        h.update(part.as_deref().unwrap_or_default().as_bytes());
        h.update(b"\0");
    }
    h.update(&[e.origin as u8]);
    uuid::Builder::from_random_bytes(h.digest128().to_be_bytes()).into_uuid()
}

/// OTel severity number → Sentry level; unset counts as an error.
fn level(severity: i32) -> &'static str {
    match severity {
        21.. => "fatal",
        17..=20 | 0 => "error",
        13..=16 => "warning",
        9..=12 => "info",
        _ => "debug",
    }
}

fn trace_id(b: &[u8]) -> Option<Uuid> {
    Uuid::from_slice(b).ok().filter(|u| !u.is_nil())
}

fn span_id(b: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(b.try_into().ok()?)).filter(|s| *s != 0)
}

fn to_map(kvs: &[KeyValue]) -> Map<String, Value> {
    kvs.iter().map(|kv| (kv.key.clone(), kv.value.as_ref().map(json_of).unwrap_or(Value::Null))).collect()
}

fn json_of(v: &AnyValue) -> Value {
    use any_value::Value as V;
    match &v.value {
        Some(V::StringValue(s)) => json!(s),
        Some(V::BoolValue(b)) => json!(b),
        Some(V::IntValue(i)) => json!(i),
        Some(V::DoubleValue(d)) => json!(d),
        Some(V::ArrayValue(a)) => Value::Array(a.values.iter().map(json_of).collect()),
        Some(V::KvlistValue(l)) => Value::Object(to_map(&l.values)),
        Some(V::BytesValue(b)) => json!(hex::encode(b)),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mallard_sentry_protocol::{NormalizeContext, ProcessedEvent, normalize_event};
    use opentelemetry_proto::tonic::{
        logs::v1::{LogRecord, ResourceLogs, ScopeLogs},
        trace::v1::{ResourceSpans, ScopeSpans, Span, span::Event},
    };

    fn kv(k: &str, v: &str) -> KeyValue {
        KeyValue {
            key: k.into(),
            value: Some(AnyValue { value: Some(any_value::Value::StringValue(v.into())) }),
            ..Default::default()
        }
    }

    fn resource(lang: &str) -> Option<Resource> {
        Some(Resource {
            attributes: vec![
                kv("service.name", "checkout"),
                kv("service.version", "2.4.1"),
                kv("deployment.environment.name", "production"),
                kv("host.name", "web-3"),
                kv("telemetry.sdk.language", lang),
                kv("telemetry.sdk.name", "opentelemetry"),
                kv("telemetry.sdk.version", "1.27.0"),
            ],
            ..Default::default()
        })
    }

    const PY_TRACE: &str = r#"Traceback (most recent call last):
  File "/app/shop/views.py", line 41, in checkout
    order = place_order(cart)
  File "/app/shop/orders.py", line 88, in place_order
    raise ValueError(f"cart {cart.id} is empty")
ValueError: cart 7 is empty
"#;

    /// What opentelemetry-python's `span.record_exception` sends.
    fn python_span() -> ExportTraceServiceRequest {
        let event = Event {
            time_unix_nano: 1_730_450_000_500_000_000,
            name: "exception".into(),
            attributes: vec![
                kv("exception.type", "ValueError"),
                kv("exception.message", "cart 7 is empty"),
                kv("exception.stacktrace", PY_TRACE),
                kv("exception.escaped", "true"),
            ],
            ..Default::default()
        };
        let span = Span {
            trace_id: hex::decode("4bf92f3577b34da6a3ce929d0e0e4736").unwrap(),
            span_id: hex::decode("00f067aa0ba902b7").unwrap(),
            name: "POST /checkout".into(),
            attributes: vec![kv("http.route", "/checkout")],
            events: vec![event, Event { name: "cache miss".into(), ..Default::default() }],
            ..Default::default()
        };
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: resource("python"),
                scope_spans: vec![ScopeSpans { spans: vec![span], ..Default::default() }],
                ..Default::default()
            }],
        }
    }

    #[test]
    fn python_span_event_becomes_a_sentry_error() {
        let found = from_traces(&python_span());
        assert_eq!(found.len(), 1, "only events named `exception`");
        let ev = to_sentry_event(&found[0]);
        assert_eq!(ev["platform"], "python");
        assert_eq!(ev["environment"], "production");
        assert_eq!(ev["release"], "2.4.1");
        assert_eq!(ev["server_name"], "web-3");
        assert_eq!(ev["transaction"], "POST /checkout");
        assert_eq!(ev["contexts"]["trace"]["trace_id"], "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(ev["contexts"]["trace"]["span_id"], "00f067aa0ba902b7");
        assert_eq!(ev["contexts"]["otel"]["attributes"]["http.route"], "/checkout");
        assert!(ev["contexts"]["otel"]["attributes"].get("exception.type").is_none());
        let exc = &ev["exception"]["values"][0];
        assert_eq!(exc["mechanism"]["handled"], false, "escaped = unhandled");
        assert_eq!(exc["stacktrace"]["frames"].as_array().unwrap().len(), 2);
        assert!(ev.get("extra").is_none());

        // Through the Sentry side's own pipeline: title, columns, trace link.
        let now = chrono::DateTime::from_timestamp(1_730_450_001, 0).unwrap();
        let p = ProcessedEvent::from_normalized(&normalize_event(ev.clone(), &NormalizeContext::new(now)), now);
        assert_eq!(p.title, "ValueError: cart 7 is empty");
        assert_eq!(p.culprit.as_deref(), Some("POST /checkout"), "the transaction, as for SDK events");
        assert_eq!(p.trace_id.unwrap().simple().to_string(), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(p.span_id, Some(0x00f067aa0ba902b7));
        assert_eq!(p.handled, Some(false));
        assert_eq!(to_sentry_event(&found[0])["event_id"], ev["event_id"], "stable id for retries");
    }

    #[test]
    fn java_log_record_with_exception() {
        let java = "java.lang.IllegalStateException: order 42 has no lines\n\
                    \tat com.example.shop.OrderService.place(OrderService.java:57)\n\
                    \tat java.base/java.lang.Thread.run(Thread.java:1583)\n";
        let record = LogRecord {
            time_unix_nano: 1_730_450_000_000_000_000,
            severity_number: 17,
            body: Some(AnyValue { value: Some(any_value::Value::StringValue("checkout failed".into())) }),
            attributes: vec![
                kv("exception.type", "java.lang.IllegalStateException"),
                kv("exception.message", "order 42 has no lines"),
                kv("exception.stacktrace", java),
                kv("order.id", "42"),
            ],
            ..Default::default()
        };
        let plain = LogRecord { attributes: vec![kv("order.id", "1")], ..Default::default() };
        let req = ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: resource("java"),
                scope_logs: vec![ScopeLogs {
                    scope: Some(opentelemetry_proto::tonic::common::v1::InstrumentationScope {
                        name: "com.example.shop.OrderService".into(),
                        ..Default::default()
                    }),
                    log_records: vec![record, plain],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let found = from_logs(&req);
        assert_eq!(found.len(), 1, "plain logs are not exceptions");
        let ev = to_sentry_event(&found[0]);
        assert_eq!(ev["platform"], "java");
        assert_eq!(ev["level"], "error");
        assert_eq!(ev["logger"], "com.example.shop.OrderService");
        assert_eq!(ev["logentry"]["formatted"], "checkout failed");
        assert_eq!(ev["tags"]["otel.source"], "log");
        assert!(ev["contexts"].get("trace").is_none(), "no ids, no trace link");
        let frames = ev["exception"]["values"][0]["stacktrace"]["frames"].as_array().unwrap();
        assert_eq!(frames.last().unwrap()["function"], "place");
        assert_eq!(frames.last().unwrap()["module"], "com.example.shop.OrderService");
    }

    #[test]
    fn node_exception_and_raw_fallback() {
        let mut req = python_span();
        let rs = &mut req.resource_spans[0];
        rs.resource = resource("nodejs");
        let ev = &mut rs.scope_spans[0].spans[0].events[0];
        ev.attributes = vec![
            kv("exception.type", "TypeError"),
            kv("exception.message", "Cannot read properties of undefined (reading 'id')"),
            kv(
                "exception.stacktrace",
                "TypeError: Cannot read properties of undefined (reading 'id')\n    at getUser (/app/src/users.js:14:22)\n    at /app/src/index.js:30:7",
            ),
        ];
        let e = &from_traces(&req)[0];
        let sentry = to_sentry_event(e);
        assert_eq!(sentry["platform"], "node");
        let frames = sentry["exception"]["values"][0]["stacktrace"]["frames"].as_array().unwrap();
        assert_eq!(frames[1]["function"], "getUser");
        assert!(sentry["exception"]["values"][0]["mechanism"].get("handled").is_none());

        let mut raw = e.clone();
        raw.stacktrace = Some("no frames in here".into());
        let sentry = to_sentry_event(&raw);
        assert!(sentry["exception"]["values"][0].get("stacktrace").is_none());
        assert_eq!(sentry["extra"]["exception.stacktrace"], "no frames in here");

        let mut req = python_span();
        req.resource_spans[0].scope_spans[0].spans[0].events[0].attributes = vec![kv("exception.stacktrace", "x")];
        assert!(from_traces(&req).is_empty(), "needs a type or a message");
    }
}
