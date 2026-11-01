//! `mallard serve`: the real binary as the server, on a temp lake, with
//! `mallard status` against it. Stops on SIGTERM; logs stay off stdout.
#![cfg(unix)]

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_mallard");

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn status(url: &str) -> Output {
    Command::new(BIN)
        .args(["status", "--json", "--url", url, "--token", "t"])
        .env_remove("MALLARD_FORMAT")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn serve_runs_the_server_for_the_cli() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let url = format!("http://127.0.0.1:{port}");
    let config = dir.path().join("mallard.toml");
    let toml = format!(
        "[server]\nhttp_addr = \"127.0.0.1:{port}\"\notlp_http_addr = \"127.0.0.1:0\"\n\
         otlp_grpc_addr = \"127.0.0.1:0\"\npublic_url = {url:?}\napi_tokens = [\"t\"]\n\
         [storage]\ncatalog = {:?}\ndata_path = {:?}\n",
        dir.path().join("catalog.ducklake").display().to_string(),
        format!("{}/lake/", dir.path().display()),
    );
    std::fs::write(&config, toml).unwrap();

    let mut serve = Command::new(BIN);
    serve.arg("serve").arg("-c").arg(&config).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("MALLARD_") {
            serve.env_remove(k);
        }
    }
    let serve = serve.spawn().unwrap();

    // Exit 6 until both sides are up.
    let deadline = Instant::now() + Duration::from_secs(60);
    let up = loop {
        let o = status(&url);
        if o.status.code() == Some(0) {
            break o;
        }
        if Instant::now() > deadline {
            let _ = Command::new("kill").arg("-KILL").arg(serve.id().to_string()).status();
            let o = serve.wait_with_output().unwrap();
            panic!("server did not come up: {}", String::from_utf8_lossy(&o.stderr));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let st: serde_json::Value = serde_json::from_slice(&up.stdout).unwrap();
    assert_eq!(st["ok"], true, "{st}");

    assert!(Command::new("kill").arg("-TERM").arg(serve.id().to_string()).status().unwrap().success());
    let o = serve.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "clean shutdown: {stderr}");
    assert!(o.stdout.is_empty(), "stdout is data only: {}", String::from_utf8_lossy(&o.stdout));
    assert!(stderr.contains("stopped"), "logs on stderr: {stderr}");
}
