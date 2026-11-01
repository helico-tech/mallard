//! Runs the real `sdl` binary against a tiny fake HTTP server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// Serve canned responses: `routes` maps a path prefix to (status, body).
/// Each response waits `delay` first, so a test can close the client's
/// stdout before anything is printed.
fn serve(routes: Vec<(&'static str, u16, String)>, delay: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; len];
            let _ = reader.read_exact(&mut body);
            let path = request_line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, body) = routes
                .iter()
                .find(|(prefix, _, _)| path.starts_with(prefix))
                .map(|(_, s, b)| (*s, b.clone()))
                .unwrap_or((404, r#"{"error":"no route"}"#.to_string()));
            std::thread::sleep(delay);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://{addr}")
}

fn sdl(url: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sdl"))
        .args(args)
        .env("SDL_URL", url)
        .env("SDL_TOKEN", "t")
        .env("SDL_PROJECT", "web")
        .env_remove("NO_COLOR")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

const EVIL: &str = "evil\\u001b]52;c;cHduZWQ=\\u0007 \\u001b[2J\\u009b31m\\r\\tend";

fn issue_json() -> String {
    format!(
        r#"{{"id":"a1b2c3d4e5f6a7b8","project_id":1,"title":"{EVIL}","culprit":"{EVIL}","level":"error",
        "platform":"node","exception_type":null,"status":"unresolved","first_seen":"2026-10-31T10:00:00Z",
        "last_seen":"2026-10-31T11:00:00Z","count":3,"user_count":1,"trend":[0,1,2]"#
    )
}

fn assert_no_escapes(out: &[u8]) {
    let text = String::from_utf8_lossy(out);
    let bad: Vec<char> = text.chars().filter(|c| c.is_control() && *c != '\n').collect();
    assert!(bad.is_empty(), "control characters {bad:?} in output:\n{text}");
}

#[test]
fn issue_and_event_fields_cannot_inject_terminal_escapes() {
    let list = format!(
        r#"{{"issues":[{}}}],"window_start":"2026-10-17T00:00:00Z","window_end":"2026-10-31T12:00:00Z"}}"#,
        issue_json()
    );
    let detail = format!(
        r#"{}, "first_release":"{EVIL}","last_release":null,"latest_event_id":"e1","tags":[{{"key":"{EVIL}","unique":1,"top":[{{"value":"{EVIL}","count":1}}]}}],"activity":[]}}"#,
        issue_json()
    );
    let event = format!(
        r#"{{"event_id":"e1","project_id":1,"issue_id":"a1b2c3d4e5f6a7b8","timestamp":"2026-10-31T11:00:00Z",
        "received_at":"2026-10-31T11:00:00Z","title":"{EVIL}","level":"error","platform":"node",
        "environment":"{EVIL}","release":null,"user":"{EVIL}","message":"{EVIL}","trace_id":null,"span_id":null,
        "culprit":"{EVIL}","tags":[["{EVIL}","{EVIL}"]],
        "payload":{{"message":"{EVIL}","exception":{{"values":[{{"type":"{EVIL}","value":"{EVIL}",
          "stacktrace":{{"frames":[{{"function":"{EVIL}","filename":"{EVIL}","lineno":18446744073709551615,"in_app":true,
            "pre_context":["{EVIL}"],"context_line":"{EVIL}","post_context":["{EVIL}"]}}]}}}}]}},
          "breadcrumbs":{{"values":[{{"message":"{EVIL}","category":"{EVIL}","level":"info"}}]}},
          "user":{{"{EVIL}":"{EVIL}"}},
          "request":{{"method":"GET","url":"{EVIL}","headers":[["{EVIL}","{EVIL}"]]}}}}}}"#
    );
    let url = serve(
        vec![
            ("/v1/projects/web/issues/", 200, detail),
            ("/v1/projects/web/issues", 200, list),
            ("/v1/projects/web/events/e1", 200, event),
        ],
        Duration::ZERO,
    );

    let out = sdl(&url, &["issues", "list"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_no_escapes(&out.stdout);
    assert!(String::from_utf8_lossy(&out.stdout).contains("evil"));

    let out = sdl(&url, &["issues", "show", "a1b2c3d4e5f6a7b8"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_no_escapes(&out.stdout);
    // The hint works even with several projects.
    assert!(String::from_utf8_lossy(&out.stdout).contains("sdl events show e1 --issue a1b2c3d4e5f6a7b8 -p web"));

    let out = sdl(&url, &["events", "show", "e1"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_no_escapes(&out.stdout);
    assert!(String::from_utf8_lossy(&out.stdout).contains("Breadcrumbs"));

    // --json keeps the data as is; serde escapes it.
    let out = sdl(&url, &["events", "show", "e1", "--json"]);
    assert!(out.status.success());
    assert_no_escapes(&out.stdout);
    assert!(String::from_utf8_lossy(&out.stdout).contains("\\u001b]52"));
}

#[test]
fn closed_stdout_is_not_a_crash() {
    let list = format!(
        r#"{{"issues":[{}}}],"window_start":"2026-10-17T00:00:00Z","window_end":"2026-10-31T12:00:00Z"}}"#,
        issue_json()
    );
    let url = serve(vec![("/v1/projects/web/issues", 200, list)], Duration::from_millis(300));
    for args in [&["issues", "list", "--json"][..], &["issues", "list"][..]] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sdl"))
            .args(args)
            .env("SDL_URL", &url)
            .env("SDL_TOKEN", "t")
            .env("SDL_PROJECT", "web")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdout.take()); // like `| true`: the reader is gone before sdl prints
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn tui_without_a_terminal_fails_with_a_message() {
    let out = sdl("http://127.0.0.1:9", &["tui"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("needs an interactive terminal"), "{err}");
}

#[test]
fn maintenance_errors_use_the_api_error_message() {
    let url = serve(vec![("/v1/maintenance", 500, r#"{"error":"disk full"}"#.into())], Duration::ZERO);
    let out = sdl(&url, &["maintenance"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("server error (500): disk full") && !err.contains('{'), "{err}");
}

#[test]
fn color_flag_and_short_flags() {
    let list = format!(
        r#"{{"issues":[{}}}],"window_start":"2026-10-17T00:00:00Z","window_end":"2026-10-31T12:00:00Z"}}"#,
        issue_json()
    );
    let url = serve(vec![("/v1/projects/web/issues", 200, list)], Duration::ZERO);
    // Piped stdout: no colour by default, colour with --color always.
    let plain = sdl(&url, &["issues", "list", "-s", "1h", "-n", "5"]);
    assert!(plain.status.success(), "{}", String::from_utf8_lossy(&plain.stderr));
    assert!(!plain.stdout.contains(&0x1b));
    let colored = sdl(&url, &["issues", "list", "--color", "always"]);
    let text = String::from_utf8_lossy(&colored.stdout);
    assert!(text.contains("\u{1b}["), "{text}");
    // The only escape sequences are our own styling.
    let stripped = mallard_term::color::strip_ansi(&text);
    assert!(!stripped.chars().any(|c| c.is_control() && c != '\n'), "{stripped}");
}

#[test]
fn errors_print_one_friendly_line() {
    let out = sdl("http://127.0.0.1:9", &["info", "--color", "never"]);
    assert_eq!(out.status.code(), Some(6), "unreachable is exit code 6 (ADR 0008)");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.starts_with("error: cannot reach server at http://127.0.0.1:9 (is it running? set SDL_URL)"), "{err}");
}
