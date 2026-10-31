//! Event normalization.
//!
//! SDKs disagree on shapes (`exception` as list or `{values}`, `timestamp`
//! as float or RFC 3339, `tags` as map or pairs, ...). [`normalize_event`]
//! turns any of these into one canonical JSON shape, close to what Sentry
//! itself stores. [`ProcessedEvent::from_normalized`] then extracts the
//! typed columns we store and computes the issue group.

use crate::grouping;
use crate::useragent;
use chrono::{DateTime, Duration, NaiveDateTime, TimeZone, Utc};
use serde_json::{Map, Value, json};
use uuid::Uuid;

pub const MAX_TAG_KEY: usize = 200;
pub const MAX_TAG_VALUE: usize = 200;

#[derive(Debug, Clone)]
pub struct NormalizeContext {
    pub received_at: DateTime<Utc>,
    /// IP of the client, used to fill `user.ip_address = "{{auto}}"`.
    pub client_ip: Option<String>,
    /// `event_id` from the envelope header, used when the payload has none.
    pub event_id_hint: Option<String>,
    pub max_breadcrumbs: usize,
}

impl NormalizeContext {
    pub fn new(received_at: DateTime<Utc>) -> Self {
        Self { received_at, client_ip: None, event_id_hint: None, max_breadcrumbs: 100 }
    }
}

/// The typed view of a normalized event: the columns we store.
#[derive(Debug, Clone)]
pub struct ProcessedEvent {
    pub event_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub level: String,
    pub platform: String,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub dist: Option<String>,
    pub server_name: Option<String>,
    pub transaction: Option<String>,
    pub logger: Option<String>,
    pub message: Option<String>,
    pub title: String,
    pub culprit: Option<String>,
    pub exception_type: Option<String>,
    pub exception_value: Option<String>,
    pub handled: Option<bool>,
    pub user_key: Option<String>,
    pub sdk_name: Option<String>,
    pub sdk_version: Option<String>,
    pub tags: Vec<(String, String)>,
    /// `contexts.trace.trace_id` (32 hex chars) as UUID, for joins with traces.
    pub trace_id: Option<Uuid>,
    /// `contexts.trace.span_id` (16 hex chars) as big-endian u64.
    pub span_id: Option<u64>,
    /// Stable 64-bit issue group hash (fingerprint).
    pub group_hash: u64,
    /// The full normalized event.
    pub payload: Value,
}

pub fn parse_uuid(s: &str) -> Option<Uuid> {
    Uuid::try_parse(s.trim()).ok()
}

