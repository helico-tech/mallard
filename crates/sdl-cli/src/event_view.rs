//! Read the parts of a normalized Sentry event payload that the CLI and TUI
//! show: exception chain, stack frames, breadcrumbs, user and request.
//! Everything is optional in the protocol, so every accessor is lenient.

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use std::fmt::Write as _;

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Frame {
    pub function: Option<String>,
    pub module: Option<String>,
    pub filename: Option<String>,
    pub lineno: Option<u64>,
    pub colno: Option<u64>,
    pub in_app: bool,
    pub context_line: Option<String>,
    pub pre_context: Vec<String>,
    pub post_context: Vec<String>,
}

impl Frame {
    fn from_value(v: &Value) -> Self {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let lines = |k: &str| {
            v.get(k)
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|l| l.as_str().unwrap_or_default().to_string()).collect())
                .unwrap_or_default()
        };
        Frame {
            function: s("function"),
            module: s("module"),
            filename: s("filename").or_else(|| s("abs_path")),
            lineno: v.get("lineno").and_then(Value::as_u64),
            colno: v.get("colno").and_then(Value::as_u64),
            in_app: v.get("in_app").and_then(Value::as_bool).unwrap_or(false),
            context_line: s("context_line"),
            pre_context: lines("pre_context"),
            post_context: lines("post_context"),
        }
    }

    /// Function name, or a placeholder for anonymous frames.
    pub(crate) fn function_name(&self) -> &str {
        match self.function.as_deref() {
            Some(f) if !f.is_empty() && f != "?" => f,
            _ => "<anonymous>",
        }
    }

    /// `file:line:col` (or the module when there is no file name).
    pub(crate) fn location(&self) -> String {
        let mut s = self.filename.clone().or_else(|| self.module.clone()).unwrap_or_else(|| "<unknown>".into());
        if let Some(l) = self.lineno {
            let _ = write!(s, ":{l}");
            if let Some(c) = self.colno {
                let _ = write!(s, ":{c}");
            }
        }
        s
    }

    /// Source lines around the frame: `(line number, text, is error line)`.
    pub(crate) fn context(&self) -> Vec<(Option<u64>, String, bool)> {
        let Some(line) = &self.context_line else {
            return vec![];
        };
        let n = self.lineno;
        let pre = self.pre_context.len() as u64;
        let mut out = Vec::new();
        for (i, l) in self.pre_context.iter().enumerate() {
            out.push((n.map(|n| n.saturating_sub(pre - i as u64)), l.clone(), false));
        }
        out.push((n, line.clone(), true));
        for (i, l) in self.post_context.iter().enumerate() {
            out.push((n.map(|n| n.saturating_add(1 + i as u64)), l.clone(), false));
        }
        out
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Exception {
    pub ty: String,
    pub value: Option<String>,
    pub mechanism: Option<String>,
    pub handled: Option<bool>,
    /// As sent: oldest call first. Use [`Exception::frames_newest_first`].
    pub frames: Vec<Frame>,
    /// Frames before symbolication (`raw_stacktrace`), when it changed them.
    pub raw_frames: Option<Vec<Frame>>,
}

impl Exception {
    pub(crate) fn frames_newest_first(&self, raw: bool) -> Vec<&Frame> {
        let src = if raw { self.raw_frames.as_ref().unwrap_or(&self.frames) } else { &self.frames };
        src.iter().rev().collect()
    }

    /// "onerror · unhandled" (or just the mechanism), when it is known.
    pub(crate) fn mechanism_label(&self) -> Option<String> {
        let m = self.mechanism.as_ref()?;
        let handled = match self.handled {
            Some(false) => " · unhandled",
            Some(true) => " · handled",
            None => "",
        };
        Some(format!("{m}{handled}"))
    }

    /// "TypeError: boom" (or just the type).
    pub(crate) fn headline(&self) -> String {
        match &self.value {
            Some(v) if !v.is_empty() => format!("{}: {v}", self.ty),
            _ => self.ty.clone(),
        }
    }
}

fn frames_of(stacktrace: Option<&Value>) -> Vec<Frame> {
    stacktrace
        .and_then(|s| s.get("frames"))
        .and_then(Value::as_array)
        .map(|a| a.iter().map(Frame::from_value).collect())
        .unwrap_or_default()
}

fn values(v: Option<&Value>) -> &[Value] {
    match v {
        Some(Value::Array(a)) => a,
        Some(o) => o.get("values").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]),
        None => &[],
    }
}

