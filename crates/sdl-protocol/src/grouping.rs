//! Issue grouping.
//!
//! Events with the same group hash form one issue. The rules follow
//! Sentry's default grouping closely enough that issues look familiar:
//!
//! 1. A custom `fingerprint` wins. `{{ default }}` in it expands to the
//!    default hash, so `["{{ default }}", "tenant-a"]` splits per tenant.
//! 2. Otherwise each exception in the chain contributes its type plus its
//!    stack frames (only in-app frames when the SDK marked any). A frame is
//!    `module|filename` + `function`, normalized so that hashes in bundle
//!    names, line numbers and anonymous functions do not split issues.
//! 3. Exceptions without frames use type + value with variable parts
//!    (numbers, ids, hex, ...) masked.
//! 4. Then a thread/top-level stack trace, then the message template.

use crate::event::{last_exception, truncate};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
use xxhash_rust::xxh3::Xxh3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub title: String,
    pub culprit: Option<String>,
    pub exception_type: Option<String>,
    pub exception_value: Option<String>,
}

pub fn group_hash(ev: &Value) -> u64 {
    let default = default_components(ev);
    let fingerprint: Option<Vec<&str>> =
        ev.get("fingerprint").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect());
    let mut h = Xxh3::new();
    match fingerprint {
        Some(fp) if !fp.is_empty() && !fp.iter().all(|f| is_default_marker(f)) => {
            h.update(b"fingerprint\0");
            for part in fp {
                if is_default_marker(part) {
                    for c in &default {
                        h.update(c.as_bytes());
                        h.update(b"\0");
                    }
                } else {
                    h.update(part.as_bytes());
                }
                h.update(b"\x01");
            }
        }
        _ => {
            for c in &default {
                h.update(c.as_bytes());
                h.update(b"\0");
            }
        }
    }
    h.digest()
}

fn is_default_marker(s: &str) -> bool {
    let t: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    t == "{{default}}"
}

