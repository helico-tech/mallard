//! sentry-cli (Rust, 2.x and 3.x) only accepts a server on the URL root
//! ("bad sentry url: not on URL root"), so its API `/api/0/...` is served on
//! the root of the HTTP port as well as under `/sentry`.

mod common;

use common::TOKEN;

#[tokio::test(flavor = "multi_thread")]
async fn sentry_cli_api_is_served_on_the_root_too() {
    let dir = tempfile::tempdir().unwrap();
    let artifacts = dir.path().join("artifacts").display().to_string();
    let srv = common::start_with(dir.path(), |c| c.storage.artifacts_path = artifacts).await;
    let http = reqwest::Client::new();
    let path = "/api/0/organizations/default/chunk-upload/";

    let mut bodies = Vec::new();
    for base in [srv.url.clone(), format!("{}/sentry", srv.url)] {
        let r = http.get(format!("{base}{path}")).bearer_auth(TOKEN).send().await.unwrap();
        assert_eq!(r.status(), 200, "{base}{path}");
        bodies.push(r.json::<serde_json::Value>().await.unwrap());
    }
    assert_eq!(bodies[0], bodies[1], "the same handler answers both");
    assert_eq!(bodies[0]["hashAlgorithm"], "sha1");

    // Same auth as under /sentry.
    let r = http.get(format!("{}{path}", srv.url)).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = http.post(format!("{}/api/0/organizations/default/releases/", srv.url)).send().await.unwrap();
    assert_eq!(r.status(), 401);
    // Only /api/0 moves: other Sentry paths stay under /sentry.
    let r = http.get(format!("{}/v1/projects", srv.url)).bearer_auth(TOKEN).send().await.unwrap();
    assert_eq!(r.status(), 404);
    srv.stop().await;
}
