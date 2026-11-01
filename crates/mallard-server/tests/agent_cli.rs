//! The agent contract end to end (ADR 0008): the real `mallard` binary
//! against a mallard server in this process. Covers `setup`, `find` (also
//! span ids), `issues list --new`, `issues resolve --reason`, `logs --around`,
//! `schema`, `schema attrs`, `--cursor`,
//! `--fields`, ndjson, plain text when piped, exit codes and JSON errors.
//!
//! The binary is `target/<profile>/mallard`: `cargo test --workspace`
//! builds it (crates/mallard has integration tests); run alone, this test
//! builds it first.

mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use common::*;
use serde_json::Value;

fn mallard_bin() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().and_then(|d| d.parent()).expect("target/<profile>/deps/<test>");
        let bin = dir.join(format!("mallard{}", std::env::consts::EXE_SUFFIX));
        if !bin.exists() {
            let ok = Command::new(env!("CARGO")).args(["build", "-p", "mallard", "--bin", "mallard"]).status();
            assert!(ok.is_ok_and(|s| s.success()), "cargo build -p mallard failed");
        }
        bin
    })
    .clone()
}

/// One run of the binary.
struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("{e}: stdout {:?} stderr {:?}", self.stdout, self.stderr))
    }

    /// The one JSON error line on stderr.
    fn error(&self) -> Value {
        assert_eq!(self.stderr.lines().count(), 1, "one error line: {}", self.stderr);
        serde_json::from_str::<Value>(&self.stderr).unwrap_or_else(|e| panic!("{e}: {}", self.stderr))["error"].clone()
    }

    fn ok(self) -> Self {
        assert_eq!(self.code, 0, "stdout {}\nstderr {}", self.stdout, self.stderr);
        self
    }
}