/// Normalize a raw event payload in place into the canonical shape.
pub fn normalize_event(mut raw: Value, ctx: &NormalizeContext) -> Value {
    if !raw.is_object() {
        raw = json!({});
    }
    let ev = raw.as_object_mut().expect("object");

    // event_id
    let id = ev
        .get("event_id")
        .and_then(Value::as_str)
        .and_then(parse_uuid)
        .or_else(|| ctx.event_id_hint.as_deref().and_then(parse_uuid))
        .unwrap_or_else(Uuid::new_v4);
    ev.insert("event_id".into(), Value::String(id.simple().to_string()));

    // timestamp: clamp obviously wrong client clocks like Sentry does.
    let ts = ev.get("timestamp").and_then(parse_timestamp).unwrap_or(ctx.received_at);
    let ts = if ts > ctx.received_at + Duration::minutes(5) || ts < ctx.received_at - Duration::days(30) {
        ctx.received_at
    } else {
        ts
    };
    ev.insert("timestamp".into(), Value::String(ts.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)));
    ev.insert(
        "received".into(),
        Value::String(ctx.received_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    );

    // level / platform
    let level = ev.get("level").and_then(Value::as_str).map(normalize_level).unwrap_or("error");
    ev.insert("level".into(), Value::String(level.into()));
    if ev.get("platform").and_then(Value::as_str).is_none_or(str::is_empty) {
        ev.insert("platform".into(), Value::String("other".into()));
    }

    // Stringify scalar string fields that some SDKs send as numbers.
    for key in ["release", "dist", "environment", "server_name", "transaction", "logger"] {
        if let Some(v) = ev.get(key) {
            match v {
                Value::String(s) if s.trim().is_empty() => {
                    ev.remove(key);
                }
                Value::String(_) => {}
                Value::Null => {
                    ev.remove(key);
                }
                other => {
                    let s = other.to_string();
                    ev.insert(key.into(), Value::String(s));
                }
            }
        }
    }

    // exception / threads -> {values: [...]}
    for key in ["exception", "threads"] {
        if let Some(v) = ev.remove(key) {
            let mut values = to_values(v);
            for val in &mut values {
                normalize_exception_like(val);
            }
            if !values.is_empty() {
                ev.insert(key.into(), json!({ "values": values }));
            }
        }
    }
    if let Some(st) = ev.get_mut("stacktrace") {
        normalize_stacktrace(st);
    }

    // message / logentry -> logentry {message?, params?, formatted}
    normalize_logentry(ev);

    // breadcrumbs -> {values: [...]}, capped to the newest N
    if let Some(v) = ev.remove("breadcrumbs") {
        let mut values = to_values(v);
        if values.len() > ctx.max_breadcrumbs {
            values.drain(..values.len() - ctx.max_breadcrumbs);
        }
        ev.insert("breadcrumbs".into(), json!({ "values": values }));
    }

    // tags -> object of string -> string
    if let Some(v) = ev.remove("tags") {
        let tags = normalize_tags(v);
        if !tags.is_empty() {
            ev.insert("tags".into(), Value::Object(tags));
        }
    }

    // fingerprint -> list of strings
    if let Some(v) = ev.remove("fingerprint") {
        if let Value::Array(items) = v {
            let fp: Vec<Value> = items
                .into_iter()
                .filter_map(|i| match i {
                    Value::String(s) => Some(Value::String(s)),
                    Value::Number(n) => Some(Value::String(n.to_string())),
                    Value::Bool(b) => Some(Value::String(b.to_string())),
                    _ => None,
                })
                .collect();
            if !fp.is_empty() {
                ev.insert("fingerprint".into(), Value::Array(fp));
            }
        }
    }

    // user.ip_address = {{auto}}
    if let Some(Value::Object(user)) = ev.get_mut("user") {
        if user.get("ip_address").and_then(Value::as_str) == Some("{{auto}}") {
            match &ctx.client_ip {
                Some(ip) => user.insert("ip_address".into(), Value::String(ip.clone())),
                None => user.remove("ip_address"),
            };
        }
    }

    // contexts: infer browser/os from the request User-Agent when missing.
    let ua = ev
        .get("request")
        .and_then(|r| r.get("headers"))
        .and_then(|h| header_value(h, "user-agent"))
        .map(str::to_string);
    if let Some(ua) = ua {
        let parsed = useragent::parse(&ua);
        let contexts = ev.entry("contexts").or_insert_with(|| json!({}));
        if let Value::Object(c) = contexts {
            if let (false, Some((name, version))) = (c.contains_key("browser"), parsed.browser) {
                c.insert("browser".into(), json!({"type": "browser", "name": name, "version": version}));
            }
            if let (false, Some((name, version))) = (c.contains_key("os"), parsed.os) {
                c.insert("os".into(), json!({"type": "os", "name": name, "version": version}));
            }
        }
    }

    raw
}

