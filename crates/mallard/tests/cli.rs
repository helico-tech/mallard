//! Runs the real `mallard` binary without a server: help, usage errors and
//! the exit code of `status` when the server is down (ADR 0008). The tests
//! against a running server are in `crates/mallard-server/tests/agent_cli.rs`.

use std::process::{Command, Output, Stdio};

fn mallard(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mallard"))
        .args(args)
        .env("MALLARD_URL", "http://127.0.0.1:9")
        .env_remove("MALLARD_TOKEN")
        .env_remove("MALLARD_FORMAT")
        .env_remove("MALLARD_PROJECT")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stderr_json(o: &Output) -> serde_json::Value {
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(err.lines().count(), 1, "one line: {err}");
    serde_json::from_str(&err).unwrap_or_else(|e| panic!("{e}: {err}"))
}

#[test]
fn help_agents_prints_the_guide() {
    let o = mallard(&["help", "agents"]);
    assert_eq!(o.status.code(), Some(0));
    let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/agents.md")).unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim_end(), doc.trim_end());
}

#[test]
fn usage_errors_exit_2_with_json_on_stderr() {
    let o = mallard(&["issues", "lst", "--json"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(o.stdout.is_empty());
    let e = stderr_json(&o);
    assert_eq!(e["error"]["code"], "usage", "{e}");
    let o = mallard(&["issues", "list", "--since", "soon"]);
    assert_eq!(o.status.code(), Some(2), "text mode too");
    assert!(String::from_utf8_lossy(&o.stderr).contains("invalid duration"));
    let o = mallard(&["--format", "ndjson", "trace", "not-hex"]);
    assert_eq!((o.status.code(), stderr_json(&o)["error"]["code"].as_str()), (Some(2), Some("usage")));
}

#[test]
fn status_exits_6_when_the_server_is_down() {
    let o = mallard(&["status", "--json"]);
    assert_eq!(o.status.code(), Some(6));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!((v["ok"].as_bool(), v["sentry"]["ok"].as_bool()), (Some(false), Some(false)), "{v}");
    assert_eq!(v["otel"]["error"]["code"], "unavailable");
    let e = stderr_json(&o);
    assert_eq!((e["error"]["code"].as_str(), e["error"]["retryable"].as_bool()), (Some("unavailable"), Some(true)));
    let o = mallard(&["status"]);
    assert_eq!(o.status.code(), Some(6));
    assert!(String::from_utf8_lossy(&o.stderr).starts_with("error: "));
    let o = mallard(&["issues", "list", "-p", "web", "--json"]);
    assert_eq!((o.status.code(), stderr_json(&o)["error"]["code"].as_str()), (Some(6), Some("unavailable")));
}

#[test]
fn old_paths_still_work() {
    for args in [&["errors", "--help"][..], &["telemetry", "traces", "--help"], &["otel", "logs", "--help"]] {
        assert_eq!(mallard(args).status.code(), Some(0), "{args:?}");
    }
}
