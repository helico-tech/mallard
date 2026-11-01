//! `--app <slug>`: which project and services a command means. An app is a project and
//! the services with the same slug (ADR 0027), so the server's list decides; it is read
//! once, before the command runs, and each command then uses its own side.
//!
//! The rule with `-p` and `-S`: `--app` replaces them, so giving one next to it is a usage
//! error. `MALLARD_PROJECT` gives way to the flag (see `cli_from` in `main.rs`). A side the
//! app lacks is an error, not an empty list that looks like "no errors".

use anyhow::Result;
use chrono::{DateTime, TimeDelta, Utc};
use mallard_client::Client;
use mallard_client::mallard_api::{AppList, AppsQuery};
use mallard_term::error::{Described, Kind, usage};
use mallard_term::text::clean;
use mallard_term::window::{Window, WindowArgs};

use crate::{Cmd, MetricsCmd, SchemaArgs, SchemaCmd, TracesCmd};

/// A command's scope: sent as `-p` and as `service` values.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Scope {
    pub project: Option<String>,
    pub services: Vec<String>,
}

/// What a command takes from an app.
#[derive(Debug, PartialEq, Eq)]
enum Need {
    /// The errors side. Projects are listed whatever the window.
    Project,
    /// The telemetry side, for this window.
    Services(Window),
    /// The initial choice of the picker, which reads the list itself.
    Picker,
}

/// Which window a telemetry command looks at (`--around` has its own).
fn services_need(w: &WindowArgs, service: Option<&String>, around: Option<Window>) -> Result<Need> {
    if service.is_some() {
        return Err(usage("--app already chooses the services: use --app or -S, not both"));
    }
    Ok(Need::Services(match around {
        Some(w) => w,
        None => w.resolve(mallard_otel_cli::DEFAULT_WINDOW)?,
    }))
}

fn log_need(a: &mallard_otel_cli::LogArgs) -> Result<Need> {
    let around = match a.around {
        Some(t) => {
            let d = mallard_term::window::parse_duration(&a.window).map_err(usage)?;
            let d = TimeDelta::from_std(d)?;
            Some(Window { from: t - d, to: t + d })
        }
        None => None,
    };
    services_need(&a.w, a.service.as_ref(), around)
}

fn event_need(c: &mallard_sentry_cli::EventsCmd) -> Result<Need> {
    match c {
        mallard_sentry_cli::EventsCmd::List { trace: Some(_), .. } => {
            Err(usage("--app does not apply with --trace: a trace is searched across all projects"))
        }
        _ => Ok(Need::Project),
    }
}

fn need(cmd: &Cmd) -> Result<Need> {
    use mallard_otel_cli::Command as Otel;
    use mallard_sentry_cli::Command as Errors;
    match cmd {
        Cmd::Issues(_) | Cmd::Releases(_) | Cmd::Sourcemaps(_) | Cmd::Find { .. } => Ok(Need::Project),
        Cmd::Events(c) => event_need(c),
        Cmd::Errors { cmd: Errors::Issues(_) | Errors::Stats { .. } | Errors::Releases(_) | Errors::Sourcemaps(_) } => {
            Ok(Need::Project)
        }
        Cmd::Errors { cmd: Errors::Events(c) } => event_need(c),
        Cmd::Traces(TracesCmd::List(a)) | Cmd::Telemetry { cmd: Otel::Traces(a) } => {
            services_need(&a.w, a.service.as_ref(), None)
        }
        Cmd::Logs(a) | Cmd::Telemetry { cmd: Otel::Logs(a) } => log_need(a),
        Cmd::Ops(a) | Cmd::Telemetry { cmd: Otel::Ops(a) } => services_need(&a.w, a.service.as_ref(), None),
        Cmd::Metrics(MetricsCmd::List(a)) | Cmd::Telemetry { cmd: Otel::Metrics(a) } => {
            services_need(&a.w, a.service.as_ref(), None)
        }
        Cmd::Metrics(MetricsCmd::Show(a)) | Cmd::Telemetry { cmd: Otel::Metric(a) } => {
            services_need(&a.w, a.service.as_ref(), None)
        }
        Cmd::Schema(SchemaArgs { cmd: Some(SchemaCmd::Attrs(a)) }) | Cmd::Telemetry { cmd: Otel::Attrs(a) } => {
            services_need(&a.w, a.service.as_ref(), None)
        }
        Cmd::Series(c) => match c.scope() {
            Some((w, service)) => services_need(w, service, None),
            None => Ok(Need::Project),
        },
        Cmd::Tui => Ok(Need::Picker),
        _ => Err(usage(
            "--app does not apply to this command; it works where -p or -S do: issues, events, releases, sourcemaps, \
             find, traces list, logs, ops, metrics, schema attrs, series, tui",
        )),
    }
}

/// Telemetry of a window in the past needs that window; a quiet app must not look
/// unknown because of a short one, so the last 24 hours always count too.
fn lookup_window(w: Window, now: DateTime<Utc>) -> Window {
    Window { from: w.from.min(now - TimeDelta::days(1)), to: w.to.max(now) }
}