impl ProcessedEvent {
    /// Extract columns, tags and the group hash from a normalized (and
    /// optionally symbolicated) event.
    pub fn from_normalized(payload: Value, received_at: DateTime<Utc>) -> Self {
        let s = |k: &str| payload.get(k).and_then(Value::as_str).map(str::to_string);
        let event_id = s("event_id").as_deref().and_then(parse_uuid).unwrap_or_else(Uuid::new_v4);
        let timestamp = payload.get("timestamp").and_then(parse_timestamp).unwrap_or(received_at);
        let meta = grouping::metadata(&payload);
        let group_hash = grouping::group_hash(&payload);
        let message = payload
            .get("logentry")
            .and_then(|l| l.get("formatted").or_else(|| l.get("message")))
            .and_then(Value::as_str)
            .map(|m| truncate(m, 1024));
        let handled = last_exception(&payload)
            .and_then(|e| e.get("mechanism"))
            .and_then(|m| m.get("handled"))
            .and_then(Value::as_bool);
        let user_key = user_key(&payload);
        let sdk = payload.get("sdk");
        let sdk_name = sdk.and_then(|s| s.get("name")).and_then(Value::as_str).map(str::to_string);
        let sdk_version = sdk.and_then(|s| s.get("version")).and_then(Value::as_str).map(str::to_string);
        let tags = derive_tags(&payload, handled, user_key.as_deref());
        let trace = payload.get("contexts").and_then(|c| c.get("trace"));
        let trace_id = trace.and_then(|t| t.get("trace_id")).and_then(Value::as_str).and_then(parse_trace_id);
        let span_id = trace.and_then(|t| t.get("span_id")).and_then(Value::as_str).and_then(parse_span_id);
        ProcessedEvent {
            event_id,
            timestamp,
            received_at,
            level: s("level").unwrap_or_else(|| "error".into()),
            platform: s("platform").unwrap_or_else(|| "other".into()),
            environment: s("environment"),
            release: s("release"),
            dist: s("dist"),
            server_name: s("server_name"),
            transaction: s("transaction"),
            logger: s("logger"),
            message,
            title: meta.title,
            culprit: meta.culprit,
            exception_type: meta.exception_type,
            exception_value: meta.exception_value,
            handled,
            user_key,
            sdk_name,
            sdk_version,
            tags,
            trace_id,
            span_id,
            group_hash,
            payload,
        }
    }
}

/// 32 hex chars (W3C / Sentry trace id) -> UUID. All-zero ids are ignored.
pub fn parse_trace_id(s: &str) -> Option<Uuid> {
    let s = s.trim();
    if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Uuid::try_parse(s).ok().filter(|u| !u.is_nil())
}

/// 16 hex chars -> big-endian u64 (same encoding as OTLP span ids).
pub fn parse_span_id(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.len() != 16 {
        return None;
    }
    u64::from_str_radix(s, 16).ok().filter(|v| *v != 0)
}

pub fn last_exception(ev: &Value) -> Option<&Value> {
    ev.get("exception")?.get("values")?.as_array()?.last()
}

fn user_key(ev: &Value) -> Option<String> {
    let user = ev.get("user")?;
    for (field, prefix) in [("id", "id"), ("email", "email"), ("username", "username"), ("ip_address", "ip")] {
        match user.get(field) {
            Some(Value::String(v)) if !v.is_empty() => return Some(format!("{prefix}:{v}")),
            Some(Value::Number(n)) => return Some(format!("{prefix}:{n}")),
            _ => {}
        }
    }
    None
}

/// Tags users can facet on: explicit tags plus the ones Sentry derives.
fn derive_tags(ev: &Value, handled: Option<bool>, user_key: Option<&str>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |k: &str, v: &str| {
        if !v.is_empty() && !out.iter().any(|(ek, _)| ek == k) {
            out.push((truncate(k, MAX_TAG_KEY), truncate(v, MAX_TAG_VALUE)));
        }
    };
    if let Some(Value::Object(tags)) = ev.get("tags") {
        for (k, v) in tags {
            if let Some(v) = v.as_str() {
                push(k, v);
            }
        }
    }
    for k in ["level", "environment", "release", "dist", "server_name", "transaction", "logger"] {
        if let Some(v) = ev.get(k).and_then(Value::as_str) {
            push(k, v);
        }
    }
    let ctx = |name: &str, field: &str| {
        ev.get("contexts").and_then(|c| c.get(name)).and_then(|c| c.get(field)).and_then(Value::as_str)
    };
    let with_version = |name: &str| {
        ctx(name, "name").map(|n| match ctx(name, "version") {
            Some(v) if !v.is_empty() => format!("{n} {v}"),
            _ => n.to_string(),
        })
    };
    for name in ["browser", "os", "runtime"] {
        if let Some(v) = with_version(name) {
            push(name, &v);
        }
        if let Some(v) = ctx(name, "name") {
            push(&format!("{name}.name"), v);
        }
    }
    if let Some(v) = ctx("device", "model").or_else(|| ctx("device", "family")) {
        push("device", v);
    }
    if let Some(url) = ev.get("request").and_then(|r| r.get("url")).and_then(Value::as_str) {
        push("url", url);
    }
    if let Some(h) = handled {
        push("handled", if h { "yes" } else { "no" });
    }
    if let Some(m) =
        last_exception(ev).and_then(|e| e.get("mechanism")).and_then(|m| m.get("type")).and_then(Value::as_str)
    {
        push("mechanism", m);
    }
    if let Some(u) = user_key {
        push("user", u);
    }
    out
}

