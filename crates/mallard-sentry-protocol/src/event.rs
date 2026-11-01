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
/// Explicit tags kept per event (Relay keeps a similar number).
pub const MAX_TAGS: usize = 200;
/// Exceptions in a chain / threads kept per event.
pub const MAX_EXCEPTIONS: usize = 32;
/// Frames kept per stack trace (the first and last half, like Sentry).
pub const MAX_FRAMES: usize = 250;
/// An event `timestamp` more than this many seconds after receive time is
/// replaced by the receive time (Relay's `max_secs_in_future` default).
pub const MAX_SECS_IN_FUTURE: i64 = 60;
/// An event `timestamp` more than this many seconds before receive time is
/// replaced by the receive time. Relay uses the event retention; 30 days is
/// a fixed bound, so a window query on `timestamp` stays near `received_at`.
pub const MAX_SECS_IN_PAST: i64 = 30 * 86_400;

/// When the envelope's `sent_at` differs this much from the receive time, the
/// client's clock is wrong and event timestamps are shifted by the difference
/// (Relay's `MINIMUM_CLOCK_DRIFT`).
pub const MIN_CLOCK_DRIFT_SECS: i64 = 55 * 60;

#[derive(Debug, Clone)]
pub struct NormalizeContext {
    pub received_at: DateTime<Utc>,
    /// `sent_at` of the envelope header: when the SDK sent it, by its clock.
    pub sent_at: Option<DateTime<Utc>>,
    /// IP of the client, for `user.ip_address` when the SDK asks (see [`client_address`]).
    pub client_ip: Option<String>,
    /// `event_id` from the envelope header, used when the payload has none.
    pub event_id_hint: Option<String>,
    pub max_breadcrumbs: usize,
}

impl NormalizeContext {
    pub fn new(received_at: DateTime<Utc>) -> Self {
        Self { received_at, sent_at: None, client_ip: None, event_id_hint: None, max_breadcrumbs: 100 }
    }

    /// How far the client's clock is off, when that is a real difference.
    fn clock_drift(&self) -> Option<Duration> {
        let drift = self.received_at.signed_duration_since(self.sent_at?);
        (drift.num_seconds().abs() >= MIN_CLOCK_DRIFT_SECS).then_some(drift)
    }
}

/// What an SDK says about inferring the client's address: `sdk.settings.infer_ip`
/// of an event or transaction (`sdk` is that field).
pub fn sdk_infer_ip(sdk: Option<&Value>) -> Option<&str> {
    sdk?.get("settings")?.get("infer_ip")?.as_str()
}

/// The client address to store, or `None` to store none. This is personal data,
/// so the address of the request (`request`) is only used when the SDK asks for
/// it: `sent` is `{{auto}}`, or `infer_ip` is `auto`. With `infer_ip = never`
/// nothing is stored, not even an address the SDK sent. An address the SDK sent
/// is otherwise kept. Without `infer_ip` nothing is added, whatever the platform.
pub fn client_address<'a>(sent: Option<&'a str>, infer_ip: Option<&str>, request: Option<&'a str>) -> Option<&'a str> {
    match (infer_ip, sent) {
        (Some("never"), _) => None,
        (_, Some("{{auto}}")) | (Some("auto"), None) => request,
        (_, sent) => sent,
    }
}

