//! `--app` end to end (ADR 0027, 0028): the real `mallard` binary against a mallard
//! server in this process. Each side of an app, two service names for one app, the rule
//! with `-p` and `-S`, exit codes and JSON errors.

#[path = "../../mallard-server/tests/common/mod.rs"]
mod common;

use std::process::{Command, Stdio};

use common::*;
use serde_json::{Value, json};

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

async fn mallard_env(url: &str, env: &[(&str, &str)], args: &[&str]) -> Out {
    let (bin, url) = (env!("CARGO_BIN_EXE_mallard"), url.to_string());
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let env: Vec<(String, String)> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    tokio::task::spawn_blocking(move || {
        let o = Command::new(bin)
            .args(&args)
            .env("MALLARD_URL", url)
            .env("MALLARD_TOKEN", TOKEN)
            .env_remove("MALLARD_PROJECT")
            .env_remove("MALLARD_FORMAT")
            .envs(env)
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

async fn mallard(url: &str, args: &[&str]) -> Out {
    mallard_env(url, &[], args).await
}

/// One span, one log and one metric point of a service.
async fn send_service(http: &reqwest::Client, otlp_url: &str, service: &str, i: usize) {
    let (trace, span) = ids(i);
    let ns = chrono::Utc::now().timestamp_nanos_opt().unwrap();
    let resource = json!({"attributes": [{"key": "service.name", "value": {"stringValue": service}}]});
    let spans = json!({"resourceSpans": [{"resource": resource, "scopeSpans": [{"spans": [{
        "traceId": trace, "spanId": span, "name": format!("op of {service}"), "kind": 2,
        "startTimeUnixNano": ns.to_string(), "endTimeUnixNano": (ns + 5_000_000).to_string()}]}]}]});
    let logs = json!({"resourceLogs": [{"resource": resource, "scopeLogs": [{"logRecords": [{
        "timeUnixNano": ns.to_string(), "severityNumber": 9,
        "body": {"stringValue": format!("hello from {service}")}}]}]}]});
    let metrics = json!({"resourceMetrics": [{"resource": resource, "scopeMetrics": [{"metrics": [{
        "name": format!("{}.count", service.to_lowercase().replace(' ', "-")),
        "gauge": {"dataPoints": [{"timeUnixNano": ns.to_string(), "asDouble": 1.0}]}}]}]}]});
    for (path, body) in [("traces", spans), ("logs", logs), ("metrics", metrics)] {
        let r = http.post(format!("{otlp_url}/v1/{path}")).json(&body).send().await.unwrap();
        assert!(r.status().is_success(), "otlp {path}: {}", r.status());
    }
}

fn services_of(items: &Value, key: &str) -> Vec<String> {
    let mut v: Vec<String> =
        items["items"].as_array().unwrap().iter().map(|i| i[key].as_str().unwrap().to_string()).collect();
    v.sort();
    v.dedup();
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn app_scopes_each_side() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start(dir.path()).await;
    let url = srv.url.as_str();
    let http = reqwest::Client::new();

    // checkout-api: a project and two service names. web: only a project. worker: only a
    // service. other: a service next to them, which must never show.
    let s = mallard(url, &["setup", "checkout-api", "--json"]).await.ok().json();
    let (pid, key) = (s["project"]["id"].as_u64().unwrap() as u32, s["project"]["public_key"].as_str().unwrap());
    mallard(url, &["setup", "web", "--json"]).await.ok();
    send_sentry_event(&http, url, pid, key, 0).await;
    for (i, service) in ["Checkout API", "checkout-api", "worker", "other"].into_iter().enumerate() {
        send_service(&http, &srv.otlp_url, service, i + 1).await;
    }
    eventually("data in the lake", || async {
        let logs = mallard(url, &["logs", "--json"]).await.ok().json();
        let issues = mallard(url, &["issues", "list", "-p", "checkout-api", "--json"]).await.ok().json();
        (logs["count"] == 4 && issues["count"] == 1).then_some(())
    })
    .await;

    // Telemetry: both service names of the app, whatever the command.
    let logs = mallard(url, &["logs", "--app", "checkout-api", "--json"]).await.ok().json();
    assert_eq!(services_of(&logs, "service_name"), ["Checkout API", "checkout-api"], "{logs}");
    let traces = mallard(url, &["traces", "list", "--app", "checkout-api", "--json"]).await.ok().json();
    assert_eq!(traces["count"], 2, "{traces}");
    let ops = mallard(url, &["ops", "--app", "checkout-api", "--json"]).await.ok().json();
    assert_eq!(services_of(&ops, "name"), ["op of Checkout API", "op of checkout-api"], "{ops}");
    let ops = mallard(url, &["ops", "--app", "checkout-api", "--compare", "1d", "--json"]).await.ok().json();
    assert_eq!(ops["count"], 2, "--compare too: {ops}");
    let metrics = mallard(url, &["metrics", "list", "--app", "checkout-api", "--json"]).await.ok().json();
    assert_eq!(metrics["count"], 1, "one name from both services: {metrics}");
    let attrs = mallard(url, &["schema", "attrs", "--app", "worker", "--key", "service.name", "--json"]).await;
    assert_eq!(attrs.code, 0, "{}", attrs.stderr);
    let series = mallard(url, &["series", "spans", "--app", "checkout-api", "--json"]).await.ok().json();
    let spans: u64 = series["items"].as_array().unwrap().iter().map(|p| p["count"].as_u64().unwrap()).sum();
    assert_eq!(spans, 2, "{series}");
    let series = mallard(url, &["series", "logs", "--app", "checkout-api", "--json"]).await.ok().json();
    let total: u64 = series["items"].as_array().unwrap().iter().map(|p| p["total"].as_u64().unwrap()).sum();
    assert_eq!(total, 2, "{series}");
    // The hidden alias, --fields, ndjson and the flag before the command.
    let v =
        mallard(url, &["--app", "worker", "telemetry", "logs", "--json", "--fields", "service_name"]).await.ok().json();
    assert_eq!(v["items"][0], json!({"service_name": "worker"}), "{v}");
    let nd = mallard(url, &["logs", "--app", "worker", "--format", "ndjson"]).await.ok();
    assert_eq!(nd.stdout.lines().count(), 1, "{}", nd.stdout);
    // `-f` keeps to the app too.
    let f = mallard(url, &["logs", "--app", "worker", "-f", "--for", "1s", "--format", "ndjson"]).await.ok();
    assert!(f.stdout.contains("hello from worker") && !f.stdout.contains("other"), "{}", f.stdout);

    // Errors: the app's project.
    let issues = mallard(url, &["issues", "list", "--app", "checkout-api", "--json"]).await.ok().json();
    assert_eq!(issues["count"], 1, "{issues}");
    let events = mallard(url, &["events", "list", "--app", "checkout-api", "--json"]).await.ok().json();
    assert_eq!(events["items"][0]["project_id"], pid, "{events}");
    let series = mallard(url, &["series", "errors", "--app", "checkout-api", "--since", "1d", "--json"]).await.ok();
    assert!(series.json()["items"].as_array().is_some_and(|v| !v.is_empty()));
    let issue = issues["items"][0]["id"].as_str().unwrap();
    let found = mallard(url, &["find", issue, "--app", "checkout-api", "--json"]).await.ok().json();
    assert_eq!(found["items"][0]["kind"], "issue", "{found}");
    let empty = mallard(url, &["issues", "list", "--app", "web", "--json"]).await.ok().json();
    assert_eq!(empty["count"], 0, "a project with no errors is a real empty list: {empty}");
    let rel = mallard(url, &["errors", "stats", "--app", "checkout-api", "--json"]).await;
    assert_eq!(rel.code, 0, "the old path: {}", rel.stderr);

    // A side the app lacks is an error, not an empty list that looks like no errors.
    let o = mallard(url, &["issues", "list", "--app", "worker", "--json"]).await;
    assert_eq!(o.code, 4, "{}", o.stderr);
    assert!(o.stdout.is_empty(), "no empty list: {}", o.stdout);
    let e = o.error();
    assert_eq!(e["code"], "not_found");
    assert!(e["message"].as_str().unwrap().contains("has no project"), "{e}");
    assert!(e["hint"].as_str().unwrap().contains("mallard setup worker"), "{e}");
    let o = mallard(url, &["logs", "--app", "web", "--json"]).await;
    assert_eq!(o.code, 4, "{}", o.stderr);
    assert!(o.error()["message"].as_str().unwrap().contains("has no telemetry"), "{}", o.stderr);
    let o = mallard(url, &["logs", "--app", "web"]).await;
    assert_eq!(o.code, 4);
    assert!(o.stderr.starts_with("error: app \"web\" has no telemetry"), "text mode: {}", o.stderr);
    assert!(o.stderr.contains("OTEL_SERVICE_NAME=web"), "{}", o.stderr);
    let o = mallard(url, &["issues", "list", "--app", "nope", "--format", "ndjson"]).await;
    assert_eq!((o.code, o.error()["code"].as_str()), (4, Some("not_found")), "{}", o.stderr);
    assert!(o.stderr.contains("no app"), "{}", o.stderr);

    // With -p or -S it is a usage error, before anything is asked.
    for args in [
        &["issues", "list", "--app", "web", "-p", "web", "--json"][..],
        &["--app", "web", "-p", "other", "issues", "list", "--json"],
        &["logs", "--app", "worker", "-S", "worker", "--json"],
        &["traces", "list", "--app", "worker", "-S", "x", "--json"],
        &["series", "spans", "--app", "worker", "-S", "x", "--json"],
    ] {
        let o = mallard(url, args).await;
        assert_eq!((o.code, o.error()["code"].as_str()), (2, Some("usage")), "{args:?}: {}", o.stderr);
        assert!(o.stdout.is_empty(), "{args:?}");
    }
    let o = mallard(url, &["logs", "--app", "worker", "-S", "worker"]).await;
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("--app or -S"), "{}", o.stderr);
    // Where it means nothing it is a usage error too.
    for args in [
        &["status", "--app", "web", "--json"][..],
        &["services", "--app", "web", "--json"],
        &["sql", "SELECT 1", "--app", "web", "--json"],
        &["events", "list", "--app", "web", "--trace", "0af7651916cd43dd8448eb211c80319c", "--json"],
    ] {
        let o = mallard(url, args).await;
        assert_eq!((o.code, o.error()["code"].as_str()), (2, Some("usage")), "{args:?}: {}", o.stderr);
    }

    // MALLARD_PROJECT is a default: the flag wins, and -p on its own still uses it.
    let env = [("MALLARD_PROJECT", "web")];
    let o = mallard_env(url, &env, &["issues", "list", "--app", "checkout-api", "--json"]).await.ok();
    assert_eq!(o.json()["count"], 1, "{}", o.stdout);
    let o = mallard_env(url, &env, &["issues", "list", "--json"]).await.ok();
    assert_eq!(o.json()["count"], 0, "web has none: {}", o.stdout);

    // The old flags are as they were.
    let logs = mallard(url, &["logs", "-S", "worker", "--json"]).await.ok().json();
    assert_eq!(services_of(&logs, "service_name"), ["worker"]);
    let issues = mallard(url, &["issues", "list", "-p", "checkout-api", "--json"]).await.ok().json();
    assert_eq!(issues["count"], 1);
    let all = mallard(url, &["logs", "--json"]).await.ok().json();
    assert_eq!(all["count"], 4, "no flag: everything");

    // The list needs the token and the server, like everything else.
    let o = mallard_env(url, &[("MALLARD_TOKEN", "wrong")], &["logs", "--app", "worker", "--json"]).await;
    assert_eq!((o.code, o.error()["code"].as_str()), (3, Some("unauthorized")), "{}", o.stderr);
    let o = mallard("http://127.0.0.1:9", &["logs", "--app", "worker", "--json"]).await;
    assert_eq!((o.code, o.error()["retryable"].as_bool()), (6, Some(true)), "{}", o.stderr);

    srv.stop().await;
}
