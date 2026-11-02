//! Sentry transaction → OTLP spans. The mapping table is in
//! docs/adr/0009-bridge.md.

use std::collections::BTreeMap;

use mallard_sentry_protocol::event::{client_address, parse_span_id, parse_timestamp, parse_trace_id, sdk_infer_ip};
use opentelemetry_proto::tonic::{
    collector::trace::v1::ExportTraceServiceRequest,
    common::v1::{AnyValue, InstrumentationScope, KeyValue, any_value},
    resource::v1::Resource,
    trace::v1::{ResourceSpans, ScopeSpans, Span, Status, span::SpanKind, status::StatusCode},
};
use serde_json::Value;

/// One OTLP request with the transaction's root span and its child spans.
/// `None` when the transaction has no valid `contexts.trace` ids or end time.
/// `client_ip` is the address of the request; only the root span can get it.
pub fn to_otlp(tx: &Value, project_slug: &str, client_ip: Option<&str>) -> Option<ExportTraceServiceRequest> {
    let trace = tx.get("contexts")?.get("trace")?;
    let trace_id = trace_id_bytes(trace.get("trace_id"))?;
    let root_id = span_id_bytes(trace.get("span_id"))?;
    let end = nanos(tx.get("timestamp"))?;
    let start = nanos(tx.get("start_timestamp")).unwrap_or(end);

    let mut root = Attrs::default();
    root.object(trace.get("data"));
    root.object(tx.get("tags"));
    root.sentry(trace);
    if let Some(user) = tx.get("user") {
        for (from, to) in
            [("id", "user.id"), ("email", "user.email"), ("username", "user.name"), ("ip_address", "client.address")]
        {
            root.set(to, user.get(from));
        }
    }
    let infer_ip = sdk_infer_ip(tx.get("sdk"));
    let mut attributes = root.finish();
    set_address(&mut attributes, "client.address", infer_ip, client_ip);
    let op = str_of(trace, "op").unwrap_or_default();
    let mut spans = vec![Span {
        trace_id: trace_id.clone(),
        span_id: root_id.clone(),
        parent_span_id: span_id_bytes(trace.get("parent_span_id")).unwrap_or_default(),
        name: str_of(tx, "transaction").or(Some(op).filter(|o| !o.is_empty())).unwrap_or("transaction").to_string(),
        kind: kind(trace.get("data"), op) as i32,
        start_time_unix_nano: start,
        end_time_unix_nano: end.max(start),
        status: status(str_of(trace, "status")),
        attributes,
        ..Default::default()
    }];

    let child_spans = tx.get("spans").and_then(Value::as_array).into_iter().flatten();
    spans.extend(child_spans.filter_map(|s| sentry_span(s, Some(&trace_id), &root_id)).map(|mut s| {
        set_address(&mut s.attributes, "client.address", not_added(infer_ip), client_ip);
        s
    }));

    let tags = tx.get("tags");
    let service = tags.and_then(|t| str_of(t, "service.name")).unwrap_or(project_slug);
    let mut res = Attrs::default();
    res.set("service.name", Some(&Value::from(service)));
    res.set("service.version", tx.get("release"));
    res.set("deployment.environment.name", tx.get("environment"));
    res.set("host.name", tx.get("server_name"));
    res.set("telemetry.sdk.language", tx.get("platform"));
    let sdk = tx.get("sdk");
    let scope = InstrumentationScope {
        name: sdk.and_then(|s| str_of(s, "name")).unwrap_or("sentry").to_string(),
        version: sdk.and_then(|s| str_of(s, "version")).unwrap_or_default().to_string(),
        ..Default::default()
    };
    Some(ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource { attributes: res.finish(), ..Default::default() }),
            scope_spans: vec![ScopeSpans { scope: Some(scope), spans, ..Default::default() }],
            ..Default::default()
        }],
    })
}

