//! The client-side P1 items of the agent CLI (issue 0004) end to end: the
//! real `mallard` binary against a mallard server in this process.
//! `--wait` (exit 8 on timeout), `logs -f --for/--until-match`,
//! `ops --compare`, `series`, `setup --sentry-cli`, `--quiet` and
//! not-found messages that say it once.

#[path = "../../mallard-server/tests/common/mod.rs"]
mod common;

use std::process::{Command, Stdio};
use std::time::Duration;

use common::*;
use serde_json::Value;

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

    /// The JSON error line (the last line of stderr).
    fn error(&self) -> Value {
        let line = self.stderr.lines().last().unwrap_or_default();
        serde_json::from_str::<Value>(line).unwrap_or_else(|e| panic!("{e}: {}", self.stderr))["error"].clone()
    }

    fn ok(self) -> Self {
        assert_eq!(self.code, 0, "stdout {}\nstderr {}", self.stdout, self.stderr);
        self
    }

    fn count(&self) -> u64 {
        self.json()["count"].as_u64().unwrap_or_else(|| panic!("no count: {}", self.stdout))
    }
}

async fn mallard(url: &str, args: &[&str]) -> Out {
    let (bin, url) = (env!("CARGO_BIN_EXE_mallard"), url.to_string());
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

fn sum(v: &Value, key: &str) -> u64 {
    v["items"].as_array().unwrap().iter().map(|p| p[key].as_u64().unwrap()).sum()
}

/// Issue 0186: on a server with a short `max_window`, the default window of `mallard trace` falls back
/// to 24 hours and says so, and finds the trace. A window that was asked for stays refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn trace_falls_back_from_a_default_window_the_server_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start_with(dir.path(), |c| c.otel.query.max_window = Duration::from_secs(2 * 86_400)).await;
    let url = srv.url.as_str();
    send_otlp(&reqwest::Client::new(), &srv.otlp_url, 0).await;
    let (id, _) = ids(0);

    let o = eventually("the trace", || async {
        let o = mallard(url, &["trace", &id, "--json"]).await;
        (o.code == 0).then_some(o)
    })
    .await;
    assert!(!o.json()["spans"].as_array().unwrap().is_empty(), "{}", o.stdout);
    assert!(o.stderr.contains("searching the last 24h"), "{}", o.stderr);

    let o = mallard(url, &["trace", &id, "--since", "7d", "--json"]).await;
    assert!(o.stdout.contains("max_window") || o.stderr.contains("max_window"), "{} {}", o.stdout, o.stderr);
    assert!(!o.stderr.contains("searching the last 24h"), "{}", o.stderr);
    srv.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn agent_cli_p1_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let srv = start(dir.path()).await;
    let url = srv.url.as_str();

    // setup --sentry-cli: the server root (sentry-cli refuses a path), any
    // org, the project and the CLI's token.
    let s = mallard(url, &["setup", "checkout", "--sentry-cli", "--json"]).await.ok().json();
    let env = &s["env"];
    assert_eq!((env["SENTRY_URL"].as_str(), env["SENTRY_ORG"].as_str()), (Some(url), Some("default")), "{s}");
    assert_eq!((env["SENTRY_PROJECT"].as_str(), env["SENTRY_AUTH_TOKEN"].as_str()), (Some("checkout"), Some(TOKEN)));
    let plain = mallard(url, &["setup", "checkout"]).await.ok();
    assert!(!plain.stdout.contains("SENTRY_URL"), "only with --sentry-cli: {}", plain.stdout);
    let (pid, key) = (s["project"]["id"].as_u64().unwrap() as u32, s["project"]["public_key"].as_str().unwrap());

    // --wait with nothing to find: the empty result on stdout, exit 8.
    let (trace, _) = ids(0);
    let o = mallard(url, &["events", "list", "--trace", &trace, "--wait", "2s", "--json"]).await;
    assert_eq!((o.code, o.count()), (8, 0), "{}", o.stderr);
    assert_eq!(o.error()["code"], "condition_not_met");
    let o = mallard(url, &["find", &trace, "--wait", "1s"]).await;
    assert_eq!(o.code, 8, "{}", o.stderr);
    assert!(o.stderr.contains("note: nothing yet") && o.stderr.contains("nothing arrived within 1s"), "{}", o.stderr);
    let o = mallard(url, &["find", &trace, "--wait", "1s", "--quiet"]).await;
    assert_eq!(o.code, 8);
    assert!(!o.stderr.contains("note:") && o.stderr.contains("error: nothing arrived"), "{}", o.stderr);

    // --wait while the data is on its way: every command returns it, exit 0.
    let events = ["events", "list", "--trace", &trace, "--wait", "60s", "--json"];
    let find = ["find", &trace, "--wait", "60s", "--json"];
    let tr = ["trace", &trace, "--wait", "60s", "--json"];
    let logs = ["logs", "-t", &trace, "--wait", "60s", "--json"];
    let traces = ["traces", "list", "--wait", "60s", "--json"];
    let waits = async {
        tokio::join!(
            mallard(url, &events),
            mallard(url, &find),
            mallard(url, &tr),
            mallard(url, &logs),
            mallard(url, &traces)
        )
    };
    let send = async {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let http = reqwest::Client::new();
        send_sentry_event(&http, url, pid, key, 0).await;
        send_otlp(&http, &srv.otlp_url, 0).await;
    };
    let ((events, find, tr, logs, traces), ()) = tokio::join!(waits, send);
    assert_eq!(events.ok().count(), 1);
    assert!(find.ok().count() >= 1);
    let tr = tr.ok().json();
    assert!(!tr["spans"].as_array().unwrap().is_empty() || !tr["errors"].as_array().unwrap().is_empty(), "{tr}");
    assert_eq!(logs.ok().count(), 1);
    assert_eq!(traces.ok().count(), 1);

    // logs -f: stops at the first match (exit 0), when --for is over (exit
    // 0), or with --until-match and no match (exit 8).
    let o =
        mallard(url, &["logs", "-f", "--since", "1h", "--until-match", "PAYMENT 0", "--for", "30s", "--json"]).await;
    let o = o.ok();
    assert_eq!(o.stdout.lines().count(), 1, "{}", o.stdout);
    assert_eq!(serde_json::from_str::<Value>(o.stdout.trim()).unwrap()["body"], "payment 0 failed");
    let o = mallard(url, &["logs", "-f", "--since", "1h", "--for", "1s"]).await.ok();
    assert!(o.stdout.contains("payment 0 failed"), "{}", o.stdout);
    let o = mallard(url, &["logs", "-f", "--until-match", "never logged", "--for", "1s", "--json"]).await;
    assert_eq!((o.code, o.error()["code"].as_str()), (8, Some("condition_not_met")), "{}", o.stderr);
    assert_eq!(mallard(url, &["logs", "--for", "1s"]).await.code, 2, "--for needs -f");

    // ops --compare: the same window a day earlier had nothing.
    let v = mallard(url, &["ops", "--since", "1h", "--compare", "1d", "--json"]).await.ok().json();
    let op = &v["items"][0];
    assert_eq!(op["name"], "POST /pay", "{v}");
    assert!(op["p95_ms"].is_number() && op["p95_ms_before"].is_null() && op["p95_change_pct"].is_null(), "{op}");
    let t = mallard(url, &["ops", "--since", "1h", "--compare", "1d"]).await.ok();
    assert!(t.stdout.contains("p95 1d ago") && t.stdout.contains("POST /pay"), "{}", t.stdout);
    assert_eq!(mallard(url, &["ops", "--compare", "0s"]).await.code, 2, "a zero shift is a usage error");

    // series: one item per bucket, in the envelope.
    let v = mallard(url, &["series", "spans", "--since", "1h", "--step", "5m", "--json"]).await.ok().json();
    assert_eq!((sum(&v, "count"), sum(&v, "errors")), (1, 1), "{v}");
    assert!(v["window"]["from"].is_string() && v["items"][0]["p95_ms"].is_number(), "{v}");
    let v = mallard(url, &["series", "logs", "-S", "checkout", "--json"]).await.ok().json();
    assert_eq!((sum(&v, "total"), sum(&v, "error")), (1, 1), "{v}");
    let v = eventually("errors series", || async {
        let v = mallard(url, &["-p", "checkout", "series", "errors", "--since", "1h", "--json"]).await.ok().json();
        (sum(&v, "events") == 1).then_some(v)
    })
    .await;
    assert_eq!(sum(&v, "errors"), 1, "{v}");
    let t = mallard(url, &["series", "spans"]).await.ok();
    assert!(t.stdout.lines().next().is_some_and(|h| h.contains("p95")), "{}", t.stdout);

    // --quiet drops the truncation note, not the data. The server says
    // whether more exist, so a second trace makes the page truncated.
    send_otlp(&reqwest::Client::new(), &srv.otlp_url, 1).await;
    let (second, _) = ids(1);
    eventually("second trace", || async {
        let l = mallard(url, &["traces", "list", "--json"]).await.ok().json();
        (l["count"] == 2).then_some(())
    })
    .await;
    let o = mallard(url, &["traces", "list", "-n", "1"]).await.ok();
    assert!(o.stderr.contains("note: 1 shown") && o.stderr.contains("--cursor "), "{}", o.stderr);
    let o = mallard(url, &["traces", "list", "-n", "1", "--quiet"]).await.ok();
    assert!(o.stderr.is_empty() && o.stdout.contains(&second), "stderr {:?}", o.stderr);

    // Not found says it once, on both sides.
    let o = mallard(url, &["-p", "checkout", "issues", "show", "0000000000000000"]).await;
    assert_eq!((o.code, o.stderr.trim()), (4, "error: issue not found"));
    let o = mallard(url, &["metrics", "show", "no.such.metric", "--json"]).await;
    let msg = o.error()["message"].as_str().unwrap().to_string();
    assert!(o.code == 4 && !msg.starts_with("not found:"), "{msg}");

    srv.stop().await;
}
