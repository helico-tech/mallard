//! Sentry span items → OTLP spans: span streaming (`span.v2`, mapping table
//! in docs/adr/0013-sentry-span-streaming.md) and the standalone spans of
//! SDKs before 11 (docs/adr/0022-sentry-standalone-spans.md).
//!
//! A v2 item is `{"version": 2, "items": [span, …]}`; each span has
//! `trace_id`, `span_id`, `parent_span_id`, `name`, `start_timestamp`,
//! `end_timestamp` (seconds), `status` (`ok` | `error`), `is_segment`,
//! typed `attributes` (`{"key": {"type": "string", "value": "…"}}`) and
//! `links`. Release, environment and SDK are attributes on every span
//! (`sentry.release`, …); the envelope's `trace` and `sdk` headers fill in
//! when a span lacks them.
//!
//! A standalone item is one span in the format of a transaction's `spans[]`,
//! without a content type. Its `data` holds `release` and `environment`.

use std::collections::BTreeMap;

use opentelemetry_proto::tonic::{
    collector::trace::v1::ExportTraceServiceRequest,
    common::v1::{AnyValue, ArrayValue, InstrumentationScope, KeyValue, any_value},
    resource::v1::Resource,
    trace::v1::{ResourceSpans, ScopeSpans, Span, Status, span::Link, status::StatusCode},
};
use serde_json::{Map, Value};

use super::transaction::{Attrs, any_value, nanos, op_kind, sentry_span, span_id_bytes, str_of, trace_id_bytes};

/// The item content type of span streaming.
pub const V2_CONTENT_TYPE: &str = "application/vnd.sentry.items.span.v2+json";

/// Span attributes that become resource or scope attributes instead.
const MOVED: [&str; 4] = ["sentry.release", "sentry.environment", "sentry.sdk.name", "sentry.sdk.version"];

/// Longest envelope header value kept: Sentry limits releases to 200 bytes.
/// The headers come from the client and are not limited otherwise, and each
/// queued item keeps a copy.
const MAX_HEADER_VALUE: usize = 200;

/// What an envelope's headers say about its spans, for spans that lack it
/// themselves: the `trace` header (release, environment) and the `sdk` header.
#[derive(Debug, Default)]
pub struct Defaults {
    release: Option<String>,
    environment: Option<String>,
    sdk_name: Option<String>,
    sdk_version: Option<String>,
}

impl Defaults {
    pub fn from_headers(headers: &Map<String, Value>) -> Self {
        let value = |header: &str, key: &str| {
            let v = str_of(headers.get(header)?, key)?;
            (v.len() <= MAX_HEADER_VALUE).then(|| v.to_string())
        };
        Defaults {
            release: value("trace", "release"),
            environment: value("trace", "environment"),
            sdk_name: value("sdk", "name"),
            sdk_version: value("sdk", "version"),
        }
    }
}

/// Spans of one service, release, environment and SDK.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Group {
    service: String,
    release: Option<String>,
    environment: Option<String>,
    sdk_name: String,
    sdk_version: String,
}

/// One OTLP request with the item's spans, grouped by resource. `None` when
/// the payload is not a v2 container; spans without valid ids or times are
/// skipped (the request may then hold no spans).
pub fn to_otlp(item: &Value, defaults: &Defaults, project_slug: &str) -> Option<ExportTraceServiceRequest> {
    let spans = item.get("items")?.as_array()?;

    let mut groups: BTreeMap<Group, Vec<Span>> = BTreeMap::new();
    for s in spans {
        let Some(span) = convert(s) else { continue };
        let attr = |key: &str| s.get("attributes").and_then(|a| a.get(key)).and_then(typed).as_ref().and_then(text);
        let group = Group {
            service: attr("service.name").unwrap_or_else(|| project_slug.to_string()),
            release: attr("sentry.release").or_else(|| defaults.release.clone()),
            environment: attr("sentry.environment").or_else(|| defaults.environment.clone()),
            sdk_name: attr("sentry.sdk.name").or_else(|| defaults.sdk_name.clone()).unwrap_or_else(|| "sentry".into()),
            sdk_version: attr("sentry.sdk.version").or_else(|| defaults.sdk_version.clone()).unwrap_or_default(),
        };
        groups.entry(group).or_default().push(span);
    }
    Some(request(groups))
}

