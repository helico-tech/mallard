//! docs/sentry/openapi.json must match the code. Regenerate with:
//! `UPDATE_OPENAPI=1 cargo test -p sdl-server --test openapi`

#[test]
fn openapi_document_is_current() {
    let generated = sdl_server::Server::openapi().to_pretty_json().unwrap();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/sentry/openapi.json");
    if std::env::var_os("UPDATE_OPENAPI").is_some() {
        std::fs::write(path, generated + "\n").unwrap();
        return;
    }
    let committed = std::fs::read_to_string(path).expect("docs/sentry/openapi.json exists");
    assert_eq!(
        committed.trim(),
        generated.trim(),
        "docs/sentry/openapi.json is stale; run `UPDATE_OPENAPI=1 cargo test -p sdl-server --test openapi`"
    );
}