/// The exception chain, primary (outermost) exception first. Events without
/// an exception but with a thread or top-level stack trace get one
/// pseudo-exception so their frames can still be shown.
pub(crate) fn exceptions(payload: &Value) -> Vec<Exception> {
    let mut out: Vec<Exception> = values(payload.get("exception"))
        .iter()
        .map(|e| {
            let mech = e.get("mechanism");
            Exception {
                ty: e.get("type").and_then(Value::as_str).unwrap_or("Error").to_string(),
                value: e.get("value").and_then(Value::as_str).map(str::to_string),
                mechanism: mech.and_then(|m| m.get("type")).and_then(Value::as_str).map(str::to_string),
                handled: mech.and_then(|m| m.get("handled")).and_then(Value::as_bool),
                frames: frames_of(e.get("stacktrace")),
                raw_frames: e.get("raw_stacktrace").map(|r| frames_of(Some(r))),
            }
        })
        .collect();
    out.reverse();
    if out.is_empty() {
        let threads = values(payload.get("threads"));
        let thread = threads
            .iter()
            .find(|t| t.get("crashed").and_then(Value::as_bool) == Some(true) && t.get("stacktrace").is_some())
            .or_else(|| threads.iter().find(|t| t.get("stacktrace").is_some()));
        let st = thread.and_then(|t| t.get("stacktrace")).or_else(|| payload.get("stacktrace"));
        let frames = frames_of(st);
        if !frames.is_empty() {
            out.push(Exception {
                ty: "Stack trace".into(),
                value: message(payload),
                frames,
                raw_frames: thread.and_then(|t| t.get("raw_stacktrace")).map(|r| frames_of(Some(r))),
                ..Default::default()
            });
        }
    }
    out
}

/// `logentry.formatted` / `logentry.message` / `message`.
pub(crate) fn message(payload: &Value) -> Option<String> {
    let le = payload.get("logentry");
    le.and_then(|l| l.get("formatted"))
        .or_else(|| le.and_then(|l| l.get("message")))
        .or_else(|| payload.get("message"))
        .and_then(|m| match m {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => {
                o.get("formatted").or_else(|| o.get("message")).and_then(Value::as_str).map(str::to_string)
            }
            _ => None,
        })
        .filter(|s| !s.is_empty())
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Breadcrumb {
    pub timestamp: Option<DateTime<Utc>>,
    pub ty: Option<String>,
    pub category: Option<String>,
    pub level: String,
    pub message: Option<String>,
    pub data: Option<Value>,
}

impl Breadcrumb {
    /// Message, or a compact rendering of `data` (e.g. `GET /api -> 500`).
    pub(crate) fn text(&self) -> String {
        if let Some(m) = &self.message {
            return m.clone();
        }
        match &self.data {
            Some(Value::Object(o)) => {
                let get = |k: &str| o.get(k).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()));
                if let (Some(method), Some(url)) = (get("method"), get("url")) {
                    let code = get("status_code").map(|c| format!(" -> {c}")).unwrap_or_default();
                    return format!("{method} {url}{code}");
                }
                o.iter()
                    .map(|(k, v)| format!("{k}={}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            Some(v) => v.to_string(),
            None => String::new(),
        }
    }
}

fn parse_ts(v: &Value) -> Option<DateTime<Utc>> {
    match v {
        Value::Number(n) => {
            // floor, not trunc: -1.5 is 2 s before the epoch plus 0.5 s.
            let f = n.as_f64()?;
            let secs = f.floor();
            Utc.timestamp_opt(secs as i64, ((f - secs) * 1e9) as u32).single()
        }
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&Utc))
            .ok()
            .or_else(|| s.parse::<f64>().ok().and_then(|f| parse_ts(&serde_json::json!(f)))),
        _ => None,
    }
}

/// Breadcrumbs, newest first.
pub(crate) fn breadcrumbs(payload: &Value) -> Vec<Breadcrumb> {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let mut out: Vec<Breadcrumb> = values(payload.get("breadcrumbs"))
        .iter()
        .map(|b| Breadcrumb {
            timestamp: b.get("timestamp").and_then(parse_ts),
            ty: s(b, "type"),
            category: s(b, "category"),
            level: s(b, "level").unwrap_or_else(|| "info".into()),
            message: s(b, "message"),
            data: b.get("data").cloned(),
        })
        .collect();
    out.reverse();
    out
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Object(_) | Value::Array(_) => Some(v.to_string()),
        other => Some(other.to_string()),
    }
}

