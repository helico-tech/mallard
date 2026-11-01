//! docs/openapi.json must match the code. Regenerate with:
//! `cargo run -p odl-server -- openapi > docs/openapi.json`

#[test]
fn openapi_document_is_current() {
    let generated = odl_server::api::openapi().to_pretty_json().unwrap();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/openapi.json");
    let committed = std::fs::read_to_string(path).expect("docs/openapi.json exists");
    assert_eq!(
        committed.trim(),
        generated.trim(),
        "docs/openapi.json is stale; run `cargo run -p odl-server -- openapi > docs/openapi.json`"
    );
}
