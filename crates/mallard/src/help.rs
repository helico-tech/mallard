//! Help: examples under every command's `--help`, and `mallard help
//! agents`, the guide for AI agents (the same text as `docs/agents.md`).

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches};
use mallard_term::output::Output;

use crate::Cli;

/// `docs/agents.md`, built in.
pub(crate) const AGENTS_GUIDE: &str = include_str!("../../../docs/agents.md");

const ROOT: &str = "\
Examples:
  mallard status
  mallard setup checkout --platform node >> .env
  mallard issues list --since 1h --sort count -n 10
  mallard find 0af7651916cd43dd8448eb211c80319c
  mallard logs -S checkout -l warn --since 10m --format ndjson

Output: --format text|json|ndjson (or MALLARD_FORMAT); JSON is compact when piped.
Exit codes: 0 ok, 1 internal, 2 usage, 3 auth, 4 not found, 5 conflict,
6 unavailable (retry), 7 disabled on the server, 8 condition not met.
AI agents: mallard help agents";

/// Examples per command path.
const EXAMPLES: &[(&[&str], &str)] = &[
    (&["status"], "mallard status\n  mallard status --json   # exit 6 when a side is down"),
    (
        &["setup"],
        "mallard setup checkout --platform node >> .env\n  mallard setup checkout --json   # {\"project\":…,\"created\":false,\"env\":{…}}\n  mallard setup checkout --sentry-cli >> .env   # adds SENTRY_URL, SENTRY_ORG, SENTRY_PROJECT, SENTRY_AUTH_TOKEN",
    ),
    (
        &["find"],
        "mallard find 0af7651916cd43dd8448eb211c80319c   # trace and/or Sentry event\n  mallard find 85fede14250ec6be                   # issue\n  mallard find 0af7651916cd43dd8448eb211c80319c --wait 30s   # exit 8 if nothing arrives",
    ),
    (&["issues"], "mallard issues list --since 1h\n  mallard issues show 85fede14250ec6be"),
    (
        &["issues", "list"],
        "mallard issues list --since 1h --sort count -n 10 --fields id,title,count,last_seen\n  mallard issues list --new --since 1d\n  mallard issues list --first-release web@1.4.0 --status all\n  mallard issues list --tag url=/checkout --env production\n  mallard issues list --cursor <next_cursor>   # the next page",
    ),
    (
        &["issues", "show"],
        "mallard issues show 85fede14250ec6be --since 1h\n  mallard issues show 85fede14250ec6be --json   # with latest_event: exception, frames, trace_id",
    ),
    (&["issues", "resolve"], "mallard issues resolve 85fede14250ec6be --reason \"fixed in #123\""),
    (&["issues", "ignore"], "mallard issues ignore 85fede14250ec6be --reason \"third-party noise\""),
    (&["issues", "unresolve"], "mallard issues unresolve 85fede14250ec6be"),
    (&["events"], "mallard events list --issue 85fede14250ec6be -n 5\n  mallard events show <event-id>"),
    (
        &["events", "list"],
        "mallard events list --issue 85fede14250ec6be -n 5\n  mallard events list --trace 0af7651916cd43dd8448eb211c80319c --wait 30s   # exit 8 if none arrives\n  mallard events list --since 1h --tag user=u-42 --release web@1.4.0\n  mallard events list --cursor <next_cursor>   # the next page",
    ),
    (
        &["events", "show"],
        "mallard events show <event-id> --issue 85fede14250ec6be\n  mallard events show <event-id> --json --raw   # with the full Sentry payload",
    ),
    (&["projects"], "mallard projects list\n  mallard projects create web --if-not-exists"),
    (&["projects", "list"], "mallard projects list --fields slug,dsn"),
    (&["projects", "create"], "mallard projects create web --platform node --if-not-exists"),
    (&["projects", "show"], "mallard projects show web"),
    (&["releases"], "mallard releases list --since 30d"),
    (&["releases", "list"], "mallard releases list --since 30d -n 10 --fields version,new_issues,last_seen"),
    (&["traces"], "mallard traces list -S checkout --errors --since 15m\n  mallard traces show <trace-id>"),
    (
        &["traces", "list"],
        "mallard traces list -S checkout --errors --since 15m -n 10\n  mallard traces list -O \"POST /pay\" --min-ms 500 -a http.route=/pay\n  mallard traces list -S checkout --since 1m --wait 30s",
    ),
    (&["traces", "show"], "mallard traces show 0af7651916cd43dd8448eb211c80319c --fields spans,errors"),
    (
        &["trace"],
        "mallard trace 0af7651916cd43dd8448eb211c80319c\n  mallard trace 0af7651916cd43dd8448eb211c80319c --since 1d --json\n  mallard trace 0af7651916cd43dd8448eb211c80319c --wait 30s",
    ),
    (
        &["logs"],
        "mallard logs -S checkout -l warn --since 10m --format ndjson --fields time,severity_text,body,trace_id\n  mallard logs --around 2026-11-01T10:15:03Z --window 2m -S checkout\n  mallard logs -t 0af7651916cd43dd8448eb211c80319c --wait 30s\n  mallard logs -S checkout -f --for 2m\n  mallard logs -S checkout -f --until-match \"listening on\" --for 1m   # exit 8 if no match",
    ),
    (&["services"], "mallard services --since 1h"),
    (
        &["ops"],
        "mallard ops -S checkout --since 1h\n  mallard ops -S checkout --since 1h --compare 1d   # p95 and error rate vs. the same hour yesterday",
    ),
    (
        &["series"],
        "mallard series spans -S checkout -O \"POST /pay\" --since 6h --step 5m\n  mallard series logs -S checkout --since 1h\n  mallard series errors -p web --since 1d --step 1h",
    ),
    (&["series", "spans"], "mallard series spans -S checkout -O \"POST /pay\" --since 6h --step 5m --json"),
    (&["series", "logs"], "mallard series logs -S checkout -q timeout --since 1h --step 1m"),
    (&["series", "errors"], "mallard series errors -p web --since 1d --step 1h"),
    (&["metrics"], "mallard metrics list -q http\n  mallard metrics show http.server.duration -g p95 -b http.route"),
    (&["metrics", "list"], "mallard metrics list -q http -S checkout"),
    (
        &["metrics", "show"],
        "mallard metrics show http.server.request.duration -g p95 -b http.route --since 6h --step 5m",
    ),
    (
        &["sql"],
        "mallard sql \"SELECT service_name, count(*) FROM spans GROUP BY 1\"   # exit 7 when SQL is off\n  mallard sql --errors \"SELECT release, count(*) FROM events GROUP BY 1\"",
    ),
    (&["sourcemaps"], "mallard sourcemaps list"),
    (&["sourcemaps", "list"], "mallard sourcemaps list -p web"),
    (&["send-test"], "mallard send-test --dsn \"$SENTRY_DSN\""),
    (&["tui"], "mallard tui -p web"),
    (&["help"], "mallard help agents\n  mallard help issues list"),
];