/// `user` fields as key/value pairs (id, email, username, ip_address, ...).
pub(crate) fn user(payload: &Value) -> Vec<(String, String)> {
    let Some(Value::Object(u)) = payload.get("user") else {
        return vec![];
    };
    u.iter().filter_map(|(k, v)| scalar(v).map(|s| (k.clone(), s))).collect()
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Request {
    pub method: Option<String>,
    pub url: Option<String>,
    pub query: Option<String>,
    pub headers: Vec<(String, String)>,
}

pub(crate) fn request(payload: &Value) -> Option<Request> {
    let r = payload.get("request")?.as_object()?;
    let headers = match r.get("headers") {
        Some(Value::Object(h)) => h.iter().filter_map(|(k, v)| scalar(v).map(|s| (k.clone(), s))).collect(),
        Some(Value::Array(a)) => {
            a.iter().filter_map(|p| Some((p.get(0)?.as_str()?.to_string(), scalar(p.get(1)?)?))).collect()
        }
        _ => vec![],
    };
    let query = match r.get("query_string") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Array(a)) if !a.is_empty() => Some(
            a.iter()
                .filter_map(|p| Some(format!("{}={}", p.get(0)?.as_str()?, p.get(1)?.as_str()?)))
                .collect::<Vec<_>>()
                .join("&"),
        ),
        Some(Value::Object(o)) if !o.is_empty() => {
            Some(o.iter().map(|(k, v)| format!("{k}={}", scalar(v).unwrap_or_default())).collect::<Vec<_>>().join("&"))
        }
        _ => None,
    };
    Some(Request {
        method: r.get("method").and_then(Value::as_str).map(str::to_string),
        url: r.get("url").and_then(Value::as_str).map(str::to_string),
        query,
        headers,
    })
}