/// The ordered list of strings that identify an event's "default" group.
pub fn default_components(ev: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(values) = ev.get("exception").and_then(|e| e.get("values")).and_then(Value::as_array) {
        for exc in values {
            let ty = exc.get("type").and_then(Value::as_str).unwrap_or("");
            let frames = frame_components(exc.get("stacktrace"));
            out.push(format!("type:{ty}"));
            if frames.is_empty() {
                let val = exc.get("value").and_then(Value::as_str).unwrap_or("");
                out.push(format!("value:{}", mask_variables(val)));
            } else {
                out.extend(frames);
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    let thread_st = ev
        .get("threads")
        .and_then(|t| t.get("values"))
        .and_then(Value::as_array)
        .and_then(|threads| {
            threads
                .iter()
                .find(|t| t.get("crashed").and_then(Value::as_bool) == Some(true))
                .or_else(|| threads.iter().find(|t| t.get("stacktrace").is_some()))
        })
        .and_then(|t| t.get("stacktrace"));
    for st in [ev.get("stacktrace"), thread_st] {
        let frames = frame_components(st);
        if !frames.is_empty() {
            out.push("stacktrace".into());
            out.extend(frames);
            return out;
        }
    }
    if let Some(le) = ev.get("logentry") {
        let msg = le.get("message").or_else(|| le.get("formatted")).and_then(Value::as_str).unwrap_or("");
        out.push(format!("message:{}", mask_variables(msg)));
        return out;
    }
    // Nothing usable: group by level + logger so unlabeled events stay together.
    out.push(format!(
        "fallback:{}:{}",
        ev.get("level").and_then(Value::as_str).unwrap_or(""),
        ev.get("logger").and_then(Value::as_str).unwrap_or("")
    ));
    out
}

fn frames_of(st: Option<&Value>) -> Vec<&Value> {
    st.and_then(|s| s.get("frames")).and_then(Value::as_array).map(|f| f.iter().collect()).unwrap_or_default()
}

/// Frames that count for grouping: in-app ones when any frame is in-app.
pub fn grouping_frames(st: Option<&Value>) -> Vec<&Value> {
    let frames = frames_of(st);
    let in_app: Vec<&Value> =
        frames.iter().copied().filter(|f| f.get("in_app").and_then(Value::as_bool) == Some(true)).collect();
    if in_app.is_empty() { frames } else { in_app }
}

fn frame_components(st: Option<&Value>) -> Vec<String> {
    grouping_frames(st)
        .into_iter()
        .filter_map(|f| {
            let module = f
                .get("module")
                .and_then(Value::as_str)
                .map(normalize_module)
                .or_else(|| {
                    f.get("filename")
                        .or_else(|| f.get("abs_path"))
                        .and_then(Value::as_str)
                        .map(normalize_filename)
                })
                .unwrap_or_default();
            let function = f.get("function").and_then(Value::as_str).and_then(normalize_function);
            match (module.is_empty(), function) {
                (_, Some(func)) => Some(format!("frame:{module}:{func}")),
                (false, None) => {
                    // Anonymous: fall back to the source line when available.
                    let line = f.get("context_line").and_then(Value::as_str).map(str::trim).filter(|l| l.len() < 120);
                    Some(format!("frame:{module}:{}", line.unwrap_or("")))
                }
                (true, None) => None,
            }
        })
        .collect()
}

static RE_HASH_SEGMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[.\-_]([0-9a-fA-F]{6,}|[0-9a-zA-Z_-]{8})\.").unwrap());
static RE_HEXISH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9a-f]{8,}\b").unwrap());

fn normalize_filename(f: &str) -> String {
    let mut s = crate::event::filename_from_abs_path(f);
    if let Some(i) = s.find(['?', '#']) {
        s.truncate(i);
    }
    for prefix in ["webpack://", "app://", "webpack-internal://"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
        }
    }
    let s = s.trim_start_matches("./");
    let s = RE_HASH_SEGMENT.replace_all(s, ".<hash>.");
    RE_HEXISH.replace_all(&s, "<hash>").into_owned()
}

fn normalize_module(m: &str) -> String {
    RE_HEXISH.replace_all(m, "<hash>").into_owned()
}

fn normalize_function(f: &str) -> Option<String> {
    let f = f.trim();
    let f = f.strip_prefix("async ").unwrap_or(f);
    let f = f.strip_prefix("new ").unwrap_or(f);
    match f {
        "" | "?" | "<anonymous>" | "Anonymous function" | "Object.<anonymous>" | "<unknown>" => None,
        _ => Some(f.trim_end_matches("/<").replace("/</<", "").to_string()),
    }
}

static RE_MASKS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"[0-9a-fA-F]{8}-?[0-9a-fA-F]{4}-?[0-9a-fA-F]{4}-?[0-9a-fA-F]{4}-?[0-9a-fA-F]{12}", "<uuid>"),
        (r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?", "<date>"),
        (r"[\w.+-]+@[\w-]+\.[\w.-]+", "<email>"),
        (r"\b\d{1,3}(?:\.\d{1,3}){3}\b", "<ip>"),
        (r"\b0x[0-9a-fA-F]+\b", "<hex>"),
        (r"\b[0-9a-fA-F]{12,}\b", "<hex>"),
        (r"\b\d+(?:\.\d+)?\b", "<int>"),
    ]
    .into_iter()
    .map(|(r, rep)| (Regex::new(r).unwrap(), rep))
    .collect()
});

/// Mask variable parts so "user 17 not found" and "user 42 not found" group.
pub fn mask_variables(s: &str) -> String {
    let mut out = truncate(s, 512);
    for (re, rep) in RE_MASKS.iter() {
        out = re.replace_all(&out, *rep).into_owned();
    }
    out
}