/// One OTLP request for a standalone span. `None` without valid ids or an end
/// time.
pub fn standalone_to_otlp(s: &Value, defaults: &Defaults, project_slug: &str) -> Option<ExportTraceServiceRequest> {
    let mut span = sentry_span(s, None, &[])?;
    let data = s.get("data");
    let moved = |key: &str| data.and_then(|d| str_of(d, key)).map(str::to_string);
    let group = Group {
        service: project_slug.to_string(),
        release: moved("release").or_else(|| defaults.release.clone()),
        environment: moved("environment").or_else(|| defaults.environment.clone()),
        sdk_name: defaults.sdk_name.clone().unwrap_or_else(|| "sentry".into()),
        sdk_version: defaults.sdk_version.clone().unwrap_or_default(),
    };
    span.attributes.retain(|kv| kv.key != "release" && kv.key != "environment");
    // Web vitals are measurements here; SDK 11 sends the same attribute.
    for (name, m) in s.get("measurements").and_then(Value::as_object).into_iter().flatten() {
        if let Some(value) = m.get("value").and_then(any_value) {
            let key = format!("browser.web_vital.{name}.value");
            span.attributes.push(KeyValue { key, value: Some(value), ..Default::default() });
        }
    }
    Some(request(BTreeMap::from([(group, vec![span])])))
}

fn request(groups: BTreeMap<Group, Vec<Span>>) -> ExportTraceServiceRequest {
    let resource_spans = groups
        .into_iter()
        .map(|(g, spans)| {
            let mut res = Attrs::default();
            res.set("service.name", Some(&Value::from(g.service)));
            res.set("service.version", g.release.map(Value::from).as_ref());
            res.set("deployment.environment.name", g.environment.map(Value::from).as_ref());
            let scope = InstrumentationScope { name: g.sdk_name, version: g.sdk_version, ..Default::default() };
            ResourceSpans {
                resource: Some(Resource { attributes: res.finish(), ..Default::default() }),
                scope_spans: vec![ScopeSpans { scope: Some(scope), spans, ..Default::default() }],
                ..Default::default()
            }
        })
        .collect();
    ExportTraceServiceRequest { resource_spans }
}

/// One v2 span. `None` without valid ids or any timestamp.
fn convert(s: &Value) -> Option<Span> {
    let trace_id = trace_id_bytes(s.get("trace_id"))?;
    let span_id = span_id_bytes(s.get("span_id"))?;
    let start = nanos(s.get("start_timestamp")).or_else(|| nanos(s.get("end_timestamp")))?;
    let end = nanos(s.get("end_timestamp")).unwrap_or(start);

    let mut attributes = BTreeMap::new();
    for (k, v) in s.get("attributes").and_then(Value::as_object).into_iter().flatten() {
        if let Some(v) = typed(v).filter(|_| !MOVED.contains(&k.as_str())) {
            attributes.insert(k.clone(), v);
        }
    }
    let text_attr = |k: &str| attributes.get(k).and_then(text);
    let op = text_attr("sentry.op").unwrap_or_default();
    let kind = op_kind(text_attr("sentry.kind").as_deref(), &op);
    let name = str_of(s, "name").map(str::to_string).or(Some(op).filter(|o| !o.is_empty()));
    let status = s.get("status").and_then(Value::as_str).filter(|s| !s.is_empty()).map(|st| match st {
        "ok" => Status { code: StatusCode::Ok as i32, message: String::new() },
        other => Status {
            code: StatusCode::Error as i32,
            message: text_attr("sentry.status.message").unwrap_or_else(|| other.to_string()),
        },
    });
    let links = s.get("links").and_then(Value::as_array).into_iter().flatten().filter_map(link).collect();
    Some(Span {
        trace_id,
        span_id,
        parent_span_id: span_id_bytes(s.get("parent_span_id")).unwrap_or_default(),
        name: name.unwrap_or_else(|| "span".into()),
        kind: kind as i32,
        start_time_unix_nano: start,
        end_time_unix_nano: end.max(start),
        status,
        attributes: key_values(attributes),
        links,
        ..Default::default()
    })
}

fn link(l: &Value) -> Option<Link> {
    let mut attributes = BTreeMap::new();
    for (k, v) in l.get("attributes").and_then(Value::as_object).into_iter().flatten() {
        if let Some(v) = typed(v) {
            attributes.insert(k.clone(), v);
        }
    }
    Some(Link {
        trace_id: trace_id_bytes(l.get("trace_id"))?,
        span_id: span_id_bytes(l.get("span_id"))?,
        attributes: key_values(attributes),
        flags: u32::from(l.get("sampled").and_then(Value::as_bool).unwrap_or(false)),
        ..Default::default()
    })
}

