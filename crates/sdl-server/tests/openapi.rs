//! docs/sentry/openapi.json must match the code. Regenerate with:
//! `cargo run -p sdl-server -- openapi > docs/sentry/openapi.json`

#[test]
fn openapi_document_is_current() {
    let generated = sdl_server::Server::openapi().to_pretty_json().unwrap();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/sentry/openapi.json");
    let committed = std::fs::read_to_string(path).expect("docs/sentry/openapi.json exists");
    assert_eq!(
        committed.trim(),
        generated.trim(),
        "docs/sentry/openapi.json is stale; run `cargo run -p sdl-server -- openapi > docs/sentry/openapi.json`"
    );
}
