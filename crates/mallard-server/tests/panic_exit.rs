//! A test that panics before `Running::stop` must fail as a plain test
//! failure (exit 101). It used to end in SIGSEGV: the Sentry writer thread was
//! left running when the process exited, inside DuckDB while its C++ static
//! destructors ran (issue 0018). So this runs the failing test in a child.

mod common;

use std::process::Command;

use common::*;
use mallard_client::{Client, sdl_client::types::CreateProject};

const CHILD: &str = "MALLARD_TEST_PANIC_CHILD";

/// Fails on purpose, only in the child below.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn child_panics_before_stop() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let srv = start(dir.path()).await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    let project =
        client.sentry.create_project(&CreateProject { name: "shop".into(), slug: None, platform: None }).await.unwrap();
    let http = reqwest::Client::new();
    send_sentry_event(&http, &srv.url, project.id, &project.public_key, 0).await;
    send_otlp(&http, &srv.otlp_url, 0).await;
    panic!("deliberate failure before srv.stop()");
}

#[test]
fn a_panic_before_stop_is_a_plain_test_failure() {
    // The crash was timing dependent (5 of 6 runs): try a few times.
    for _ in 0..3 {
        let o = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_panics_before_stop", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(stderr.contains("deliberate failure"), "the child ran the test: {stderr}");
        assert_eq!(o.status.code(), Some(101), "{:?}\n{stderr}", o.status);
    }
}