fn key_values(attrs: BTreeMap<String, AnyValue>) -> Vec<KeyValue> {
    attrs.into_iter().map(|(key, v)| KeyValue { key, value: Some(v), ..Default::default() }).collect()
}

/// A typed attribute (`{"type": "integer", "value": 3}`; the `unit` is
/// dropped) as an OTel value. A bare value is taken as it is.
fn typed(v: &Value) -> Option<AnyValue> {
    use any_value::Value as V;
    let Some(obj) = v.as_object().filter(|o| o.contains_key("value")) else { return any_value(v) };
    let value = &obj["value"];
    let num = |f: fn(&Value) -> Option<V>| f(value).or_else(|| value.as_str().and_then(|s| f(&s.parse().ok()?)));
    let typed = match obj.get("type").and_then(Value::as_str) {
        Some("string") => value.as_str().map(|s| V::StringValue(s.to_string())),
        Some("boolean") => value.as_bool().map(V::BoolValue),
        Some("integer") => num(|v| v.as_i64().map(V::IntValue)),
        Some("double") => num(|v| v.as_f64().map(V::DoubleValue)),
        Some("array") => {
            value.as_array().map(|a| V::ArrayValue(ArrayValue { values: a.iter().filter_map(any_value).collect() }))
        }
        _ => None,
    };
    match typed {
        Some(t) => Some(AnyValue { value: Some(t) }),
        None => any_value(value),
    }
}