/// The clap command with the examples added.
pub(crate) fn command() -> clap::Command {
    let mut c = Cli::command().after_help(ROOT);
    for (path, text) in EXAMPLES {
        c = with_examples(c, path, text);
    }
    c
}

fn with_examples(c: clap::Command, path: &[&str], text: &str) -> clap::Command {
    match path.split_first() {
        None => c.after_help(format!("Examples:\n  {text}")),
        Some((first, rest)) => c.mut_subcommand(*first, |s| with_examples(s, rest, text)),
    }
}

/// `mallard help [command…]` and `mallard help agents`.
pub(crate) fn run(topic: &[String], output: &Output) -> Result<()> {
    if topic == ["agents"] {
        output.out.line(AGENTS_GUIDE)?;
        return Ok(());
    }
    let args = std::iter::once("mallard".to_string()).chain(topic.iter().cloned()).chain(["--help".to_string()]);
    match command().try_get_matches_from(args).and_then(|m| Cli::from_arg_matches(&m)) {
        Err(e) if !e.use_stderr() => {
            output.out.line(&e.render().to_string())?;
            Ok(())
        }
        Err(e) => Err(mallard_term::error::usage(format!("unknown help topic {:?}: {}", topic.join(" "), e.kind()))),
        Ok(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every visible command has examples in its --help.
    #[test]
    fn every_command_has_examples() {
        fn walk(c: &clap::Command, path: &str, missing: &mut Vec<String>) {
            for s in c.get_subcommands().filter(|s| !s.is_hide_set()) {
                let p = format!("{path} {}", s.get_name());
                if s.get_after_help().is_none() {
                    missing.push(p.clone());
                }
                walk(s, &p, missing);
            }
        }
        let c = command();
        assert!(c.get_after_help().is_some());
        let mut missing = vec![];
        walk(&c, "mallard", &mut missing);
        assert!(missing.is_empty(), "no examples: {missing:?}");
    }

    #[test]
    fn the_agent_guide_is_the_doc() {
        let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/agents.md")).unwrap();
        assert_eq!(AGENTS_GUIDE, doc);
        for must in ["--format", "exit code", "mallard setup", "mallard find", "--fields", "--around"] {
            assert!(AGENTS_GUIDE.to_lowercase().contains(&must.to_lowercase()), "guide mentions {must}");
        }
    }

    #[test]
    fn unknown_topics_are_usage_errors() {
        let o = Output::default();
        let res = run(&["nope".to_string()], &o);
        assert_eq!(mallard_term::error::exit_code(&res), 2);
    }
}