/// The scope of `--app slug` for `cmd`. Reads the list unless the picker will.
pub(crate) async fn resolve(client: &Client, slug: &str, cmd: &Cmd) -> Result<Scope> {
    let need = need(cmd)?;
    let q = match need {
        Need::Picker => return Ok(Scope { project: Some(slug.to_string()), services: vec![] }),
        Need::Project => AppsQuery::default(),
        Need::Services(w) => {
            let w = lookup_window(w, Utc::now());
            AppsQuery { from: Some(w.from), to: Some(w.to), since: None }
        }
    };
    let list = client.apps(&q).await.map_err(|e| crate::described(e, client.base_url()))?;
    let scope = check(slug, &list, &need)?;
    for warning in &list.warnings {
        eprintln!("warning: {}", clean(warning));
    }
    Ok(scope)
}

/// `slug`'s side of the list. Something missing from a list with warnings may only be
/// unreadable, so that is "try again" (exit 6), never "not found" (exit 4).
fn check(slug: &str, list: &AppList, need: &Need) -> Result<Scope> {
    let name = clean(slug);
    let missing = |message: String, hint: String| {
        let d = if list.warnings.is_empty() {
            Described::new(Kind::NotFound, message).hint(hint)
        } else {
            let why: Vec<String> = list.warnings.iter().map(|w| clean(w).into_owned()).collect();
            Described::new(
                Kind::Unavailable,
                format!("{message}, but the list of apps is incomplete: {}", why.join("; ")),
            )
            .hint("try again")
        };
        anyhow::Error::new(d)
    };
    let Some(app) = list.apps.iter().find(|a| a.slug == slug) else {
        return Err(missing(
            format!("no app {name:?}"),
            "mallard apps lists them; services need data in the last 24h".to_string(),
        ));
    };
    match need {
        Need::Project if app.project.is_none() => Err(missing(
            format!("app {name:?} has no project, so it has no errors to show"),
            format!("mallard setup {name} gives it a DSN"),
        )),
        Need::Services(_) if app.services.is_empty() => Err(missing(
            format!("app {name:?} has no telemetry in this window or the last 24h"),
            format!(
                "set OTEL_SERVICE_NAME={name} in the app, or widen --since; -S {name} works for data still on its way"
            ),
        )),
        Need::Project => Ok(Scope { project: Some(app.slug.clone()), services: vec![] }),
        Need::Services(_) => Ok(Scope { project: None, services: app.services.clone() }),
        Need::Picker => unreachable!("handled by the caller"),
    }
}

#[cfg(test)]
mod tests {
    use mallard_client::mallard_api::App;

    use super::*;

    /// The exit code and the message of a failed check.
    fn failed(r: Result<Scope>) -> (u8, String) {
        let r = r.map(|_| ());
        let code = mallard_term::error::exit_code(&r);
        (code, mallard_term::error::message(&r.unwrap_err()))
    }

    fn list(warnings: &[&str]) -> AppList {
        let app = |slug: &str, project, services: &[&str]| App {
            slug: slug.into(),
            name: slug.into(),
            project,
            services: services.iter().map(|s| s.to_string()).collect(),
        };
        AppList {
            apps: vec![
                app("checkout-api", Some(3), &["Checkout API", "checkout-api"]),
                app("web", Some(4), &[]),
                app("worker", None, &["worker"]),
            ],
            warnings: warnings.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn window() -> Need {
        Need::Services(Window { from: Utc::now() - TimeDelta::hours(1), to: Utc::now() })
    }

    #[test]
    fn each_side_gets_its_part_of_the_app() {
        let s = check("checkout-api", &list(&[]), &Need::Project).unwrap();
        assert_eq!(s, Scope { project: Some("checkout-api".into()), services: vec![] });
        let s = check("checkout-api", &list(&[]), &window()).unwrap();
        assert_eq!(s.services, ["Checkout API", "checkout-api"], "both service names");
    }

    #[test]
    fn a_missing_side_is_not_found_and_says_why() {
        for (slug, need, says) in [
            ("nope", Need::Project, "no app"),
            ("worker", Need::Project, "has no project"),
            ("web", window(), "has no telemetry"),
        ] {
            let (code, m) = failed(check(slug, &list(&[]), &need));
            assert_eq!(code, 4, "{slug}");
            assert!(m.contains(says) && m.contains(slug), "{m}");
        }
    }

    /// A side that could not be read must not look like a side the app lacks.
    #[test]
    fn missing_from_an_incomplete_list_is_try_again() {
        let warn = ["telemetry: services could not be read: boom\x1b[2J"];
        let (code, m) = failed(check("web", &list(&warn), &window()));
        assert_eq!(code, 6);
        assert!(m.contains("incomplete") && !m.contains('\x1b'), "{m}");
        assert!(check("worker", &list(&warn), &window()).is_ok(), "what is there is used");
        assert_eq!(failed(check("nope", &list(&warn), &Need::Project)).0, 6);
    }

    #[test]
    fn the_lookup_covers_the_window_and_the_last_day() {
        let now = Utc::now();
        let short = Window { from: now - TimeDelta::minutes(5), to: now };
        assert_eq!(lookup_window(short, now), Window { from: now - TimeDelta::days(1), to: now });
        let old = Window { from: now - TimeDelta::days(30), to: now - TimeDelta::days(29) };
        assert_eq!(lookup_window(old, now), Window { from: old.from, to: now });
    }

    #[test]
    fn hostile_slugs_are_cleaned_in_messages() {
        let (_, m) = failed(check("a\x1b[2J", &list(&[]), &Need::Project));
        assert!(!m.contains('\x1b'), "{m}");
    }
}
