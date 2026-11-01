//! Non-interactive commands: the context a host passes in ([`Ctx`]),
//! error messages ([`describe`]) and the dispatch ([`run`]). The commands
//! themselves are in `commands.rs`.

use crate::fmt;
use crate::paint as p;
use crate::{Command, ReleasesCmd, SourcemapsCmd};
use anyhow::Result;
use mallard_term::color::{ColorChoice, Out};
use mallard_term::error::{Described, Failure, Hints, Kind};
use mallard_term::output::{Format, Output};
use sdl_client::Client;
use serde::Serialize;

/// `println!` through [`Out`] (colour stripped when off) that returns the
/// error instead of panicking; a closed pipe (`sdl issues list | head`) is
/// then reported as success by the binary.
macro_rules! out {
    ($o:expr) => {
        $o.line("")?
    };
    ($o:expr, $($arg:tt)*) => {
        $o.line(&format!($($arg)*))?
    };
}

mod commands;

pub use commands::create_or_get;

/// Hints for the standalone `sdl` binary.
pub const HINTS: Hints =
    Hints { command: "sdl", url_env: "SDL_URL", token_env: "SDL_TOKEN", project_env: "SDL_PROJECT" };

/// One-line, actionable message for a client error. Safe to print: server
/// text in it is cleaned.
pub fn describe(e: &sdl_client::Error, url: &str, hints: &Hints) -> String {
    classify(e, url, hints).to_string()
}

/// [`describe`] with the [`Kind`] (exit code) and the hint apart.
pub fn classify(e: &sdl_client::Error, url: &str, hints: &Hints) -> Described {
    use sdl_client::Error as E;
    let f = match e {
        E::Api { status, code, message } => Failure::Status { status: *status, code, message },
        E::InvalidUrl(m) => Failure::InvalidUrl(m),
        E::InvalidSegment(_) => {
            return Described::new(Kind::Usage, format!("{e}: ids and slugs can't be empty, '.' or '..'"));
        }
        E::Http(h) if h.is_connect() => Failure::Connect,
        E::Http(h) if h.is_timeout() => Failure::Timeout,
        E::Http(h) if h.is_decode() => Failure::Decode(h.to_string()),
        E::Http(h) => Failure::Other(h.to_string()),
        e => Failure::Other(e.to_string()),
    };
    mallard_term::error::classify(f, url, hints)
}

/// What the commands need besides their own arguments. A host that mounts
/// [`Command`] builds this from its own global flags:
/// `Ctx::new(client).with_project(p).with_output(o)`. `odl_cli::Ctx` has
/// the same builder.
#[derive(Debug)]
pub struct Ctx {
    pub(crate) client: Client,
    /// Project id or slug; optional when the server has only one project.
    pub(crate) project: Option<String>,
    /// Format, fields, raw (ADR 0008).
    pub(crate) output: Output,
    pub(crate) out: Out,
    pub(crate) hints: Hints,
}

impl Ctx {
    /// No project, text output, colour as `--color=auto` says, [`HINTS`].
    pub fn new(client: Client) -> Self {
        let output = Output::default();
        Ctx { client, project: None, out: output.out, output, hints: HINTS }
    }

    /// Project id or slug; optional when the server has only one project.
    pub fn with_project(mut self, project: Option<String>) -> Self {
        self.project = project;
        self
    }

    /// Print JSON instead of tables (`--json`).
    pub fn with_json(mut self, json: bool) -> Self {
        if json {
            self.output.format = Format::Json;
        }
        self
    }

    /// Format, colour, `--fields` and `--raw` at once.
    pub fn with_output(mut self, output: Output) -> Self {
        self.out = output.out;
        self.output = output;
        self
    }

    /// The `--color` flag (for stdout).
    pub fn with_color(mut self, color: ColorChoice) -> Self {
        self.out = Out::stdout(color);
        self.output.out = self.out;
        self
    }

    pub fn with_hints(mut self, hints: Hints) -> Self {
        self.hints = hints;
        self
    }

    /// The friendly message, with the client error kept as its source so a
    /// host can still downcast it. The server's message in the source is
    /// cleaned too: hosts may print the whole chain.
    pub(crate) fn err(&self, e: sdl_client::Error) -> anyhow::Error {
        let msg = classify(&e, self.url(), &self.hints);
        let e = match e {
            sdl_client::Error::Api { status, code, message } => {
                sdl_client::Error::Api { status, code, message: fmt::clean(&message).into_owned() }
            }
            e => e,
        };
        anyhow::Error::new(e).context(msg)
    }

    fn url(&self) -> &str {
        self.client.base_url()
    }

    /// The `--project`, or the only project on the server.
    pub async fn resolve_project(&self) -> Result<String> {
        if let Some(p) = &self.project {
            return Ok(p.clone());
        }
        let projects = self.client.projects().await.map_err(|e| self.err(e))?;
        let h = &self.hints;
        match projects.as_slice() {
            [one] => Ok(one.slug.clone()),
            [] => Err(anyhow::Error::new(
                Described::new(Kind::NotFound, "no projects yet")
                    .hint(format!("create one: {} projects create <name>", h.command)),
            )),
            many => {
                let slugs: Vec<String> = many.iter().map(|p| fmt::clean(&p.slug).into_owned()).collect();
                Err(anyhow::Error::new(Described::new(Kind::Usage, "several projects; pick one").hint(format!(
                    "-p or {}: {}",
                    h.project_env,
                    slugs.join(", ")
                ))))
            }
        }
    }

