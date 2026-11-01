//! `mallard apps`: the one list of apps the server works out (ADR 0027). An app
//! is a project and the services with the same slug, or either alone.

use std::time::Duration;

use anyhow::Result;
use comfy_table::Cell;
use mallard_client::mallard_api::{App, AppsQuery};
use mallard_term::output::List;
use mallard_term::table::{right, table};
use mallard_term::text::clean;
use mallard_term::window::WindowArgs;

/// Default window, as the server's: an app that reports now and then stays listed.
const WINDOW: Duration = Duration::from_secs(86_400);

pub(crate) async fn run(r: &crate::Run, window: &WindowArgs) -> Result<()> {
    let w = window.resolve(WINDOW)?;
    let q = AppsQuery { from: Some(w.from), to: Some(w.to), since: None };
    let found = r.client.apps(&q).await.map_err(|e| crate::described(e, r.client.base_url()))?;
    for warning in &found.warnings {
        eprintln!("warning: {}", clean(warning));
    }
    if r.output.text() {
        r.output.out.line(&apps_text(&found.apps))?;
    } else {
        r.output.list(&List::new(found.apps).window(w))?;
    }
    Ok(())
}

fn apps_text(apps: &[App]) -> String {
    if apps.is_empty() {
        return "no apps yet: send data, or create a project with mallard setup <name>".into();
    }
    let mut t = table(&["app", "name", "project", "services"]);
    for a in apps {
        let services: Vec<String> = a.services.iter().map(|s| clean(s).to_string()).collect();
        t.add_row(vec![
            Cell::new(clean(&a.slug)),
            Cell::new(clean(&a.name)),
            right(a.project.map_or_else(|| "-".to_string(), |p| p.to_string())),
            Cell::new(if services.is_empty() { "-".to_string() } else { services.join(", ") }),
        ]);
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_show_which_side_they_have() {
        let app = |slug: &str, project, services: &[&str]| App {
            slug: slug.into(),
            name: slug.into(),
            project,
            services: services.iter().map(|s| s.to_string()).collect(),
        };
        let text = apps_text(&[app("web", Some(3), &["web", "Web"]), app("api", None, &["api\x1b[31m"])]);
        let text = mallard_term::color::strip_ansi(&text);
        assert!(text.contains("web, Web"), "{text}");
        assert!(!text.contains('\x1b'), "server text is cleaned: {text}");
        assert!(apps_text(&[]).starts_with("no apps"));
    }
}