/// The text of a string, int or bool value.
fn text(v: &AnyValue) -> Option<String> {
    match v.value.as_ref()? {
        any_value::Value::StringValue(s) if !s.is_empty() => Some(s.clone()),
        any_value::Value::IntValue(i) => Some(i.to_string()),
        any_value::Value::BoolValue(b) => Some(b.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::trace::v1::span::SpanKind;
    use serde_json::json;

    fn attr<'a>(kvs: &'a [KeyValue], key: &str) -> Option<&'a any_value::Value> {
        kvs.iter().find(|k| k.key == key)?.value.as_ref()?.value.as_ref()
    }

    fn s(v: &str) -> any_value::Value {
        any_value::Value::StringValue(v.into())
    }

    /// Shaped like what @sentry/node 11 sends (trimmed).
    fn item() -> Value {
        json!({"version": 2, "items": [
            {"name": "charge card", "span_id": "88bd0a55715169b9", "trace_id": "96840ee330eb4f48adfeda513ab13381",
             "parent_span_id": "bc2f788536782290", "start_timestamp": 1793538736.9843285,
             "end_timestamp": 1793538736.984466, "is_segment": false, "status": "error",
             "attributes": {
                "sentry.status.message": {"value": "internal_error", "type": "string"},
                "sentry.op": {"value": "payment.charge", "type": "string"},
                "sentry.segment.name": {"value": "GET", "type": "string"},
                "sentry.release": {"value": "tracing-app@1.0.0", "type": "string"},
                "sentry.environment": {"value": "e2e", "type": "string"},
                "sentry.sdk.name": {"value": "sentry.javascript.node", "type": "string"},
                "sentry.sdk.version": {"value": "11.10.0", "type": "string"},
                "sentry.is_localhost": {"value": true, "type": "boolean"}}},
            {"name": "GET", "span_id": "bc2f788536782290", "trace_id": "96840ee330eb4f48adfeda513ab13381",
             "parent_span_id": "95c5b31c975f2037", "start_timestamp": 1793538736.9767754,
             "end_timestamp": 1793538736.988743, "is_segment": true, "status": "ok",
             "links": [{"trace_id": "16840ee330eb4f48adfeda513ab13381", "span_id": "15c5b31c975f2037",
                        "sampled": true, "attributes": {"sentry.link.type": {"type": "string", "value": "previous_trace"}}}],
             "attributes": {
                "sentry.op": {"value": "http.server", "type": "string"},
                "sentry.kind": {"value": "server", "type": "string"},
                "http.response.status_code": {"value": 500, "type": "integer"},
                "app.memory": {"value": "109035520", "type": "integer"},
                "sentry.sample_rate": {"value": 0.5, "type": "double"},
                "device.archs": {"value": ["arm64"], "type": "array"},
                "plain": "bare"}},
            {"name": "no ids", "start_timestamp": 1793538736.9}
        ]})
    }

    /// What @sentry/browser 10.78 sends for an INP (slow interaction): written
    /// from its source (`createSpanEnvelope`, `SentrySpan.getSpanJSON`,
    /// `startStandaloneWebVitalSpan`, `_onInp`), not captured from a browser.
    fn inp_span() -> Value {
        json!({
            "data": {
                "sentry.origin": "auto.http.browser.inp",
                "sentry.op": "ui.interaction.click",
                "sentry.exclusive_time": 232,
                "release": "shop-web@2.4.0",
                "environment": "production",
                "transaction": "/checkout",
                "user": "dev@example.com",
                "user_agent.original": "Mozilla/5.0 (Macintosh) Chrome/141.0.0.0 Safari/537.36",
                "sentry.sample_rate": 1
            },
            "description": "body > main > button#checkout",
            "op": "ui.interaction.click",
            "parent_span_id": "a1b2c3d4e5f60718",
            "span_id": "9c1f0e2d3b4a5968",
            "start_timestamp": 1793538736.5,
            "timestamp": 1793538736.732,
            "trace_id": "96840ee330eb4f48adfeda513ab13381",
            "origin": "auto.http.browser.inp",
            "exclusive_time": 232,
            "measurements": {"inp": {"value": 232, "unit": "millisecond"}},
            "is_segment": true,
            "segment_id": "9c1f0e2d3b4a5968"
        })
    }

    #[test]
    fn standalone_spans_become_otel_spans() {
        let headers =
            json!({"trace": {"release": "from-dsc", "environment": "from-dsc", "trace_id": "x", "public_key": "y"}});
        let defaults = Defaults::from_headers(headers.as_object().unwrap());
        let req = standalone_to_otlp(&inp_span(), &defaults, "shop").unwrap();
        assert_eq!(req.resource_spans.len(), 1);
        let rs = &req.resource_spans[0];
        let res = &rs.resource.as_ref().unwrap().attributes;
        assert_eq!(attr(res, "service.name"), Some(&s("shop")));
        assert_eq!(attr(res, "service.version"), Some(&s("shop-web@2.4.0")));
        assert_eq!(attr(res, "deployment.environment.name"), Some(&s("production")));
        assert_eq!(rs.scope_spans[0].scope.as_ref().unwrap().name, "sentry", "the envelope has no sdk header");

        let span = &rs.scope_spans[0].spans[0];
        assert_eq!(hex::encode(&span.trace_id), "96840ee330eb4f48adfeda513ab13381");
        assert_eq!(hex::encode(&span.span_id), "9c1f0e2d3b4a5968");
        assert_eq!(hex::encode(&span.parent_span_id), "a1b2c3d4e5f60718");
        assert_eq!(span.name, "body > main > button#checkout");
        assert_eq!(span.kind, SpanKind::Internal as i32);
        assert_eq!(span.end_time_unix_nano - span.start_time_unix_nano, 232_000_000);
        assert_eq!(attr(&span.attributes, "browser.web_vital.inp.value"), Some(&any_value::Value::IntValue(232)));
        assert_eq!(attr(&span.attributes, "transaction"), Some(&s("/checkout")));
        assert_eq!(attr(&span.attributes, "sentry.op"), Some(&s("ui.interaction.click")));
        assert_eq!(attr(&span.attributes, "release"), None, "on the resource");
        assert_eq!(attr(&span.attributes, "environment"), None, "on the resource");

        // Without release and environment in `data`, the envelope's are used.
        let mut bare = inp_span();
        bare["data"] = json!({});
        let req = standalone_to_otlp(&bare, &defaults, "shop").unwrap();
        let res = &req.resource_spans[0].resource.as_ref().unwrap().attributes;
        assert_eq!(attr(res, "service.version"), Some(&s("from-dsc")));

        let mut no_id = inp_span();
        no_id["trace_id"] = json!("nope");
        assert!(standalone_to_otlp(&no_id, &defaults, "shop").is_none());
    }

    #[test]
    fn v2_spans_become_otel_spans() {
        let headers =
            json!({"trace": {"release": "from-dsc", "environment": "prod"}, "sdk": {"name": "x", "version": "1"}});
        let req = to_otlp(&item(), &Defaults::from_headers(headers.as_object().unwrap()), "shop").unwrap();
        assert_eq!(req.resource_spans.len(), 2, "the second span has no release attributes: the envelope's");

        let rs = req.resource_spans.iter().find(|r| r.scope_spans[0].spans[0].name == "charge card").unwrap();
        let res = &rs.resource.as_ref().unwrap().attributes;
        assert_eq!(attr(res, "service.name"), Some(&s("shop")));
        assert_eq!(attr(res, "service.version"), Some(&s("tracing-app@1.0.0")));
        assert_eq!(attr(res, "deployment.environment.name"), Some(&s("e2e")));
        let scope = rs.scope_spans[0].scope.as_ref().unwrap();
        assert_eq!((scope.name.as_str(), scope.version.as_str()), ("sentry.javascript.node", "11.10.0"));
        let charge = &rs.scope_spans[0].spans[0];
        assert_eq!(hex::encode(&charge.trace_id), "96840ee330eb4f48adfeda513ab13381");
        assert_eq!(hex::encode(&charge.parent_span_id), "bc2f788536782290");
        assert_eq!(charge.kind, SpanKind::Internal as i32);
        let st = charge.status.as_ref().unwrap();
        assert_eq!((st.code, st.message.as_str()), (StatusCode::Error as i32, "internal_error"));
        assert_eq!(charge.end_time_unix_nano - charge.start_time_unix_nano, 137_000, "rounded to µs");
        assert_eq!(attr(&charge.attributes, "sentry.segment.name"), Some(&s("GET")));
        assert_eq!(attr(&charge.attributes, "sentry.is_localhost"), Some(&any_value::Value::BoolValue(true)));
        assert!(attr(&charge.attributes, "sentry.release").is_none(), "moved to the resource");

        let rs = req.resource_spans.iter().find(|r| r.scope_spans[0].spans[0].name == "GET").unwrap();
        let res = &rs.resource.as_ref().unwrap().attributes;
        assert_eq!(attr(res, "service.version"), Some(&s("from-dsc")));
        assert_eq!(rs.scope_spans[0].scope.as_ref().unwrap().name, "x");
        let get = &rs.scope_spans[0].spans[0];
        assert_eq!(get.kind, SpanKind::Server as i32);
        assert_eq!(get.status.as_ref().unwrap().code, StatusCode::Ok as i32);
        let a = &get.attributes;
        assert_eq!(attr(a, "http.response.status_code"), Some(&any_value::Value::IntValue(500)));
        assert_eq!(attr(a, "app.memory"), Some(&any_value::Value::IntValue(109_035_520)), "integers as text");
        assert_eq!(attr(a, "sentry.sample_rate"), Some(&any_value::Value::DoubleValue(0.5)));
        assert_eq!(attr(a, "plain"), Some(&s("bare")));
        let Some(any_value::Value::ArrayValue(archs)) = attr(a, "device.archs") else { panic!("array") };
        assert_eq!(archs.values.len(), 1);
        assert_eq!(get.links.len(), 1);
        assert_eq!(hex::encode(&get.links[0].span_id), "15c5b31c975f2037");
        assert_eq!(get.links[0].flags, 1);
    }

    #[test]
    fn names_kinds_and_bad_payloads() {
        let span = |v: Value| convert(&v).unwrap();
        let ids = json!({"trace_id": "96840ee330eb4f48adfeda513ab13381", "span_id": "88bd0a55715169b9",
            "end_timestamp": 1793538736.9});
        let mut v = ids.clone();
        v["attributes"] = json!({"sentry.op": {"type": "string", "value": "db.query"}});
        let db = span(v);
        assert_eq!((db.name.as_str(), db.kind), ("db.query", SpanKind::Client as i32), "no name: the op");
        assert_eq!(db.start_time_unix_nano, db.end_time_unix_nano, "no start: the end");
        assert!(db.status.is_none() && db.parent_span_id.is_empty());
        assert_eq!(span(ids.clone()).name, "span");

        let none = Defaults::default();
        assert!(to_otlp(&json!({"span_id": "88bd0a55715169b9"}), &none, "shop").is_none(), "not v2");
        let req = to_otlp(&json!({"version": 2, "items": [{"name": "x"}]}), &none, "shop").unwrap();
        assert!(req.resource_spans.is_empty());

        let long = json!({"trace": {"release": "r".repeat(201), "environment": "prod"}});
        let d = Defaults::from_headers(long.as_object().unwrap());
        assert_eq!((d.release, d.environment.as_deref()), (None, Some("prod")), "oversized header values are ignored");
    }
}