/// A span in Sentry's own format (`spans[]` of a transaction, or a standalone
/// span). `trace_id` and `parent` fill in when the span has none. `None`
/// without a valid trace id, span id or end time.
pub(super) fn sentry_span(s: &Value, trace_id: Option<&[u8]>, parent: &[u8]) -> Option<Span> {
    let span_id = span_id_bytes(s.get("span_id"))?;
    let end = nanos(s.get("timestamp"))?;
    let start = nanos(s.get("start_timestamp")).unwrap_or(end);
    let op = str_of(s, "op").unwrap_or_default();
    let mut a = Attrs::default();
    a.object(s.get("tags"));
    a.object(s.get("data"));
    a.sentry(s);
    Some(Span {
        trace_id: trace_id_bytes(s.get("trace_id")).or_else(|| trace_id.map(<[u8]>::to_vec))?,
        span_id,
        parent_span_id: span_id_bytes(s.get("parent_span_id")).unwrap_or_else(|| parent.to_vec()),
        name: str_of(s, "description").or(Some(op).filter(|o| !o.is_empty())).unwrap_or("span").to_string(),
        kind: kind(s.get("data"), op) as i32,
        start_time_unix_nano: start,
        end_time_unix_nano: end.max(start),
        status: status(str_of(s, "status")),
        attributes: a.finish(),
        ..Default::default()
    })
}

/// Store what [`client_address`] says in the attribute `key`: fill `{{auto}}`,
/// drop what must not be stored, add the address when `infer_ip` asks.
pub(super) fn set_address(attributes: &mut Vec<KeyValue>, key: &str, infer_ip: Option<&str>, request: Option<&str>) {
    let at = attributes.iter().position(|kv| kv.key == key);
    let sent = at.and_then(|i| match attributes[i].value.as_ref()?.value.as_ref()? {
        any_value::Value::StringValue(s) => Some(s.as_str()),
        _ => None,
    });
    let value = client_address(sent, infer_ip, request)
        .map(|a| AnyValue { value: Some(any_value::Value::StringValue(a.to_string())) });
    match (at, value) {
        (Some(i), Some(v)) => attributes[i].value = Some(v),
        (Some(i), None) => {
            attributes.remove(i);
        }
        (None, Some(v)) => attributes.push(KeyValue { key: key.to_string(), value: Some(v), ..Default::default() }),
        (None, None) => {}
    }
}

/// `infer_ip` for a span that does not get the address unless it holds
/// `{{auto}}` itself: one address per transaction or segment, on its root.
pub(super) fn not_added(infer_ip: Option<&str>) -> Option<&str> {
    infer_ip.filter(|i| *i != "auto")
}

/// A non-empty string field.
pub(super) fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Attributes with unique keys; a later value for a key wins.
#[derive(Default)]
pub(super) struct Attrs(pub(super) BTreeMap<String, AnyValue>);

impl Attrs {
    pub(super) fn set(&mut self, key: &str, v: Option<&Value>) {
        if let Some(v) = v.and_then(any_value) {
            self.0.insert(key.to_string(), v);
        }
    }

    /// Every entry of a JSON object (`tags`, `data`).
    fn object(&mut self, v: Option<&Value>) {
        for (k, v) in v.and_then(Value::as_object).into_iter().flatten() {
            self.set(k, Some(v));
        }
    }

    /// `op`, `origin` and `status` of a span or `contexts.trace`.
    fn sentry(&mut self, span: &Value) {
        for k in ["op", "origin", "status"] {
            self.set(&format!("sentry.{k}"), span.get(k));
        }
    }

    pub(super) fn finish(self) -> Vec<KeyValue> {
        self.0.into_iter().map(|(key, v)| KeyValue { key, value: Some(v), ..Default::default() }).collect()
    }
}

/// Scalars keep their type; arrays and objects become JSON text.
pub(super) fn any_value(v: &Value) -> Option<AnyValue> {
    use any_value::Value as V;
    let value = match v {
        Value::Null => return None,
        Value::String(s) if s.is_empty() => return None,
        Value::String(s) => V::StringValue(s.clone()),
        Value::Bool(b) => V::BoolValue(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => V::IntValue(i),
            None => V::DoubleValue(n.as_f64()?),
        },
        other => V::StringValue(other.to_string()),
    };
    Some(AnyValue { value: Some(value) })
}

pub(super) fn trace_id_bytes(v: Option<&Value>) -> Option<Vec<u8>> {
    Some(parse_trace_id(v?.as_str()?)?.as_bytes().to_vec())
}