/// `t` corrected for the client's clock; unchanged on overflow.
fn shifted(t: DateTime<Utc>, drift: Option<Duration>) -> DateTime<Utc> {
    drift.and_then(|d| t.checked_add_signed(d)).unwrap_or(t)
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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

    // timestamp: the event time drives first/last seen and trends, so keep
    // it, but correct a client clock that is off (by `sent_at`) and replace
    // what is still out of range with the receive time, like Relay does.
    let drift = ctx.clock_drift();
    let ts = ev.get("timestamp").and_then(parse_timestamp).map_or(ctx.received_at, |t| shifted(t, drift));
    let ts = if ts > ctx.received_at + Duration::seconds(MAX_SECS_IN_FUTURE)
        || ts < ctx.received_at - Duration::seconds(MAX_SECS_IN_PAST)
    {
        ctx.received_at
    } else {
        ts
    };
    ev.insert("timestamp".into(), Value::String(rfc3339(ts)));
    ev.insert("received".into(), Value::String(rfc3339(ctx.received_at)));

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
            if values.len() > MAX_EXCEPTIONS {
                // Keep the newest (last) ones: they carry the title.
                values.drain(..values.len() - MAX_EXCEPTIONS);
            }
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
        if drift.is_some() {
            for crumb in values.iter_mut().filter_map(Value::as_object_mut) {
                if let Some(t) = crumb.get("timestamp").and_then(parse_timestamp) {
                    crumb.insert("timestamp".into(), Value::String(rfc3339(shifted(t, drift))));
                }
            }
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
    if let Some(v) = ev.remove("fingerprint")
        && let Value::Array(items) = v
    {
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

    // user.ip_address = {{auto}}, or sdk.settings.infer_ip
    let sent = ev.get("user").and_then(|u| u.get("ip_address")).and_then(Value::as_str);
    match client_address(sent, sdk_infer_ip(ev.get("sdk")), ctx.client_ip.as_deref()).map(str::to_string) {
        Some(ip) => {
            let user = ev.entry("user").or_insert(Value::Null);
            if user.is_null() {
                *user = json!({});
            }
            if let Value::Object(user) = user {
                user.insert("ip_address".into(), Value::String(ip));
            }
        }
        None => {
            if let Some(Value::Object(user)) = ev.get_mut("user") {
                user.remove("ip_address");
            }
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
            if let (false, Some(b)) = (c.contains_key("browser"), parsed.browser) {
                c.insert("browser".into(), json!({"type": "browser", "name": b.name, "version": b.version}));
            }
            if let (false, Some(os)) = (c.contains_key("os"), parsed.os) {
                c.insert("os".into(), json!({"type": "os", "name": os.name, "version": os.version}));
            }
        }
    }

    raw
}

impl ProcessedEvent {
    /// Extract columns, tags and the group hash from a normalized (and
    /// optionally symbolicated) event.
    pub fn from_normalized(payload: &Value, received_at: DateTime<Utc>) -> Self {
        let s = |k: &str| payload.get(k).and_then(Value::as_str).map(str::to_string);
        let event_id = s("event_id").as_deref().and_then(parse_uuid).unwrap_or_else(Uuid::new_v4);
        let timestamp = payload.get("timestamp").and_then(parse_timestamp).unwrap_or(received_at);
        let meta = grouping::metadata(payload);
        let group_hash = grouping::group_hash(payload);
        let message = payload
            .get("logentry")
            .and_then(|l| l.get("formatted").or_else(|| l.get("message")))
            .and_then(Value::as_str)
            .map(|m| truncate(m, 1024));
        let handled = last_exception(payload)
            .and_then(|e| e.get("mechanism"))
            .and_then(|m| m.get("handled"))
            .and_then(Value::as_bool);
        let user_key = user_key(payload);
        let sdk = payload.get("sdk");
        let sdk_name = sdk.and_then(|s| s.get("name")).and_then(Value::as_str).map(str::to_string);
        let sdk_version = sdk.and_then(|s| s.get("version")).and_then(Value::as_str).map(str::to_string);
        let tags = derive_tags(payload, handled, user_key.as_deref());
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
    // `from_str_radix` alone would also accept a leading `+`.
    if s.len() != 16 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
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
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push = |k: &str, v: &str| {
        if v.is_empty() {
            return;
        }
        // Dedupe on the stored (truncated) key.
        let k = truncate(k, MAX_TAG_KEY);
        if seen.insert(k.clone()) {
            out.push((k, truncate(v, MAX_TAG_VALUE)));
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
    const LEVELS: &[(&str, &str)] = &[
        ("fatal", "fatal"),
        ("critical", "fatal"),
        ("warning", "warning"),
        ("warn", "warning"),
        ("info", "info"),
        ("log", "info"),
        ("debug", "debug"),
        ("trace", "debug"),
    ];
    LEVELS.iter().find(|(name, _)| name.eq_ignore_ascii_case(l)).map_or("error", |(_, level)| level)
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
    if let Some(val) = o.get("value")
        && !val.is_string()
        && !val.is_null()
    {
        let s = val.to_string();
        o.insert("value".into(), Value::String(s));
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
        if frames.len() > MAX_FRAMES {
            // Keep the outermost and innermost frames, drop the middle.
            let keep_tail = MAX_FRAMES / 2;
            let cut_from = MAX_FRAMES - keep_tail;
            let cut_to = frames.len() - keep_tail;
            frames.drain(cut_from..cut_to);
        }
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
            // Like Relay: SDKs that send only `filename` (the browser SDK
            // does) get it moved to `abs_path`, which symbolication and the
            // `debug_meta` lookup key on; URL filenames become their path.
            if fo.get("abs_path").and_then(Value::as_str).is_none_or(str::is_empty)
                && let Some(f) =
                    fo.get("filename").and_then(Value::as_str).filter(|f| !f.is_empty()).map(str::to_string)
            {
                fo.insert("filename".into(), Value::String(filename_from_abs_path(&f)));
                fo.insert("abs_path".into(), Value::String(f));
            }
            if fo.get("filename").is_none()
                && let Some(abs) = fo.get("abs_path").and_then(Value::as_str)
            {
                let f = filename_from_abs_path(abs);
                fo.insert("filename".into(), Value::String(f));
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
    if !logentry.get("formatted").is_some_and(Value::is_string)
        && let Some(Value::String(tpl)) = logentry.get("message")
    {
        let params = logentry.get("params").and_then(Value::as_array).cloned().unwrap_or_default();
        let formatted = format_params(tpl, &params);
        logentry.insert("formatted".into(), Value::String(formatted));
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
        let placeholder = matches!((c, chars.peek()), ('%', Some('s' | 'd' | 'i' | 'f' | 'r')) | ('{', Some('}')));
        if placeholder && let Some(p) = it.next() {
            chars.next();
            match p {
                Value::String(s) => out.push_str(s),
                other => out.push_str(&other.to_string()),
            }
            continue;
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
        .take(MAX_TAGS)
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
    fn client_address_only_when_the_sdk_asks() {
        const REQUEST: Option<&str> = Some("203.0.113.9");
        const AUTO: Option<&str> = Some("{{auto}}");
        const OWN: Option<&str> = Some("198.51.100.7");
        // (sent, infer_ip, request) -> stored
        let cases = [
            (AUTO, None, REQUEST, REQUEST),
            (AUTO, Some("auto"), REQUEST, REQUEST),
            (AUTO, Some("never"), REQUEST, None),
            (AUTO, None, None, None),
            (None, Some("auto"), REQUEST, REQUEST),
            (None, Some("auto"), None, None),
            (None, Some("never"), REQUEST, None),
            (None, None, REQUEST, None),
            (None, Some("legacy"), REQUEST, None),
            (OWN, None, REQUEST, OWN),
            (OWN, Some("auto"), REQUEST, OWN),
            (OWN, Some("never"), REQUEST, None),
        ];
        for (sent, infer_ip, request, want) in cases {
            assert_eq!(client_address(sent, infer_ip, request), want, "{sent:?} {infer_ip:?} {request:?}");
        }
    }

    #[test]
    fn infer_ip_of_an_event() {
        let user = |event: Value, client_ip: Option<&str>| {
            let mut c = ctx();
            c.client_ip = client_ip.map(str::to_string);
            normalize_event(event, &c)["user"].clone()
        };
        let sdk = |infer_ip: &str| json!({"name": "x", "settings": {"infer_ip": infer_ip}});
        let ip = "203.0.113.9";
        assert_eq!(user(json!({"sdk": sdk("auto")}), Some(ip)), json!({"ip_address": ip}));
        assert_eq!(user(json!({"sdk": sdk("auto"), "user": null}), Some(ip)), json!({"ip_address": ip}));
        assert_eq!(
            user(json!({"sdk": sdk("auto"), "user": {"id": "42"}}), Some(ip)),
            json!({"id": "42", "ip_address": ip})
        );
        assert_eq!(user(json!({"sdk": sdk("auto")}), None), Value::Null, "no address known: no user");
        assert_eq!(user(json!({"sdk": sdk("never")}), Some(ip)), Value::Null);
        assert_eq!(user(json!({}), Some(ip)), Value::Null, "the SDK did not ask");
        let auto = json!({"id": "42", "ip_address": "{{auto}}"});
        assert_eq!(user(json!({"sdk": sdk("never"), "user": auto}), Some(ip)), json!({"id": "42"}));
        assert_eq!(user(json!({"user": auto}), None), json!({"id": "42"}));
        let own = json!({"ip_address": "198.51.100.7"});
        assert_eq!(user(json!({"sdk": sdk("auto"), "user": own}), Some(ip)), own);
        assert_eq!(user(json!({"sdk": sdk("never"), "user": own}), Some(ip)), json!({}));
    }

    /// The browser SDK sends only `filename` (a full URL).
    #[test]
    fn filename_only_frames_get_abs_path() {
        let ev = normalize_event(
            json!({"exception": {"values": [{"type": "E", "stacktrace": {"frames": [
                {"filename": "http://127.0.0.1:5173/assets/index-Cc1.js?v=2", "lineno": 17, "colno": 30972},
                {"filename": "/srv/app/main.js", "abs_path": "", "lineno": 1},
                {"abs_path": "https://x.test/a.js", "filename": "a.js", "lineno": 1}
            ]}}]}}),
            &ctx(),
        );
        let f = &ev["exception"]["values"][0]["stacktrace"]["frames"];
        assert_eq!(f[0]["abs_path"], "http://127.0.0.1:5173/assets/index-Cc1.js?v=2");
        assert_eq!(f[0]["filename"], "/assets/index-Cc1.js");
        assert_eq!(f[1]["abs_path"], "/srv/app/main.js");
        assert_eq!(f[1]["filename"], "/srv/app/main.js");
        assert_eq!(f[2]["abs_path"], "https://x.test/a.js");
        assert_eq!(f[2]["filename"], "a.js");
    }

    #[test]
    fn clamps_future_timestamps_and_defaults() {
        let ev = normalize_event(json!({"timestamp": "2099-01-01T00:00:00Z"}), &ctx());
        assert_eq!(ev["timestamp"], "2026-10-31T12:00:00.000Z");
        assert_eq!(ev["level"], "error");
        assert_eq!(ev["platform"], "other");
        assert!(ev["event_id"].as_str().unwrap().len() == 32);
    }

    /// A client clock that is off by more than 55 minutes: `sent_at` tells by
    /// how much. Event and breadcrumbs move together; a smaller difference is
    /// not touched.
    #[test]
    fn clock_drift_shifts_event_and_breadcrumbs() {
        let event = json!({
            "timestamp": "2026-10-31T14:00:00Z",
            "breadcrumbs": [{"timestamp": 1793458780.0, "message": "b"}, {"message": "no time"}],
        });
        let mut c = ctx(); // received at 12:00:00
        c.sent_at = Some("2026-10-31T14:00:10Z".parse().unwrap());
        let ev = normalize_event(event.clone(), &c);
        assert_eq!(ev["timestamp"], "2026-10-31T11:59:50.000Z");
        // 1793458780 is 14:59:40; two hours and ten seconds earlier.
        assert_eq!(ev["breadcrumbs"]["values"][0]["timestamp"], "2026-10-31T12:59:30.000Z");
        assert!(ev["breadcrumbs"]["values"][1].get("timestamp").is_none());

        // Off by less than 55 minutes: no shift.
        c.sent_at = Some("2026-10-31T12:54:00Z".parse().unwrap());
        let ev = normalize_event(json!({"timestamp": "2026-10-31T11:50:00Z", "breadcrumbs": event["breadcrumbs"]}), &c);
        assert_eq!(ev["timestamp"], "2026-10-31T11:50:00.000Z");
        assert_eq!(ev["breadcrumbs"]["values"][0]["timestamp"], 1793458780.0);

        // No timestamp: the receive time, not shifted.
        c.sent_at = Some("2026-10-31T14:00:10Z".parse().unwrap());
        assert_eq!(normalize_event(json!({}), &c)["timestamp"], "2026-10-31T12:00:00.000Z");

        // A garbage sent_at far away must not overflow: left as it is.
        c.sent_at = Some(DateTime::<Utc>::MIN_UTC);
        assert_eq!(
            normalize_event(json!({"timestamp": "2026-10-31T11:50:00Z"}), &c)["timestamp"],
            "2026-10-31T11:50:00.000Z"
        );
    }

    #[test]
    fn timestamps_outside_the_window_get_the_receive_time() {
        let at = |ts: &str| normalize_event(json!({"timestamp": ts}), &ctx())["timestamp"].clone();
        // Replayed or buffered events keep their time.
        assert_eq!(at("2026-10-31T11:10:00Z"), "2026-10-31T11:10:00.000Z");
        assert_eq!(at("2026-10-02T12:00:00Z"), "2026-10-02T12:00:00.000Z");
        assert_eq!(at("2026-10-31T12:01:00Z"), "2026-10-31T12:01:00.000Z");
        // Too far in the past or future: receive time.
        assert_eq!(at("2026-10-01T11:59:59Z"), "2026-10-31T12:00:00.000Z");
        assert_eq!(at("2026-10-31T12:01:01Z"), "2026-10-31T12:00:00.000Z");
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
        let p = ProcessedEvent::from_normalized(&ev, ctx().received_at);
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
        let p = ProcessedEvent::from_normalized(&ev, ctx().received_at);
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
    fn caps_tags_frames_and_exceptions() {
        let tags: serde_json::Map<String, Value> = (0..5000).map(|i| (format!("k{i}"), json!("v"))).collect();
        let frames: Vec<Value> = (0..1000).map(|i| json!({"function": format!("f{i}")})).collect();
        let excs: Vec<Value> =
            (0..100).map(|i| json!({"type": format!("E{i}"), "stacktrace": {"frames": frames}})).collect();
        let started = std::time::Instant::now();
        let ev = normalize_event(json!({"tags": tags, "exception": excs}), &ctx());
        let _ = ProcessedEvent::from_normalized(&ev, ctx().received_at);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(ev["tags"].as_object().unwrap().len(), MAX_TAGS);
        let values = ev["exception"]["values"].as_array().unwrap();
        assert_eq!(values.len(), MAX_EXCEPTIONS);
        assert_eq!(values.last().unwrap()["type"], "E99");
        let fr = values[0]["stacktrace"]["frames"].as_array().unwrap();
        assert_eq!(fr.len(), MAX_FRAMES);
        assert_eq!(fr[0]["function"], "f0");
        assert_eq!(fr.last().unwrap()["function"], "f999");
    }

    #[test]
    fn tags_dedupe_on_the_truncated_key() {
        let long = "k".repeat(MAX_TAG_KEY);
        let ev = json!({"tags": {format!("{long}a"): "1", format!("{long}b"): "2"}});
        let tags = derive_tags(&ev, None, None);
        assert_eq!(tags.iter().filter(|(k, _)| *k == long).count(), 1, "{tags:?}");
    }

    #[test]
    fn levels() {
        assert_eq!(normalize_level("WARN"), "warning");
        assert_eq!(normalize_level("Critical"), "fatal");
        assert_eq!(normalize_level("whatever"), "error");
    }

    #[test]
    fn span_ids_are_strict_hex() {
        assert_eq!(parse_span_id("+00f067aa0ba902b"), None);
        assert_eq!(parse_span_id(" 00f067aa0ba902b7 "), Some(0x00f067aa0ba902b7));
    }

    #[test]
    fn trace_context() {
        let ev = normalize_event(
            json!({"contexts": {"trace": {"trace_id": "4bf92f3577b34da6a3ce929d0e0e4736", "span_id": "00f067aa0ba902b7"}}}),
            &ctx(),
        );
        let p = ProcessedEvent::from_normalized(&ev, ctx().received_at);
        assert_eq!(p.trace_id.unwrap().simple().to_string(), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(p.span_id, Some(0x00f067aa0ba902b7));
        assert_eq!(parse_trace_id("00000000000000000000000000000000"), None);
        assert_eq!(parse_span_id("xyz"), None);
    }
}
