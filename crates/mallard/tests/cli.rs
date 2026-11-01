//! Runs the real `mallard` binary without a server: help, usage errors and
//! the exit code of `status` when the server is down (ADR 0008), and the
//! server commands that need no server: `check`, or in a build without the
//! `server` feature all of them (ADR 0023). The tests against a running server
//! are in `agent_cli.rs`.

use std::process::{Command, Output, Stdio};

fn mallard(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mallard"))
        .args(args)
        .env("MALLARD_URL", "http://127.0.0.1:9")
        .env_remove("MALLARD_TOKEN")
        .env_remove("MALLARD_FORMAT")
        .env_remove("MALLARD_PROJECT")
        .env_remove("MALLARD_CONFIG")
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

/// The commands of the `server` feature.
const SERVER_COMMANDS: [&[&str]; 4] =
    [&["serve"], &["check"], &["maintenance", "--full"], &["install-extensions", "--dir", "x"]];

#[cfg(feature = "server")]
#[test]
fn server_commands_are_in_the_help() {
    let help = String::from_utf8(mallard(&["--help"]).stdout).unwrap();
    for cmd in SERVER_COMMANDS {
        assert!(help.contains(&format!("  {} ", cmd[0])), "{}: {help}", cmd[0]);
    }
}

#[cfg(not(feature = "server"))]
#[test]
fn a_build_without_the_server_says_so() {
    let help = String::from_utf8(mallard(&["--help"]).stdout).unwrap();
    for cmd in SERVER_COMMANDS {
        assert!(!help.contains(&format!("  {} ", cmd[0])), "hidden: {help}");
        let o = mallard(cmd);
        assert_eq!(o.status.code(), Some(2), "{cmd:?}");
        assert!(o.stdout.is_empty());
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.starts_with("error: this build of mallard has no server"), "{err}");
        let o = mallard(&[cmd, &["--json"]].concat());
        let e = stderr_json(&o);
        assert_eq!((o.status.code(), e["error"]["code"].as_str()), (Some(2), Some("usage")), "{e}");
    }
    let o = mallard(&["serve", "-c", "mallard.toml"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("no server"), "the flag parses too");
}

#[cfg(feature = "server")]
#[test]
fn check_validates_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.toml");
    std::fs::write(&good, "[server]\napi_tokens = [\"t\"]\n").unwrap();
    let good = good.to_str().unwrap();
    let o = mallard(&["check", "-c", good]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "config ok\n");
    assert!(o.stderr.is_empty(), "no logging");
    let o = mallard(&["check", "--config", good, "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!((v["ok"].as_bool(), v["connected"].as_bool()), (Some(true), Some(false)), "{v}");

    // Env instead of the flag; a bad key is a usage error with the contract's shape.
    let bad = dir.path().join("bad.toml");
    std::fs::write(&bad, "[server]\nnope = 1\n").unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_mallard"))
        .args(["check", "--json"])
        .env("MALLARD_CONFIG", &bad)
        .env_remove("MALLARD_FORMAT")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(o.stdout.is_empty());
    let e = stderr_json(&o);
    assert_eq!(e["error"]["code"], "usage", "{e}");
    assert!(e["error"]["message"].as_str().unwrap().contains("nope"), "{e}");
    let o = mallard(&["check", "-c", bad.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.starts_with("error: ") && err.trim_end().lines().count() == 1, "{err}");
}