pub(super) fn span_id_bytes(v: Option<&Value>) -> Option<Vec<u8>> {
    Some(parse_span_id(v?.as_str()?)?.to_be_bytes().to_vec())
}

/// Seconds (float) or RFC 3339 → Unix nanoseconds. Float seconds are
/// rounded to microseconds (what is stored), so float noise does not show.
pub(super) fn nanos(v: Option<&Value>) -> Option<u64> {
    let ns = match v? {
        Value::Number(n) => (n.as_f64()? * 1e6).round() as i64 * 1000,
        other => parse_timestamp(other)?.timestamp_nanos_opt()?,
    };
    u64::try_from(ns).ok().filter(|n| *n > 0)
}

/// `data["otel.kind"]` when the SDK sends it (OTel-based SDKs), else from `op`.
fn kind(data: Option<&Value>, op: &str) -> SpanKind {
    op_kind(data.and_then(|d| d.get("otel.kind")).and_then(Value::as_str), op)
}

/// The kind the SDK named (`server`, `CLIENT`, …), else from `op`.
pub(super) fn op_kind(named: Option<&str>, op: &str) -> SpanKind {
    if let Some(k) = named.and_then(|k| SpanKind::from_str_name(&format!("SPAN_KIND_{}", k.to_ascii_uppercase()))) {
        return k;
    }
    let op = op.to_ascii_lowercase();
    let starts = |p: &[&str]| p.iter().any(|p| op.starts_with(p));
    if op.ends_with(".server") {
        SpanKind::Server
    } else if op.ends_with(".client")
        || ["http", "db", "rpc", "grpc"].contains(&op.as_str())
        || starts(&["db.", "cache."])
    {
        SpanKind::Client
    } else if starts(&["queue.publish", "queue.submit"]) {
        SpanKind::Producer
    } else if starts(&["queue.process", "queue.receive", "queue.task"]) {
        SpanKind::Consumer
    } else {
        SpanKind::Internal
    }
}