pub fn normalize_level(l: &str) -> &'static str {
    match l.to_ascii_lowercase().as_str() {
        "fatal" | "critical" => "fatal",
        "warning" | "warn" => "warning",
        "info" | "log" => "info",
        "debug" | "trace" => "debug",
        _ => "error",
    }
}

pub fn parse_timestamp(v: &Value) -> Option<DateTime<Utc>> {
    match v {
        Value::Number(n) => {
            let f = n.as_f64()?;
            let secs = f.floor() as i64;
            let nanos = ((f - f.floor()) * 1e9) as u32;
            Utc.timestamp_opt(secs, nanos).single()
        }
        Value::String(s) => {
            let s = s.trim();
            if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                return Some(dt.with_timezone(&Utc));
            }
            for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
                if let Ok(n) = NaiveDateTime::parse_from_str(s, fmt) {
                    return Some(n.and_utc());
                }
            }
            s.parse::<f64>().ok().and_then(|f| parse_timestamp(&json!(f)))
        }
        _ => None,
    }
}

/// `{values: [...]}`, `[...]` or a single object -> list.
fn to_values(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.into_iter().filter(Value::is_object).collect(),
        Value::Object(mut o) => match o.remove("values") {
            Some(Value::Array(a)) => a.into_iter().filter(Value::is_object).collect(),
            Some(_) | None if o.is_empty() => vec![],
            Some(_) | None => vec![Value::Object(o)],
        },
        _ => vec![],
    }
}

fn normalize_exception_like(v: &mut Value) {
    let Some(o) = v.as_object_mut() else { return };
    if let Some(val) = o.get("value") {
        if !val.is_string() && !val.is_null() {
            let s = val.to_string();
            o.insert("value".into(), Value::String(s));
        }
    }
    if let Some(st) = o.get_mut("stacktrace") {
        normalize_stacktrace(st);
    }
}

fn normalize_stacktrace(st: &mut Value) {
    if let Value::Array(frames) = st {
        let frames = std::mem::take(frames);
        *st = json!({ "frames": frames });
    }
    let Some(o) = st.as_object_mut() else { return };
    if let Some(Value::Array(frames)) = o.get_mut("frames") {
        frames.retain(Value::is_object);
        for f in frames.iter_mut() {
            let fo = f.as_object_mut().expect("object");
            for k in ["lineno", "colno"] {
                if let Some(Value::String(s)) = fo.get(k) {
                    match s.parse::<u64>() {
                        Ok(n) => fo.insert(k.into(), json!(n)),
                        Err(_) => fo.remove(k),
                    };
                }
            }
            if fo.get("filename").is_none() {
                if let Some(abs) = fo.get("abs_path").and_then(Value::as_str) {
                    let f = filename_from_abs_path(abs);
                    fo.insert("filename".into(), Value::String(f));
                }
            }
        }
    }
}

/// `https://example.com/static/app.js?v=1` -> `/static/app.js`.
pub fn filename_from_abs_path(abs: &str) -> String {
    if let Ok(u) = url::Url::parse(abs) {
        if u.scheme() == "http" || u.scheme() == "https" {
            return u.path().to_string();
        }
        if u.scheme() == "file" {
            return u.path().to_string();
        }
    }
    abs.to_string()
}