    fn print_json<T: Serialize>(&self, v: &T) -> Result<()> {
        self.output.object(v)
    }
}

/// Run one command. Prints to stdout; errors are one-line, friendly
/// messages. Write errors are returned too: a closed pipe is a `BrokenPipe`
/// in the chain, which `mallard_term::error::report` treats as success. All
/// text that comes from events or the server is cleaned before it is
/// printed (`--json` output is escaped instead).
pub async fn run(cmd: Command, ctx: &Ctx) -> Result<()> {
    run_inner(cmd, ctx).await
}

async fn run_inner(cmd: Command, ctx: &Ctx) -> Result<()> {
    match cmd {
        Command::SendTest { dsn, message } => commands::send_test(&dsn, &message, ctx).await,
        Command::Info => commands::info(ctx).await,
        Command::Projects(c) => commands::projects(ctx, c).await,
        Command::Issues(c) => commands::issues(ctx, c).await,
        Command::Events(c) => commands::events(ctx, c).await,
        Command::Stats { window, interval } => commands::stats(ctx, window, interval).await,
        Command::Releases(ReleasesCmd::List { window, limit }) => commands::releases(ctx, window, limit).await,
        Command::Sql(a) => commands::sql(ctx, a).await,
        Command::Sourcemaps(SourcemapsCmd::List) => commands::sourcemaps(ctx).await,
        Command::Maintenance => {
            ctx.client.maintenance().await.map_err(|e| ctx.err(e))?;
            if !ctx.output.text() {
                ctx.output.object(&serde_json::json!({"ok": true}))?;
            } else {
                out!(ctx.out, "{} maintenance done (flushed, compacted, expired)", p::green("✓"));
            }
            Ok(())
        }
        Command::Tui => crate::tui::run(ctx).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MALLARD: Hints = Hints {
        command: "mallard errors",
        url_env: "MALLARD_URL",
        token_env: "MALLARD_TOKEN",
        project_env: "MALLARD_PROJECT",
    };

    #[test]
    fn hints_name_the_host() {
        let e = sdl_client::Error::Api { status: 401, code: String::new(), message: String::new() };
        assert_eq!(describe(&e, "http://x", &HINTS), "invalid token (set SDL_TOKEN or --token)");
        assert_eq!(describe(&e, "http://x", &MALLARD), "invalid token (set MALLARD_TOKEN or --token)");
        let e = sdl_client::Client::new("nope", None).unwrap_err();
        assert!(describe(&e, "nope", &MALLARD).contains("set MALLARD_URL or --url"));
    }

    #[test]
    fn describe_maps_every_kind_of_error() {
        let api = |status: u16, m: &str| sdl_client::Error::Api { status, code: String::new(), message: m.into() };
        let d = |e: sdl_client::Error| describe(&e, "http://x", &HINTS);
        assert_eq!(d(api(403, "")), "token not allowed to do this (check SDL_TOKEN)");
        assert_eq!(d(api(404, "")), "not found");
        assert_eq!(d(api(404, "issue not found")), "issue not found");
        assert_eq!(d(api(400, "bad since")), "request rejected (400): bad since");
        assert_eq!(d(api(500, "down\x1b[2J")), "server error (500): down\\u{1b}[2J");
        assert_eq!(d(api(503, "busy")), "server busy (503): busy (try again)");
        let e = block_on(sdl_client::Client::new("http://x", None).unwrap().project("..")).unwrap_err();
        assert_eq!(d(e), "invalid path segment \"..\": ids and slugs can't be empty, '.' or '..'");
    }

    #[test]
    fn err_keeps_the_typed_source_and_cleans_it() {
        let ctx = Ctx::new(sdl_client::Client::new("http://x", None).unwrap()).with_hints(MALLARD);
        let e =
            ctx.err(sdl_client::Error::Api { status: 404, code: String::new(), message: "gone\x1b]0;x\x07".into() });
        assert_eq!(e.to_string(), "gone\\u{1b}]0;x\\u{7}");
        let src = e.downcast_ref::<sdl_client::Error>().expect("typed source");
        assert!(src.is_not_found());
        assert!(!format!("{e:#}").chars().any(char::is_control), "{e:#}");
    }

    /// Minimal executor for futures that resolve without I/O.
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let mut f = std::pin::pin!(f);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    #[test]
    fn json_output_escapes_c1_and_bidi_but_stays_equal() {
        let v = serde_json::json!({"t": "a\u{9b}31m\u{202e}b\u{1b}c\nd"});
        let pretty = serde_json::to_string_pretty(&v).unwrap();
        let out = mallard_term::text::json_safe(&pretty);
        assert!(!out.chars().any(|c| c.is_control() && c != '\n'), "{out:?}");
        assert!(out.contains("\\u009b") && out.contains("\\u202e"));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&out).unwrap(), v);
    }
}