/// `ok` → OK; missing → UNSET; any other Sentry status → ERROR with the
/// status as message.
fn status(s: Option<&str>) -> Option<Status> {
    let (code, message) = match s? {
        "ok" => (StatusCode::Ok, String::new()),
        other => (StatusCode::Error, other.to_string()),
    };
    Some(Status { code: code as i32, message })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The client address of the root and of a child span.
    #[test]
    fn client_address_only_when_the_sdk_asks() {
        let addresses = |sdk: Value, user: Value, child: Value, client_ip: Option<&str>| {
            let tx = json!({
                "contexts": {"trace": {"span_id": "a1b2c3d4e5f60718", "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736"}},
                "start_timestamp": 1793538736.0, "timestamp": 1793538737.0, "sdk": sdk, "user": user,
                "spans": [{"span_id": "1111111111111111", "start_timestamp": 1793538736.0, "timestamp": 1793538737.0,
                           "data": child}]
            });
            let req = to_otlp(&tx, "shop", client_ip).unwrap();
            let spans = &req.resource_spans[0].scope_spans[0].spans;
            let get = |s: &Span| attr(&s.attributes, "client.address");
            (get(&spans[0]), get(&spans[1]))
        };
        let ip = "203.0.113.9";
        let (auto, never) = (json!({"settings": {"infer_ip": "auto"}}), json!({"settings": {"infer_ip": "never"}}));
        let (none, want_ip) = (json!({}), Some(ip.to_string()));
        let asked = json!({"ip_address": "{{auto}}"});
        let own = json!({"ip_address": "198.51.100.7"});
        let child_asked = json!({"client.address": "{{auto}}"});
        let no_addr: (Option<String>, Option<String>) = (None, None);

        assert_eq!(addresses(auto.clone(), none.clone(), none.clone(), Some(ip)), (want_ip.clone(), None));
        assert_eq!(addresses(auto.clone(), none.clone(), none.clone(), None), no_addr);
        assert_eq!(addresses(never.clone(), none.clone(), none.clone(), Some(ip)), no_addr);
        assert_eq!(addresses(none.clone(), none.clone(), none.clone(), Some(ip)), no_addr, "the SDK did not ask");
        assert_eq!(addresses(none.clone(), asked.clone(), none.clone(), Some(ip)), (want_ip.clone(), None));
        assert_eq!(addresses(none.clone(), asked.clone(), none.clone(), None), no_addr, "not the placeholder");
        assert_eq!(addresses(never.clone(), asked.clone(), child_asked.clone(), Some(ip)), no_addr);
        assert_eq!(addresses(none.clone(), none.clone(), child_asked.clone(), Some(ip)), (None, want_ip.clone()));
        assert_eq!(addresses(none.clone(), none.clone(), child_asked, None), no_addr);
        assert_eq!(
            addresses(auto, own.clone(), none.clone(), Some(ip)),
            (Some("198.51.100.7".to_string()), None),
            "the SDK's own"
        );
        assert_eq!(addresses(never, own, none.clone(), Some(ip)), no_addr);
    }

    /// What @sentry/node 8 sends for an Express request with two child spans.
    fn node_transaction() -> Value {
        json!({
            "contexts": {
                "trace": {
                    "span_id": "a1b2c3d4e5f60718", "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
                    "parent_span_id": "00f067aa0ba902b7",
                    "data": {"sentry.source": "route", "sentry.origin": "auto.http.otel.http", "sentry.op": "http.server",
                             "sentry.sample_rate": 1, "otel.kind": "SERVER", "http.method": "GET",
                             "http.route": "/users/:id", "http.status_code": 500, "url": "http://localhost:3000/users/42"},
                    "origin": "auto.http.otel.http", "op": "http.server", "status": "internal_error"
                },
                "runtime": {"name": "node", "version": "v20.11.0"},
                "otel": {"resource": {"service.name": "node", "service.namespace": "sentry"}}
            },
            "spans": [
                {"span_id": "1111111111111111", "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
                 "parent_span_id": "a1b2c3d4e5f60718", "description": "query", "op": "middleware.express",
                 "origin": "auto.http.otel.express", "status": "ok",
                 "data": {"express.name": "query", "express.type": "middleware", "sentry.op": "middleware.express"},
                 "start_timestamp": 1730450000.101, "timestamp": 1730450000.102},
                {"span_id": "2222222222222222", "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
                 "parent_span_id": "a1b2c3d4e5f60718", "description": "SELECT * FROM users WHERE id = $1",
                 "op": "db", "origin": "auto.db.otel.postgres", "status": "internal_error",
                 "data": {"db.system": "postgresql", "db.name": "shop"}, "tags": {"shard": "a"},
                 "start_timestamp": 1730450000.110, "timestamp": 1730450000.240},
                {"description": "no ids, skipped", "start_timestamp": 1730450000.1, "timestamp": 1730450000.2}
            ],
            "start_timestamp": 1730450000.1, "timestamp": 1730450000.25,
            "transaction": "GET /users/:id", "type": "transaction", "transaction_info": {"source": "route"},
            "platform": "node", "server_name": "web-1", "event_id": "c5b5c7c6c0a64b0c8b3c3e5e1b1b2b3b",
            "environment": "production", "release": "shop@1.2.3",
            "sdk": {"name": "sentry.javascript.node", "version": "8.35.0"},
            "user": {"id": "42", "ip_address": "10.0.0.1"}, "tags": {"tenant": "acme"}
        })
    }

    fn attr(kvs: &[KeyValue], key: &str) -> Option<String> {
        kvs.iter().find(|k| k.key == key).and_then(|k| k.value.as_ref()?.value.as_ref()).map(|v| match v {
            any_value::Value::StringValue(s) => s.clone(),
            any_value::Value::IntValue(i) => i.to_string(),
            any_value::Value::BoolValue(b) => b.to_string(),
            any_value::Value::DoubleValue(d) => d.to_string(),
            other => format!("{other:?}"),
        })
    }

    #[test]
    fn node_transaction_becomes_a_root_span_with_children() {
        let req = to_otlp(&node_transaction(), "shop", None).unwrap();
        let rs = &req.resource_spans[0];
        let res = &rs.resource.as_ref().unwrap().attributes;
        assert_eq!(attr(res, "service.name").as_deref(), Some("shop"), "the project slug, not OTel's `node`");
        assert_eq!(attr(res, "service.version").as_deref(), Some("shop@1.2.3"));
        assert_eq!(attr(res, "deployment.environment.name").as_deref(), Some("production"));
        assert_eq!(attr(res, "host.name").as_deref(), Some("web-1"));
        let scope = rs.scope_spans[0].scope.as_ref().unwrap();
        assert_eq!((scope.name.as_str(), scope.version.as_str()), ("sentry.javascript.node", "8.35.0"));

        let spans = &rs.scope_spans[0].spans;
        assert_eq!(spans.len(), 3, "the span without ids is skipped");
        let root = &spans[0];
        assert_eq!(root.name, "GET /users/:id");
        assert_eq!(hex::encode(&root.trace_id), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(hex::encode(&root.span_id), "a1b2c3d4e5f60718");
        assert_eq!(hex::encode(&root.parent_span_id), "00f067aa0ba902b7", "distributed parent kept");
        assert_eq!(root.kind, SpanKind::Server as i32);
        assert_eq!(root.end_time_unix_nano - root.start_time_unix_nano, 150_000_000);
        let st = root.status.as_ref().unwrap();
        assert_eq!((st.code, st.message.as_str()), (StatusCode::Error as i32, "internal_error"));
        for (k, v) in [
            ("http.route", "/users/:id"),
            ("http.status_code", "500"),
            ("tenant", "acme"),
            ("sentry.op", "http.server"),
            ("sentry.origin", "auto.http.otel.http"),
            ("user.id", "42"),
            ("client.address", "10.0.0.1"),
        ] {
            assert_eq!(attr(&root.attributes, k).as_deref(), Some(v), "{k}");
        }

        let mw = &spans[1];
        assert_eq!((mw.name.as_str(), mw.kind), ("query", SpanKind::Internal as i32));
        assert_eq!(mw.parent_span_id, root.span_id);
        assert_eq!(mw.status.as_ref().unwrap().code, StatusCode::Ok as i32);
        assert_eq!(mw.end_time_unix_nano - mw.start_time_unix_nano, 1_000_000);
        let db = &spans[2];
        assert_eq!((db.name.as_str(), db.kind), ("SELECT * FROM users WHERE id = $1", SpanKind::Client as i32));
        assert_eq!(attr(&db.attributes, "db.system").as_deref(), Some("postgresql"));
        assert_eq!(attr(&db.attributes, "shard").as_deref(), Some("a"));
        assert_eq!(db.status.as_ref().unwrap().code, StatusCode::Error as i32);
    }

    #[test]
    fn service_name_tag_wins_and_ids_are_required() {
        let mut tx = node_transaction();
        tx["tags"]["service.name"] = json!("checkout");
        tx["start_timestamp"] = json!("2024-11-01T08:33:20.100Z");
        let req = to_otlp(&tx, "shop", None).unwrap();
        let rs = &req.resource_spans[0];
        assert_eq!(attr(&rs.resource.as_ref().unwrap().attributes, "service.name").as_deref(), Some("checkout"));
        assert!(rs.scope_spans[0].spans[0].start_time_unix_nano > 0, "RFC 3339 timestamps work too");

        tx["contexts"]["trace"]["trace_id"] = json!("not hex");
        assert!(to_otlp(&tx, "shop", None).is_none());
        assert!(to_otlp(&json!({"type": "transaction"}), "shop", None).is_none());
    }

    #[test]
    fn op_maps_to_span_kind() {
        for (op, k) in [
            ("http.server", SpanKind::Server),
            ("grpc.server", SpanKind::Server),
            ("http.client", SpanKind::Client),
            ("db.sql.query", SpanKind::Client),
            ("cache.get", SpanKind::Client),
            ("queue.publish", SpanKind::Producer),
            ("queue.process", SpanKind::Consumer),
            ("ui.render", SpanKind::Internal),
            ("", SpanKind::Internal),
        ] {
            assert_eq!(kind(None, op), k, "{op}");
        }
        assert_eq!(kind(Some(&json!({"otel.kind": "PRODUCER"})), "http.server"), SpanKind::Producer);
        assert_eq!(status(None), None);
        assert_eq!(status(Some("cancelled")).unwrap().code, StatusCode::Error as i32);
    }
}