fn normalize_logentry(ev: &mut Map<String, Value>) {
    let message = ev.remove("message");
    let mut logentry = match ev.remove("logentry") {
        Some(Value::Object(o)) => o,
        Some(Value::String(s)) => {
            let mut m = Map::new();
            m.insert("formatted".into(), Value::String(s));
            m
        }
        _ => Map::new(),
    };
    match message {
        Some(Value::String(s)) if !s.is_empty() => {
            if !logentry.contains_key("formatted") {
                logentry.insert("formatted".into(), Value::String(s));
            }
        }
        Some(Value::Object(o)) => {
            for (k, v) in o {
                logentry.entry(k).or_insert(v);
            }
        }
        _ => {}
    }
    if logentry.is_empty() {
        return;
    }
    if !logentry.get("formatted").is_some_and(Value::is_string) {
        if let Some(Value::String(tpl)) = logentry.get("message") {
            let params = logentry.get("params").and_then(Value::as_array).cloned().unwrap_or_default();
            let formatted = format_params(tpl, &params);
            logentry.insert("formatted".into(), Value::String(formatted));
        }
    }
    if logentry.get("formatted").is_some_and(Value::is_string) {
        ev.insert("logentry".into(), Value::Object(logentry));
    }
}

/// Substitute `%s`/`%d`/`{}` placeholders with params, in order.
fn format_params(tpl: &str, params: &[Value]) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut it = params.iter();
    let mut chars = tpl.chars().peekable();
    while let Some(c) = chars.next() {
        let placeholder = match (c, chars.peek()) {
            ('%', Some('s' | 'd' | 'i' | 'f' | 'r')) => true,
            ('{', Some('}')) => true,
            _ => false,
        };
        if placeholder {
            if let Some(p) = it.next() {
                chars.next();
                match p {
                    Value::String(s) => out.push_str(s),
                    other => out.push_str(&other.to_string()),
                }
                continue;
            }
        }
        out.push(c);
    }
    out
}

fn normalize_tags(v: Value) -> Map<String, Value> {
    let pairs: Vec<(String, Value)> = match v {
        Value::Object(o) => o.into_iter().collect(),
        Value::Array(a) => a
            .into_iter()
            .filter_map(|p| match p {
                Value::Array(mut kv) if kv.len() == 2 => {
                    let v = kv.pop()?;
                    let k = kv.pop()?;
                    Some((k.as_str()?.to_string(), v))
                }
                _ => None,
            })
            .collect(),
        _ => vec![],
    };
    pairs
        .into_iter()
        .filter_map(|(k, v)| {
            let v = match v {
                Value::String(s) => s,
                Value::Null => return None,
                other => other.to_string(),
            };
            let k = k.trim();
            (!k.is_empty() && !v.is_empty())
                .then(|| (truncate(k, MAX_TAG_KEY), Value::String(truncate(&v, MAX_TAG_VALUE))))
        })
        .collect()
}

/// Headers are either an object or a list of `[name, value]` pairs.
fn header_value<'a>(headers: &'a Value, name: &str) -> Option<&'a str> {
    match headers {
        Value::Object(o) => o.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).and_then(|(_, v)| v.as_str()),
        Value::Array(a) => a.iter().find_map(|p| {
            let p = p.as_array()?;
            (p.first()?.as_str()?.eq_ignore_ascii_case(name)).then(|| p.get(1)?.as_str()).flatten()
        }),
        _ => None,
    }
}

