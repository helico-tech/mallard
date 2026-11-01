//! `sdl send-test`: send one error event to a DSN with the Sentry envelope
//! protocol, so a DSN can be checked without installing an SDK.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::{Value, json};

/// The parts of a DSN `{scheme}://{key}[:secret]@{host}[:port]/{path/}{project}`.
#[derive(Debug, PartialEq)]
pub struct Dsn {
    pub public_key: String,
    pub project_id: String,
    /// `{scheme}://{host}[:port]{path}/api/{project}/envelope/`
    pub envelope_url: String,
}

pub fn parse_dsn(dsn: &str) -> Result<Dsn> {
    let u = url::Url::parse(dsn.trim()).context("invalid DSN (expected http(s)://KEY@HOST/PROJECT_ID)")?;
    if u.scheme() != "http" && u.scheme() != "https" {
        bail!("invalid DSN: scheme must be http or https");
    }
    let public_key = u.username().to_string();
    if public_key.is_empty() {
        bail!("invalid DSN: missing public key (http(s)://KEY@HOST/PROJECT_ID)");
    }
    let host = u.host_str().context("invalid DSN: missing host")?;
    let path = u.path().trim_end_matches('/');
    let (prefix, project_id) = path.rsplit_once('/').unwrap_or(("", path));
    if project_id.is_empty() {
        bail!("invalid DSN: missing project id");
    }
    let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
    let envelope_url = format!("{}://{host}{port}{prefix}/api/{project_id}/envelope/", u.scheme());
    Ok(Dsn { public_key, project_id: project_id.to_string(), envelope_url })
}

/// A small but realistic error event: exception with an in-app frame and
/// source context, a breadcrumb and tags.
pub fn test_event(event_id: &str, message: &str) -> Value {
    let now = Utc::now();
    json!({
        "event_id": event_id,
        "timestamp": now.timestamp_millis() as f64 / 1000.0,
        "platform": "other",
        "level": "error",
        "logger": "sdl.send-test",
        "environment": "test",
        "release": concat!("sdl@", env!("CARGO_PKG_VERSION")),
        "sdk": {"name": "sdl.send-test", "version": env!("CARGO_PKG_VERSION")},
        "tags": {"source": "sdl-send-test"},
        "exception": {"values": [{
            "type": "SdlTestError",
            "value": message,
            "mechanism": {"type": "generic", "handled": true},
            "stacktrace": {"frames": [
                {"function": "main", "module": "sdl", "filename": "sdl/main.rs", "lineno": 1, "in_app": false},
                {"function": "send_test", "module": "sdl::send_test", "filename": "sdl/send_test.rs", "lineno": 42,
                 "in_app": true,
                 "pre_context": ["fn send_test(dsn: &Dsn) {", "    // verify the DSN end to end"],
                 "context_line": format!("    raise(\"{message}\");"),
                 "post_context": ["}"]}
            ]}
        }]},
        "breadcrumbs": {"values": [
            {"timestamp": now.timestamp_millis() as f64 / 1000.0, "category": "sdl", "level": "info",
             "message": "sending test event"}
        ]}
    })
}

/// Envelope with one event item.
pub fn envelope(dsn_str: &str, event: &Value) -> Vec<u8> {
    let payload = serde_json::to_vec(event).expect("json");
    let header = json!({"event_id": event["event_id"], "dsn": dsn_str, "sent_at": Utc::now().to_rfc3339()});
    let item = json!({"type": "event", "length": payload.len(), "content_type": "application/json"});
    let mut out = Vec::with_capacity(payload.len() + 256);
    out.extend_from_slice(header.to_string().as_bytes());
    out.push(b'\n');
    out.extend_from_slice(item.to_string().as_bytes());
    out.push(b'\n');
    out.extend_from_slice(&payload);
    out.push(b'\n');
    out
}

pub fn auth_header(key: &str) -> String {
    format!("Sentry sentry_version=7, sentry_key={key}, sentry_client=sdl-cli/{}", env!("CARGO_PKG_VERSION"))
}

/// Send and return the event id the server accepted.
pub async fn send(dsn_str: &str, message: &str) -> Result<(Dsn, String)> {
    let dsn = parse_dsn(dsn_str)?;
    let event_id = uuid::Uuid::new_v4().simple().to_string();
    let body = envelope(dsn_str, &test_event(&event_id, message));
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
    let resp = http
        .post(&dsn.envelope_url)
        .header("X-Sentry-Auth", auth_header(&dsn.public_key))
        .header("Content-Type", "application/x-sentry-envelope")
        .body(body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("cannot reach {}: {}", dsn.envelope_url, root_cause(&e)))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("detail").or_else(|| v.get("error")).and_then(Value::as_str).map(str::to_string))
            .unwrap_or(text);
        bail!("server rejected the event ({}): {detail}", status.as_u16());
    }
    let id = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
        .unwrap_or(event_id);
    Ok((dsn, id))
}

fn root_cause(e: &(dyn std::error::Error + 'static)) -> String {
    let mut cur = e;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dsns() {
        let d = parse_dsn("http://abc@127.0.0.1:8791/1").unwrap();
        assert_eq!(
            d,
            Dsn {
                public_key: "abc".into(),
                project_id: "1".into(),
                envelope_url: "http://127.0.0.1:8791/api/1/envelope/".into()
            }
        );
        let d = parse_dsn("https://key:secret@errors.example.com/sentry/42").unwrap();
        assert_eq!(d.envelope_url, "https://errors.example.com/sentry/api/42/envelope/");
        assert_eq!(d.public_key, "key");
        assert!(parse_dsn("http://127.0.0.1/1").is_err());
        assert!(parse_dsn("http://k@127.0.0.1/").is_err());
        assert!(parse_dsn("not a dsn").is_err());
    }

    #[test]
    fn envelope_has_three_lines_with_correct_length() {
        let ev = test_event("0123456789abcdef0123456789abcdef", "hi");
        let env = envelope("http://k@h/1", &ev);
        let text = String::from_utf8(env).unwrap();
        let lines: Vec<&str> = text.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 3);
        let item: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(item["length"].as_u64().unwrap() as usize, lines[2].len());
        let payload: Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(payload["exception"]["values"][0]["value"], "hi");
        assert!(auth_header("k").contains("sentry_key=k"));
    }
}
