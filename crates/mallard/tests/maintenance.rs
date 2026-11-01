//! `mallard maintenance [--full]`: the real binary on a temp lake with
//! data from both sides.

#[path = "../../mallard-server/tests/common/mod.rs"]
mod common;

use std::path::Path;
use std::process::{Command, Output, Stdio};

use common::*;
use mallard_client::{Client, mallard_sentry_client::types::CreateProject};

async fn maintenance(config: &Path, extra: &[&str]) -> Output {
    let (config, extra) = (config.to_path_buf(), extra.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    tokio::task::spawn_blocking(move || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_mallard"));
        c.arg("maintenance").arg("-c").arg(&config).args(&extra).stdin(Stdio::null());
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("MALLARD_") {
                c.env_remove(k);
            }
        }
        c.output().unwrap()
    })
    .await
    .unwrap()
}

/// Parquet files below `dir` whose path contains `part`.
fn parquet_files(dir: &Path, part: &str) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| {
                    let p = e.path();
                    if p.is_dir() {
                        parquet_files(&p, part)
                    } else {
                        usize::from(p.extension().is_some_and(|x| x == "parquet") && p.to_string_lossy().contains(part))
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_shot_maintenance_runs_both_sides() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("mallard.toml");
    let toml = format!(
        "[server]\napi_tokens = [\"t\"]\n[storage]\ncatalog = {:?}\ndata_path = {:?}\n",
        dir.path().join("catalog.ducklake").display().to_string(),
        format!("{}/lake/", dir.path().display()),
    );
    std::fs::write(&file, toml).unwrap();

    let srv = start(dir.path()).await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    let project =
        client.sentry.create_project(&CreateProject { name: "shop".into(), slug: None, platform: None }).await.unwrap();
    let http = reqwest::Client::new();
    send_sentry_event(&http, &srv.url, project.id, &project.public_key, 0).await;
    send_otlp(&http, &srv.otlp_url, 0).await;
    eventually("the trace", || {
        let client = &client;
        async move { client.trace_overview(&ids(0).0, None).await.ok().filter(|t| t.spans.len() == 1) }
    })
    .await;

    // A running server holds the DuckDB-file catalog: refused, nothing done.
    let o = maintenance(&file, &[]).await;
    assert!(!o.status.success(), "stdout: {}", String::from_utf8_lossy(&o.stdout));
    srv.stop().await;

    let o = maintenance(&file, &["--full"]).await;
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "stdout: {stdout}\nstderr: {}", String::from_utf8_lossy(&o.stderr));
    for step in ["sentry: flushed", "otel: compaction:", "otel: full compaction:", "otel: retention:"] {
        assert!(stdout.contains(step), "{step}: {stdout}");
    }
    let lake = dir.path().join("lake");
    assert!(parquet_files(&lake, "events") > 0, "Sentry events flushed to Parquet: {stdout}");
    assert!(parquet_files(&lake, "spans") > 0, "spans flushed to Parquet: {stdout}");

    // Without --full, and on a lake that has nothing left to do.
    let o = maintenance(&file, &[]).await;
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && !stdout.contains("full compaction"), "{stdout}");

    // The data is still there.
    let srv = start(dir.path()).await;
    let client = Client::new(&srv.url, Some(TOKEN)).unwrap();
    let t = client.trace_overview(&ids(0).0, None).await.unwrap();
    assert_eq!((t.spans.len(), t.errors.len(), t.logs.len()), (1, 1, 1));
    srv.stop().await;
}