pub fn truncate(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> NormalizeContext {
        NormalizeContext::new(Utc.with_ymd_and_hms(2026, 10, 31, 12, 0, 0).unwrap())
    }

    #[test]
    fn normalizes_python_style_event() {
        let raw = json!({
            "event_id": "9ec79c33-ec99-42ab-8353-589fcb2e04dc",
            "timestamp": 1793448000.5,
            "level": "warn",
            "platform": "python",
            "message": "hello %s",
            "tags": [["a", "1"], ["b", 2]],
            "exception": [{"type": "ValueError", "value": 3, "stacktrace": [{"function": "f", "lineno": "10"}]}],
            "breadcrumbs": [{"message": "x"}],
            "user": {"ip_address": "{{auto}}"}
        });
        let mut c = ctx();
        c.client_ip = Some("1.2.3.4".into());
        let ev = normalize_event(raw, &c);
        assert_eq!(ev["event_id"], "9ec79c33ec9942ab8353589fcb2e04dc");
        assert_eq!(ev["level"], "warning");
        assert_eq!(ev["timestamp"], "2026-10-31T12:00:00.500Z");
        assert_eq!(ev["logentry"]["formatted"], "hello %s");
        assert_eq!(ev["tags"]["b"], "2");
        assert_eq!(ev["exception"]["values"][0]["value"], "3");
        assert_eq!(ev["exception"]["values"][0]["stacktrace"]["frames"][0]["lineno"], 10);
        assert_eq!(ev["breadcrumbs"]["values"][0]["message"], "x");
        assert_eq!(ev["user"]["ip_address"], "1.2.3.4");
    }

    #[test]
    fn clamps_future_timestamps_and_defaults() {
        let ev = normalize_event(json!({"timestamp": "2099-01-01T00:00:00Z"}), &ctx());
        assert_eq!(ev["timestamp"], "2026-10-31T12:00:00.000Z");
        assert_eq!(ev["level"], "error");
        assert_eq!(ev["platform"], "other");
        assert!(ev["event_id"].as_str().unwrap().len() == 32);
    }

    #[test]
    fn logentry_params_are_formatted() {
        let ev = normalize_event(json!({"logentry": {"message": "user %s failed {}", "params": ["bob", 3]}}), &ctx());
        assert_eq!(ev["logentry"]["formatted"], "user bob failed 3");
    }

    #[test]
    fn infers_browser_from_user_agent() {
        let ev = normalize_event(
            json!({"platform": "javascript", "request": {"url": "https://x.test/a", "headers": {"User-Agent": "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0"}}}),
            &ctx(),
        );
        assert_eq!(ev["contexts"]["browser"]["name"], "Firefox");
        let p = ProcessedEvent::from_normalized(ev, ctx().received_at);
        assert!(p.tags.contains(&("browser".into(), "Firefox 131.0".into())));
        assert!(p.tags.contains(&("url".into(), "https://x.test/a".into())));
    }

    #[test]
    fn processed_event_columns() {
        let ev = normalize_event(
            json!({
                "exception": {"values": [{"type": "TypeError", "value": "x is undefined", "mechanism": {"type": "onerror", "handled": false}}]},
                "user": {"email": "a@b.c"},
                "release": 12,
                "sdk": {"name": "sentry.javascript.browser", "version": "10.0.0"}
            }),
            &ctx(),
        );
        let p = ProcessedEvent::from_normalized(ev, ctx().received_at);
        assert_eq!(p.title, "TypeError: x is undefined");
        assert_eq!(p.handled, Some(false));
        assert_eq!(p.user_key.as_deref(), Some("email:a@b.c"));
        assert_eq!(p.release.as_deref(), Some("12"));
        assert_eq!(p.sdk_name.as_deref(), Some("sentry.javascript.browser"));
        assert!(p.tags.contains(&("handled".into(), "no".into())));
        assert!(p.tags.contains(&("mechanism".into(), "onerror".into())));
        assert_eq!(p.trace_id, None);
    }

    #[test]
    fn trace_context() {
        let ev = normalize_event(
            json!({"contexts": {"trace": {"trace_id": "4bf92f3577b34da6a3ce929d0e0e4736", "span_id": "00f067aa0ba902b7"}}}),
            &ctx(),
        );
        let p = ProcessedEvent::from_normalized(ev, ctx().received_at);
        assert_eq!(p.trace_id.unwrap().simple().to_string(), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(p.span_id, Some(0x00f067aa0ba902b7));
        assert_eq!(parse_trace_id("00000000000000000000000000000000"), None);
        assert_eq!(parse_span_id("xyz"), None);
    }
}