async fn mallard(url: &str, args: &[&str]) -> Out {
    let (bin, url) = (mallard_bin(), url.to_string());
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        let o = Command::new(bin)
            .args(&args)
            .env("MALLARD_URL", url)
            .env("MALLARD_TOKEN", TOKEN)
            .env_remove("MALLARD_PROJECT")
            .env_remove("MALLARD_FORMAT")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_contract_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start(dir.path()).await;
    let url = srv.url.clone();
    let url = url.as_str();

    // status: both sides up; capabilities from both.
    let st = mallard(url, &["status", "--json"]).await.ok().json();
    assert_eq!(st["ok"], true, "{st}");
    assert_eq!(st["capabilities"]["sql"], false);
    assert_eq!(st["capabilities"]["ingest_lag_secs"], 0.2);
    assert!(st["capabilities"]["retention_secs"]["errors"].is_u64(), "{st}");

    // setup: create, then reuse; DSN and OTLP env.
    let s1 = mallard(url, &["setup", "checkout", "--platform", "node", "--json"]).await.ok().json();
    assert_eq!(s1["created"], true, "{s1}");
    assert!(s1["env"]["SENTRY_DSN"].as_str().unwrap().starts_with("http://"), "{s1}");
    assert_eq!(s1["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"], url);
    assert_eq!(s1["env"]["OTEL_SERVICE_NAME"], "checkout");
    let s2 = mallard(url, &["setup", "checkout", "--json"]).await.ok().json();
    assert_eq!((s2["created"].as_bool(), &s2["project"]["id"]), (Some(false), &s1["project"]["id"]));
    let text = mallard(url, &["setup", "checkout"]).await.ok();
    assert!(text.stdout.lines().any(|l| l.starts_with("SENTRY_DSN=http://")), "{}", text.stdout);
    assert!(text.stdout.lines().all(|l| l.contains('=') && !l.starts_with(' ')), "env lines only: {}", text.stdout);
    assert!(text.stderr.contains("project exists"), "{}", text.stderr);

    // One request: a Sentry error, a span and a log in the same trace.
    let (trace, _) = ids(0);
    let http = reqwest::Client::new();
    let (pid, key) = (s1["project"]["id"].as_u64().unwrap() as u32, s1["project"]["public_key"].as_str().unwrap());
    send_sentry_event(&http, url, pid, key, 0).await;
    send_otlp(&http, &srv.otlp_url, 0).await;

    // find: the trace (with its span, error and log) and the event.
    let deadline = Instant::now() + Duration::from_secs(60);
    let found = loop {
        let o = mallard(url, &["find", &trace, "--json"]).await;
        if o.code == 0 {
            let v = o.json();
            let items = v["items"].as_array().unwrap().clone();
            let t = items.iter().find(|m| m["kind"] == "trace");
            if t.is_some_and(|t| t["spans"] == 1 && t["errors"] == 1 && t["logs"] == 1) {
                break items;
            }
        } else {
            assert_eq!(o.code, 4, "not found until ingested: {}", o.stderr);
        }
        assert!(Instant::now() < deadline, "trace never complete");
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    assert_eq!(found.iter().find(|m| m["kind"] == "trace").unwrap()["next"], format!("mallard trace {trace}"));
    let o = mallard(url, &["find", &"f".repeat(32), "--json"]).await;
    assert_eq!((o.code, o.error()["code"].as_str()), (4, Some("not_found")), "{}", o.stderr);
    assert!(o.json()["items"].as_array().unwrap().is_empty(), "the empty list is still printed");

    // traces list: the Sentry error count per trace; trace: per span.
    let l = mallard(url, &["traces", "list", "--since", "1h", "--json"]).await.ok().json();
    let item = l["items"].as_array().unwrap().iter().find(|t| t["trace_id"] == trace.as_str()).expect("listed");
    assert_eq!((item["sentry_errors"].as_u64(), item["error_count"].as_u64()), (Some(1), Some(1)), "{item}");
    let nd = mallard(url, &["traces", "list", "--fields", "trace_id,sentry_errors", "--format", "ndjson"]).await.ok();
    assert_eq!(nd.stdout.trim(), format!(r#"{{"trace_id":"{trace}","sentry_errors":1}}"#));
    let text = mallard(url, &["traces", "list"]).await.ok();
    assert!(
        text.stdout.lines().next().is_some_and(|h| h.contains("errors") && h.contains("sentry")),
        "{}",
        text.stdout
    );
    let t = mallard(url, &["trace", &trace, "--json", "--fields", "spans"]).await.ok().json();
    assert_eq!(t["spans"][0]["sentry_errors"], 1, "{t}");
    let text = mallard(url, &["trace", &trace]).await.ok();
    assert!(text.stdout.contains("POST /pay") && text.stdout.contains("⚠ 1"), "{}", text.stdout);

    // issues list --new, the envelope, --fields, ndjson, plain text.
    let l = mallard(url, &["-p", "checkout", "issues", "list", "--new", "--since", "1h", "--json"]).await.ok().json();
    assert_eq!((l["count"].as_u64(), l["truncated"].as_bool()), (Some(1), Some(false)), "{l}");
    assert!(l["window"]["from"].is_string() && l["window"]["to"].is_string(), "{l}");
    let issue = l["items"][0]["id"].as_str().unwrap().to_string();
    let old =
        mallard(url, &["-p", "checkout", "issues", "list", "--new", "--from", "2h", "--to", "1h", "--json"]).await;
    assert_eq!(old.ok().json()["count"], 0);
    let f = mallard(url, &["-p", "checkout", "issues", "list", "--fields", "id,title", "--json"]).await.ok();
    assert_eq!(f.stdout.trim().matches("\"id\"").count(), 1);
    let keys: Vec<String> = f.json()["items"][0].as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["id", "title"]);
    let t = mallard(url, &["-p", "checkout", "issues", "list", "--fields", "id,title"]).await.ok();
    assert!(t.stdout.starts_with(&format!("{issue}  ")), "text fields in the asked order: {}", t.stdout);
    let bad = mallard(url, &["-p", "checkout", "issues", "list", "--fields", "nope", "--json"]).await;
    assert_eq!(bad.code, 2);
    let msg = bad.error()["message"].as_str().unwrap().to_string();
    assert!(
        msg.contains("fields: ") && msg.contains(",id,") && msg.contains(",title,"),
        "lists the valid fields: {msg}"
    );
    let nd = mallard(url, &["-p", "checkout", "issues", "list", "--format", "ndjson"]).await.ok();
    assert_eq!(nd.stdout.lines().count(), 1);
    assert_eq!(serde_json::from_str::<Value>(nd.stdout.trim()).unwrap()["id"], issue.as_str());
    let plain = mallard(url, &["-p", "checkout", "issues", "list"]).await.ok();
    assert!(plain.stdout.contains(&issue), "{}", plain.stdout);
    assert!(
        !plain.stdout.chars().any(|c| ('\u{2500}'..='\u{259f}').contains(&c)),
        "no boxes or bars when piped: {}",
        plain.stdout
    );

    // issues show: concise latest event with the trace id.
    let d = mallard(url, &["-p", "checkout", "issues", "show", &issue, "--json"]).await.ok().json();
    assert_eq!(d["latest_event"]["exception"][0]["type"], "TypeError", "{d}");
    assert_eq!(d["latest_event"]["trace_id"], trace.as_str());
    assert!(d["latest_event"].get("payload").is_none(), "payload only with --raw");

    // resolve with a reason, twice: the second is not a change.
    let r = mallard(url, &["-p", "checkout", "issues", "resolve", &issue, "--reason", "fixed in #123", "--json"]).await;
    let r = r.ok().json();
    assert_eq!(
        (r["results"][0]["status"].as_str(), r["results"][0]["changed"].as_bool()),
        (Some("resolved"), Some(true)),
        "{r}"
    );
    let r =
        mallard(url, &["-p", "checkout", "issues", "resolve", &issue, "--reason", "again", "--json"]).await.ok().json();
    assert_eq!(r["results"][0]["changed"], false, "{r}");
    let d = mallard(url, &["-p", "checkout", "issues", "show", &issue, "--json", "--fields", "status,activity"]).await;
    let d = d.ok().json();
    assert_eq!((d["status"].as_str(), d["activity"][0]["reason"].as_str()), (Some("resolved"), Some("fixed in #123")));

    // logs --around the time of the log record.
    let logs = mallard(url, &["logs", "-t", &trace, "--since", "1h", "--json"]).await.ok().json();
    let time = logs["items"][0]["time"].as_str().unwrap().to_string();
    let around = mallard(url, &["logs", "--around", &time, "--window", "30s", "--json"]).await.ok().json();
    let item = &around["items"][0];
    assert_eq!(item["body"], "payment 0 failed", "{around}");
    assert!(item.get("resource_attributes").is_none(), "only with --raw");
    let raw = mallard(url, &["logs", "--around", &time, "--window", "30s", "--json", "--raw"]).await.ok().json();
    assert!(raw["items"][0]["resource_attributes"].is_object());
    let t: chrono::DateTime<chrono::Utc> = time.parse().unwrap();
    let later = (t + chrono::Duration::hours(1)).to_rfc3339();
    assert_eq!(mallard(url, &["logs", "--around", &later, "--json"]).await.ok().json()["count"], 0);

    // Errors: exit codes and one JSON line on stderr.
    let o = mallard(url, &["--token", "wrong", "-p", "checkout", "issues", "list", "--json"]).await;
    assert_eq!((o.code, o.stdout.as_str()), (3, ""));
    assert_eq!(o.error()["code"], "unauthorized");
    let o = mallard(url, &["-p", "checkout", "issues", "show", "0000000000000000", "--json"]).await;
    assert_eq!((o.code, o.error()["code"].as_str(), o.error()["status"].as_u64()), (4, Some("not_found"), Some(404)));
    let o = mallard(url, &["sql", "SELECT 1", "--json"]).await;
    assert_eq!((o.code, o.error()["code"].as_str()), (7, Some("disabled")), "{}", o.stderr);
    let o = mallard(url, &["--token", "wrong", "status", "--json"]).await;
    assert_eq!(o.code, 3, "{}", o.stderr);

    // find: a span id resolves to its trace.
    let (_, span) = ids(0);
    let v = mallard(url, &["find", &span, "--json"]).await.ok().json();
    let m = v["items"].as_array().unwrap().iter().find(|m| m["kind"] == "span").cloned().expect("span match");
    assert_eq!((m["trace_id"].as_str(), m["service"].as_str()), (Some(trace.as_str()), Some("checkout")), "{v}");
    assert_eq!(m["next"], format!("mallard trace {trace}"));

    // schema and schema attrs.
    let s = mallard(url, &["schema", "--json"]).await.ok().json();
    assert_eq!(
        (s["items"][0]["name"].as_str(), s["items"][0]["time_column"].as_str()),
        (Some("spans"), Some("start_time"))
    );
    let a = mallard(url, &["schema", "attrs", "--key", "service.name", "--since", "1h", "--json"]).await.ok().json();
    assert_eq!((a["items"][0]["value"].as_str(), a["items"][0]["count"].as_u64()), (Some("checkout"), Some(1)), "{a}");
    let text = mallard(url, &["schema", "attrs", "--signal", "logs"]).await.ok();
    assert!(text.stdout.lines().any(|l| l.trim_start().starts_with("service.name")), "{}", text.stdout);

    // --cursor: a second request, then two pages of one trace and one log each.
    send_otlp(&http, &srv.otlp_url, 1).await;
    eventually("second trace", || async {
        let l = mallard(url, &["traces", "list", "--json"]).await.ok().json();
        (l["count"] == 2).then_some(())
    })
    .await;
    for cmd in [&["traces", "list"][..], &["logs"]] {
        let first = mallard(url, &[cmd, &["-n", "1", "--since", "1h", "--json"]].concat()).await.ok().json();
        let cursor = first["next_cursor"].as_str().unwrap_or_else(|| panic!("{cmd:?}: {first}")).to_string();
        assert_eq!(first["truncated"], true);
        let (from, to) = (first["window"]["from"].as_str().unwrap(), first["window"]["to"].as_str().unwrap());
        let args = [cmd, &["-n", "1", "--from", from, "--to", to, "--cursor", cursor.as_str(), "--json"]].concat();
        let second = mallard(url, &args).await.ok().json();
        assert_eq!((second["count"].as_u64(), second["next_cursor"].is_null()), (Some(1), true), "{cmd:?}: {second}");
        assert_ne!(first["items"][0], second["items"][0], "{cmd:?}");
        let nd = mallard(url, &[cmd, &["-n", "1", "--since", "1h", "--format", "ndjson"]].concat()).await.ok();
        assert!(nd.stderr.contains("--cursor "), "{cmd:?}: the note has the cursor: {}", nd.stderr);
    }
    let o = mallard(url, &["logs", "--cursor", "nope", "--json"]).await;
    assert_eq!((o.code, o.error()["code"].as_str()), (2, Some("usage")), "{}", o.stderr);

    srv.stop().await;
    // The server is gone: status says so with exit code 6.
    let o = mallard(url, &["status", "--json"]).await;
    assert_eq!((o.code, o.json()["ok"].as_bool()), (6, Some(false)));
    assert_eq!(o.error()["retryable"], true);
}

/// Agent CLI P1, Sentry side: `--tag`, `--cursor`, `releases list` and
/// `sql --errors` (on, and off with exit 7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tags_cursors_releases_and_error_sql() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start_with(dir.path(), |c| c.sentry.query.enable_sql = true).await;
    let url = srv.url.clone();
    let url = url.as_str();
    let s = mallard(url, &["setup", "web", "--json"]).await.ok().json();
    let (pid, key) = (s["project"]["id"].as_u64().unwrap(), s["project"]["public_key"].as_str().unwrap());
    let http = reqwest::Client::new();
    for (msg, release, user) in [("a", "web@1", "u-1"), ("b", "web@2", "u-2"), ("c", "web@2", "u-1")] {
        let event = serde_json::json!({"message": msg, "level": "error", "release": release, "tags": {"user": user}});
        let r = http
            .post(format!("{url}/sentry/api/{pid}/store/?sentry_key={key}&sentry_version=7"))
            .body(event.to_string())
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{}", r.status());
    }

    // --tag on issues and events.
    let l = mallard(url, &["issues", "list", "--tag", "user=u-1", "--json", "--fields", "title"]).await.ok().json();
    let mut titles: Vec<&str> = l["items"].as_array().unwrap().iter().map(|i| i["title"].as_str().unwrap()).collect();
    titles.sort();
    assert_eq!(titles, ["a", "c"], "{l}");
    let e = mallard(url, &["events", "list", "--tag", "user=u-2", "--release", "web@2", "--json"]).await.ok().json();
    assert_eq!(e["count"], 1, "{e}");

    // --cursor: two pages of issues, nothing twice.
    let p1 = mallard(url, &["issues", "list", "-n", "2", "--json"]).await.ok().json();
    assert_eq!((p1["count"].as_u64(), p1["truncated"].as_bool()), (Some(2), Some(true)), "{p1}");
    let cursor = p1["next_cursor"].as_str().expect("a cursor").to_string();
    let text = mallard(url, &["issues", "list", "-n", "2"]).await.ok();
    // A new cursor (the window ends now), same shape.
    assert!(text.stderr.contains("next page: --cursor last_seen:"), "{}", text.stderr);
    assert!(!text.stdout.contains("--cursor"), "the note is not data: {}", text.stdout);
    let nd = mallard(url, &["events", "list", "-n", "1", "--format", "ndjson"]).await.ok();
    assert_eq!(nd.stdout.lines().count(), 1);
    assert!(nd.stderr.contains("next page: --cursor "), "{}", nd.stderr);
    let p2 = mallard(url, &["issues", "list", "-n", "2", "--cursor", &cursor, "--json"]).await.ok().json();
    assert_eq!(p2["count"], 1, "{p2}");
    assert_ne!(p2["items"][0]["id"], p1["items"][0]["id"]);
    assert_ne!(p2["items"][0]["id"], p1["items"][1]["id"]);
    let o = mallard(url, &["issues", "list", "--cursor", &cursor, "--sort", "count", "--json"]).await;
    assert_eq!(o.code, 2, "{}", o.stderr);

    // releases list.
    let r = mallard(url, &["releases", "list", "--json", "--fields", "version,events,new_issues"]).await.ok().json();
    assert_eq!(r["items"][0], serde_json::json!({"version": "web@2", "events": 2, "new_issues": 2}), "{r}");
    assert!(r["window"]["from"].is_string());
    let text = mallard(url, &["releases", "list"]).await.ok();
    assert!(text.stdout.contains("web@1") && text.stdout.contains("web@2"), "{}", text.stdout);

    // sql --errors on this server; the telemetry side is still off.
    let q = "SELECT tags['user'] AS u, count(*) AS n FROM events GROUP BY 1 ORDER BY 1";
    let v = mallard(url, &["sql", "--errors", q, "--json"]).await.ok().json();
    assert_eq!(v["rows"], serde_json::json!([["u-1", 2], ["u-2", 1]]), "{v}");
    let text = mallard(url, &["sql", "--errors", q]).await.ok();
    assert!(text.stdout.contains("u-1") && text.stdout.contains("2 rows"), "{}", text.stdout);
    let o = mallard(url, &["sql", "--errors", "SELECT * FROM spans", "--json"]).await;
    assert_eq!((o.code, o.error()["code"].as_str()), (2, Some("usage")), "{}", o.stderr);
    let o = mallard(url, &["sql", "SELECT 1", "--json"]).await;
    assert_eq!((o.code, o.error()["code"].as_str()), (7, Some("disabled")), "{}", o.stderr);
    srv.stop().await;

    // And with the Sentry side's SQL off: exit 7 naming the setting.
    let dir = tempfile::tempdir().unwrap();
    let srv = start(dir.path()).await;
    let o = mallard(&srv.url, &["sql", "--errors", "SELECT 1", "--json"]).await;
    assert_eq!((o.code, o.error()["code"].as_str()), (7, Some("disabled")), "{}", o.stderr);
    assert!(o.error()["message"].as_str().unwrap().contains("sentry.query.enable_sql"), "{}", o.stderr);
    srv.stop().await;
}
