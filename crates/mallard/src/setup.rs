//! `mallard setup <name>`: get or create the project and print what an app
//! needs to send errors and telemetry, as `KEY=value` lines (ready for
//! `>> .env`) or JSON. Safe to run again: an existing project is reused.
//! `--sentry-cli` adds what `sentry-cli` reads for sourcemap uploads.

use std::collections::BTreeMap;

use anyhow::Result;
use mallard_client::mallard_sentry_client::types::Project;
use serde::Serialize;

use crate::Run;

#[derive(clap::Args, Debug)]
pub(crate) struct SetupArgs {
    /// Project name; also the OTel service name.
    name: String,
    /// URL-safe id (derived from the name when omitted). The same slug under the slug rule on
    /// another project (`my_app`, `my-app`) is a conflict, exit 5.
    #[arg(long)]
    slug: Option<String>,
    /// SDK platform, e.g. node, javascript, python.
    #[arg(long)]
    platform: Option<String>,
    /// Also SENTRY_URL, SENTRY_ORG, SENTRY_PROJECT and SENTRY_AUTH_TOKEN for sentry-cli.
    #[arg(long)]
    sentry_cli: bool,
}

#[derive(Debug, Serialize)]
struct Setup {
    project: Project,
    /// False when the project already existed.
    created: bool,
    env: BTreeMap<String, String>,
    /// OTLP ingest needs a token: set `OTEL_EXPORTER_OTLP_HEADERS` to
    /// `Authorization=Bearer <ingest token>` (not printed: the CLI does not know it).
    ingest_auth: bool,
}

pub(crate) async fn run(r: &Run, a: SetupArgs) -> Result<()> {
    let ctx = r.sentry();
    let (project, created) = mallard_sentry_cli::create_or_get(&ctx, &a.name, a.slug, a.platform).await?;
    // Both answers are optional: older servers, or a side that is down,
    // still give a DSN and the CLI's own URL.
    let (info, stats) = tokio::join!(r.client.sentry.info(), r.client.otel.stats());
    let public = info.ok().and_then(|i| i.public_url);
    let ingest_auth = stats.ok().and_then(|s| s.capabilities).is_some_and(|c| c.ingest_auth);
    let mut env = env_vars(&project, public.as_deref(), r.client.base_url());
    if a.sentry_cli {
        let base = env["OTEL_EXPORTER_OTLP_ENDPOINT"].clone();
        env.extend(sentry_cli_vars(&project, &base, r.client.token()));
    }
    let setup = Setup { project, created, env, ingest_auth };
    if !r.output.text() {
        return r.output.object(&setup);
    }
    let what = if setup.created { "created project" } else { "project exists:" };
    r.output.note(format!("{what} {} (id {})", setup.project.slug, setup.project.id));
    let mut lines: Vec<String> = setup.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    if a.sentry_cli && r.client.token().is_none() {
        lines.push("# sentry-cli needs a token: SENTRY_AUTH_TOKEN=<one of the server's api_tokens>".into());
    }
    if ingest_auth {
        lines
            .push("# OTLP ingest needs a token: OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer <ingest token>".into());
    }
    r.output.out.line(&mallard_term::text::clean_text(&lines.join("\n")))?;
    Ok(())
}

/// The env vars of the Sentry and OpenTelemetry SDKs. OTLP/HTTP is served
/// on the same address as the API: the server's public URL without the
/// `/sentry` prefix, else the URL the CLI uses.
fn env_vars(p: &Project, public_url: Option<&str>, cli_url: &str) -> BTreeMap<String, String> {
    let base = public_url
        .map(|u| u.trim_end_matches('/'))
        .map(|u| u.strip_suffix("/sentry").unwrap_or(u))
        .unwrap_or(cli_url)
        .trim_end_matches('/')
        .to_string();
    BTreeMap::from([
        ("SENTRY_DSN".to_string(), p.dsn.clone()),
        ("OTEL_EXPORTER_OTLP_ENDPOINT".to_string(), base),
        ("OTEL_EXPORTER_OTLP_PROTOCOL".to_string(), "http/protobuf".to_string()),
        ("OTEL_SERVICE_NAME".to_string(), p.slug.clone()),
    ])
}

/// What `sentry-cli` reads. Its URL must be the server's root (sentry-cli
/// refuses a URL with a path, so not `…/sentry`); any org slug is accepted
/// (`default`, as in the docs); the token is the CLI's own API token.
fn sentry_cli_vars(p: &Project, base: &str, token: Option<&str>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        ("SENTRY_URL".to_string(), base.to_string()),
        ("SENTRY_ORG".to_string(), "default".to_string()),
        ("SENTRY_PROJECT".to_string(), p.slug.clone()),
    ]);
    if let Some(t) = token {
        env.insert("SENTRY_AUTH_TOKEN".to_string(), t.to_string());
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Project {
        Project {
            id: 3,
            slug: "checkout".into(),
            name: "Checkout".into(),
            platform: None,
            public_key: "k".into(),
            dsn: "http://k@mallard:8080/sentry/3".into(),
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn otlp_endpoint_is_the_public_url_without_the_sentry_prefix() {
        let env = env_vars(&project(), Some("http://mallard:8080/sentry/"), "http://localhost:8080");
        assert_eq!(env["OTEL_EXPORTER_OTLP_ENDPOINT"], "http://mallard:8080");
        assert_eq!(env["OTEL_SERVICE_NAME"], "checkout");
        assert_eq!(env["SENTRY_DSN"], "http://k@mallard:8080/sentry/3");
        let env = env_vars(&project(), None, "http://localhost:8080/");
        assert_eq!(env["OTEL_EXPORTER_OTLP_ENDPOINT"], "http://localhost:8080");
    }

    #[test]
    fn sentry_cli_vars_use_the_root_url() {
        let env = sentry_cli_vars(&project(), "http://mallard:8080", Some("tok"));
        assert_eq!(env["SENTRY_URL"], "http://mallard:8080", "sentry-cli refuses a path");
        assert_eq!((env["SENTRY_ORG"].as_str(), env["SENTRY_PROJECT"].as_str()), ("default", "checkout"));
        assert_eq!(env["SENTRY_AUTH_TOKEN"], "tok");
        assert!(!sentry_cli_vars(&project(), "http://x", None).contains_key("SENTRY_AUTH_TOKEN"));
    }
}