/// Short summaries of well-known contexts: runtime, os, browser, device.
pub(crate) fn contexts(payload: &Value) -> Vec<(String, String)> {
    let Some(c) = payload.get("contexts") else {
        return vec![];
    };
    let mut out = Vec::new();
    for key in ["runtime", "browser", "os", "device"] {
        let Some(ctx) = c.get(key) else { continue };
        let name = ctx.get("name").and_then(Value::as_str);
        let version = ctx.get("version").and_then(Value::as_str);
        let text = match (name, version) {
            (Some(n), Some(v)) => format!("{n} {v}"),
            (Some(n), None) => n.to_string(),
            _ => match ctx.get("model").or_else(|| ctx.get("arch")).and_then(Value::as_str) {
                Some(m) => m.to_string(),
                None => continue,
            },
        };
        out.push((key.to_string(), text));
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn timestamps_from_numbers_strings_and_rfc3339() {
        let t = |v: Value| parse_ts(&v).map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        assert_eq!(t(json!(1_700_000_000)).as_deref(), Some("2023-11-14T22:13:20.000Z"));
        assert_eq!(t(json!(1_700_000_000.25)).as_deref(), Some("2023-11-14T22:13:20.250Z"));
        assert_eq!(t(json!("1700000000.25")).as_deref(), Some("2023-11-14T22:13:20.250Z"));
        assert_eq!(t(json!("2026-10-31T12:00:00.5+02:00")).as_deref(), Some("2026-10-31T10:00:00.500Z"));
        assert_eq!(t(json!(-1.5)).as_deref(), Some("1969-12-31T23:59:58.500Z"));
        assert_eq!(t(json!("yesterday")), None);
        assert_eq!(t(json!(true)), None);
    }

    pub(crate) fn node_payload() -> Value {
        json!({
            "event_id": "655354d4abd54c31a077367796352d8c",
            "platform": "node",
            "level": "error",
            "exception": {"values": [
                {"type": "TypeError", "value": "root cause",
                 "mechanism": {"type": "chained", "handled": true},
                 "stacktrace": {"frames": [
                    {"function": "main", "filename": "node:internal/main", "lineno": 1, "in_app": false}
                 ]}},
                {"type": "Error", "value": "inside span",
                 "mechanism": {"type": "generic", "handled": false},
                 "stacktrace": {"frames": [
                    {"function": "runMain", "filename": "node:internal/modules/run_main", "lineno": 98, "colno": 5, "in_app": false},
                    {"function": "parseConfig", "filename": "/app/smoke.mjs", "lineno": 11, "colno": 84, "in_app": true,
                     "pre_context": ["line nine", "line ten"], "context_line": "throw new Error(\"inside span\")",
                     "post_context": ["line twelve"]}
                 ]},
                 "raw_stacktrace": {"frames": [
                    {"function": "n", "filename": "/app/smoke.min.js", "lineno": 1, "colno": 999, "in_app": true}
                 ]}}
            ]},
            "breadcrumbs": {"values": [
                {"category": "auth", "level": "info", "message": "user logged in", "timestamp": 1793480353.983},
                {"category": "http", "type": "http", "timestamp": "2026-10-31T20:59:14Z",
                 "data": {"method": "GET", "url": "/api/cart", "status_code": 500}}
            ]},
            "user": {"id": "user-42", "email": "u@example.com"},
            "request": {"method": "POST", "url": "https://shop.example/checkout", "query_string": [["a", "1"]],
                        "headers": [["User-Agent", "curl/8"]]},
            "contexts": {"runtime": {"name": "node", "version": "v24.22.1"}, "os": {"name": "macOS", "version": "27.0.1"},
                         "trace": {"trace_id": "b7644661850f40e69380d8c9ea253e84", "span_id": "b62d7c6abb676108"}},
            "tags": {"tenant": "acme"}
        })
    }

    #[test]
    fn exception_chain_primary_first_and_frames_newest_first() {
        let ex = exceptions(&node_payload());
        assert_eq!(ex.len(), 2);
        assert_eq!(ex[0].headline(), "Error: inside span");
        assert_eq!(ex[0].handled, Some(false));
        assert_eq!(ex[1].ty, "TypeError");
        let frames = ex[0].frames_newest_first(false);
        assert_eq!(frames[0].function_name(), "parseConfig");
        assert!(frames[0].in_app);
        assert_eq!(frames[0].location(), "/app/smoke.mjs:11:84");
        let raw = ex[0].frames_newest_first(true);
        assert_eq!(raw[0].location(), "/app/smoke.min.js:1:999");
        let ctx = frames[0].context();
        assert_eq!(ctx.len(), 4);
        assert_eq!(ctx[0].0, Some(9));
        assert_eq!(ctx[2], (Some(11), "throw new Error(\"inside span\")".to_string(), true));
        assert_eq!(ctx[3].0, Some(12));
    }

    #[test]
    fn huge_line_numbers_do_not_overflow() {
        let f = Frame::from_value(&json!({
            "lineno": u64::MAX, "pre_context": ["a", "b"], "context_line": "c", "post_context": ["d", "e"]
        }));
        let ctx = f.context();
        assert_eq!(ctx.len(), 5);
        assert_eq!(ctx[0].0, Some(u64::MAX - 2));
        assert_eq!(ctx[1].0, Some(u64::MAX - 1));
        assert_eq!(ctx[2].0, Some(u64::MAX));
        assert_eq!(ctx[4].0, Some(u64::MAX));
    }

    #[test]
    fn thread_stacktrace_fallback() {
        let p = json!({"message": "hello", "threads": {"values": [
            {"crashed": true, "stacktrace": {"frames": [{"function": "f", "lineno": 3}]}}
        ]}});
        let ex = exceptions(&p);
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0].headline(), "Stack trace: hello");
        assert_eq!(ex[0].frames[0].function_name(), "f");
        assert!(exceptions(&json!({})).is_empty());
    }

    #[test]
    fn breadcrumbs_newest_first_with_both_timestamp_forms() {
        let b = breadcrumbs(&node_payload());
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].text(), "GET /api/cart -> 500");
        assert_eq!(b[1].text(), "user logged in");
        assert!(b[0].timestamp.is_some() && b[1].timestamp.is_some());
    }

    #[test]
    fn user_request_contexts_message() {
        let p = node_payload();
        assert!(user(&p).contains(&("email".into(), "u@example.com".into())));
        let r = request(&p).unwrap();
        assert_eq!(r.method.as_deref(), Some("POST"));
        assert_eq!(r.query.as_deref(), Some("a=1"));
        assert_eq!(r.headers, vec![("User-Agent".into(), "curl/8".into())]);
        assert_eq!(
            contexts(&p),
            vec![("runtime".into(), "node v24.22.1".into()), ("os".into(), "macOS 27.0.1".into())]
        );
        assert_eq!(
            message(&json!({"logentry": {"formatted": "Payment 123 failed"}})).as_deref(),
            Some("Payment 123 failed")
        );
        assert_eq!(message(&p), None);
    }
}