pub fn metadata(ev: &Value) -> Metadata {
    let exc = last_exception(ev);
    let exception_type = exc.and_then(|e| e.get("type")).and_then(Value::as_str).map(str::to_string);
    let exception_value = exc.and_then(|e| e.get("value")).and_then(Value::as_str).map(|v| truncate(v, 1024));
    let message = ev
        .get("logentry")
        .and_then(|l| l.get("formatted").or_else(|| l.get("message")))
        .and_then(Value::as_str);
    let title = match (&exception_type, &exception_value) {
        (Some(t), Some(v)) if !t.is_empty() && !v.is_empty() => format!("{t}: {}", first_line(v)),
        (Some(t), _) if !t.is_empty() => t.clone(),
        (_, Some(v)) if !v.is_empty() => first_line(v).to_string(),
        _ => match message {
            Some(m) if !m.trim().is_empty() => first_line(m).to_string(),
            _ => "<unlabeled event>".to_string(),
        },
    };
    let culprit = ev
        .get("transaction")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let st = exc.and_then(|e| e.get("stacktrace")).or_else(|| ev.get("stacktrace"));
            let frame = grouping_frames(st).into_iter().last()?;
            let place = frame
                .get("module")
                .or_else(|| frame.get("filename"))
                .or_else(|| frame.get("abs_path"))
                .and_then(Value::as_str);
            let func = frame.get("function").and_then(Value::as_str).filter(|f| *f != "?");
            match (place, func) {
                (Some(p), Some(f)) => Some(format!("{f}({p})")),
                (Some(p), None) => Some(p.to_string()),
                (None, Some(f)) => Some(f.to_string()),
                (None, None) => None,
            }
        });
    Metadata { title: truncate(&title, 200), culprit: culprit.map(|c| truncate(&c, 200)), exception_type, exception_value }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn js_error(file: &str, line: u32, value: &str) -> Value {
        json!({
            "platform": "javascript",
            "exception": {"values": [{
                "type": "TypeError",
                "value": value,
                "stacktrace": {"frames": [
                    {"filename": "/vendor.js", "function": "dispatch", "in_app": false, "lineno": 1},
                    {"filename": file, "function": "handleClick", "in_app": true, "lineno": line, "colno": 4},
                ]}
            }]}
        })
    }

    #[test]
    fn same_code_different_lines_and_bundle_hash_group_together() {
        let a = js_error("https://x.test/assets/main.4f3a2b1c.js?v=1", 10, "a is undefined");
        let b = js_error("https://x.test/assets/main.9d8e7f6a.js", 99, "b is undefined");
        assert_eq!(group_hash(&a), group_hash(&b));
    }

    #[test]
    fn different_functions_split() {
        let a = js_error("/main.js", 1, "x");
        let mut b = a.clone();
        b["exception"]["values"][0]["stacktrace"]["frames"][1]["function"] = json!("other");
        assert_ne!(group_hash(&a), group_hash(&b));
    }

    #[test]
    fn exceptions_without_frames_mask_values() {
        let a = json!({"exception": {"values": [{"type": "KeyError", "value": "user 17 at 2026-01-01T10:00:00Z"}]}});
        let b = json!({"exception": {"values": [{"type": "KeyError", "value": "user 42 at 2026-02-03T11:00:00Z"}]}});
        let c = json!({"exception": {"values": [{"type": "KeyError", "value": "order 42 missing"}]}});
        assert_eq!(group_hash(&a), group_hash(&b));
        assert_ne!(group_hash(&a), group_hash(&c));
    }

    #[test]
    fn messages_group_by_template() {
        let a = json!({"logentry": {"message": "Payment %s failed", "formatted": "Payment 1 failed"}});
        let b = json!({"logentry": {"message": "Payment %s failed", "formatted": "Payment 2 failed"}});
        let c = json!({"logentry": {"formatted": "Payment 3 failed"}});
        let d = json!({"logentry": {"formatted": "Payment 4 failed"}});
        assert_eq!(group_hash(&a), group_hash(&b));
        assert_eq!(group_hash(&c), group_hash(&d));
    }

    #[test]
    fn custom_fingerprint_and_default_marker() {
        let base = js_error("/main.js", 1, "x");
        let mut a = base.clone();
        a["fingerprint"] = json!(["{{ default }}", "tenant-a"]);
        let mut b = base.clone();
        b["fingerprint"] = json!(["{{ default }}", "tenant-b"]);
        let mut only_default = base.clone();
        only_default["fingerprint"] = json!(["{{default}}"]);
        assert_ne!(group_hash(&a), group_hash(&b));
        assert_eq!(group_hash(&only_default), group_hash(&base));
        let mut c = json!({"message": "whatever"});
        c["fingerprint"] = json!(["db-timeout"]);
        let mut d = js_error("/other.js", 5, "y");
        d["fingerprint"] = json!(["db-timeout"]);
        assert_eq!(group_hash(&c), group_hash(&d));
    }

    #[test]
    fn metadata_title_and_culprit() {
        let m = metadata(&js_error("/src/app.ts", 3, "x is undefined\nmore"));
        assert_eq!(m.title, "TypeError: x is undefined");
        assert_eq!(m.culprit.as_deref(), Some("handleClick(/src/app.ts)"));
        let m = metadata(&json!({"logentry": {"formatted": "boom"}, "transaction": "GET /x"}));
        assert_eq!(m.title, "boom");
        assert_eq!(m.culprit.as_deref(), Some("GET /x"));
        assert_eq!(metadata(&json!({})).title, "<unlabeled event>");
    }
}
