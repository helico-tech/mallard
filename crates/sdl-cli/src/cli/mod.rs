//! Non-interactive commands: the context a host passes in ([`Ctx`]),
//! error messages ([`describe`]) and the dispatch ([`run`]). The commands
//! themselves are in `commands.rs`.

use crate::fmt;
use crate::paint as p;
use crate::{Command, SourcemapsCmd};
use anyhow::{Result, bail};
use sdl_client::Client;
use serde::Serialize;

/// `println!` to a locked stdout that returns the error instead of
/// panicking; `run` treats a closed pipe (`sdl issues list | head`) as success.
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        writeln!(std::io::stdout().lock(), $($arg)*)?
    }};
}

mod commands;

/// How the host names the commands and its env vars, for the hints in
/// messages ("create one with: sdl projects create <name>").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hints {
    /// What runs these commands, e.g. `sdl` or `mallard errors`.
    pub command: &'static str,
    pub url_env: &'static str,
    pub token_env: &'static str,
    pub project_env: &'static str,
}

impl Hints {
    /// The standalone `sdl` binary.
    pub const SDL: Hints =
        Hints { command: "sdl", url_env: "SDL_URL", token_env: "SDL_TOKEN", project_env: "SDL_PROJECT" };
}

impl Default for Hints {
    fn default() -> Self {
        Hints::SDL
    }
}

/// One-line, actionable message for a client error. Safe to print: server
/// text in it is stripped of control characters.
pub fn describe(e: &sdl_client::Error, url: &str, hints: &Hints) -> String {
    fmt::clean(&describe_raw(e, url, hints))
}

fn describe_raw(e: &sdl_client::Error, url: &str, hints: &Hints) -> String {
    match e {
        sdl_client::Error::Api { status: 401, .. } => format!("invalid token (set {} or --token)", hints.token_env),
        sdl_client::Error::Api { status: 403, .. } => {
            format!("token not allowed to do this (check {})", hints.token_env)
        }
        sdl_client::Error::Api { status: 404, message } => {
            if message.is_empty() {
                "not found".into()
            } else {
                format!("not found: {message}")
            }
        }
        sdl_client::Error::Api { status, message } if *status >= 500 => {
            format!("server error ({status}): {message}")
        }
        sdl_client::Error::Api { status, message } => {
            format!("request rejected ({status}): {message}")
        }
        sdl_client::Error::InvalidSegment(_) => format!("{e}: ids and slugs can't be empty, '.' or '..'"),
        sdl_client::Error::Url(m) => {
            format!("invalid server url {url:?}: {m} (set {} or --url)", hints.url_env)
        }
        sdl_client::Error::Http(h) if h.is_connect() => {
            format!("cannot reach server at {url} (is it running? set {})", hints.url_env)
        }
        sdl_client::Error::Http(h) if h.is_timeout() => format!("request to {url} timed out"),
        sdl_client::Error::Http(h) if h.is_decode() => {
            format!("unexpected response from {url} (version mismatch?): {h}")
        }
        sdl_client::Error::Http(h) => format!("request to {url} failed: {h}"),
        e => format!("request to {url} failed: {e}"),
    }
}

/// What the commands need besides their own arguments. A host that mounts
/// [`Command`] builds this from its own global flags:
/// `Ctx::new(client).with_project(p).with_json(true)`.
pub struct Ctx {
    pub(crate) client: Client,
    /// Project id or slug; optional when the server has only one project.
    pub(crate) project: Option<String>,
    /// Print raw JSON instead of tables.
    pub(crate) json: bool,
    pub(crate) hints: Hints,
}

impl Ctx {
    /// No project, table output, [`Hints::SDL`].
    pub fn new(client: Client) -> Self {
        Ctx { client, project: None, json: false, hints: Hints::SDL }
    }

    /// Project id or slug; optional when the server has only one project.
    pub fn with_project(mut self, project: Option<String>) -> Self {
        self.project = project;
        self
    }

    /// Print raw JSON instead of tables.
    pub fn with_json(mut self, json: bool) -> Self {
        self.json = json;
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
        let msg = describe(&e, self.url(), &self.hints);
        let e = match e {
            sdl_client::Error::Api { status, message } => {
                sdl_client::Error::Api { status, message: fmt::clean(&message) }
            }
            e => e,
        };
        anyhow::Error::new(e).context(msg)
    }

    fn url(&self) -> &str {
        self.client.base_url()
    }

    /// The `--project`, or the only project on the server.
    pub(crate) async fn resolve_project(&self) -> Result<String> {
        if let Some(p) = &self.project {
            return Ok(p.clone());
        }
        let projects = self.client.projects().await.map_err(|e| self.err(e))?;
        let h = &self.hints;
        match projects.as_slice() {
            [one] => Ok(one.slug.clone()),
            [] => bail!("no projects yet (create one: {} projects create <name>)", h.command),
            many => {
                let slugs: Vec<String> = many.iter().map(|p| fmt::clean(&p.slug)).collect();
                bail!("several projects; pick one with -p or {} ({})", h.project_env, slugs.join(", "))
            }
        }
    }

    fn print_json<T: Serialize>(&self, v: &T) -> Result<()> {
        out!("{}", json_safe(&serde_json::to_string_pretty(v)?));
        Ok(())
    }
}

/// Run one command. Prints to stdout; errors are one-line, friendly messages.
/// All text that comes from events or the server is stripped of terminal
/// control characters before it is printed (except in `--json` output).
pub async fn run(cmd: Command, ctx: &Ctx) -> Result<()> {
    p::init();
    match run_inner(cmd, ctx).await {
        Err(e) if is_broken_pipe(&e) => Ok(()),
        r => r,
    }
}

/// serde escapes C0 control characters in JSON strings but writes DEL, C1
/// (e.g. U+009B, an 8-bit CSI) and bidi overrides as they are. Escape those
/// too; `\uXXXX` is the same JSON value, so the output stays lossless.
fn json_safe(json: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        // Raw newlines are the pretty-printer's own; inside strings serde escapes them.
        if (c.is_control() && c != '\n') || fmt::is_bidi_control(c) {
            let _ = write!(out, "\\u{:04x}", c as u32);
        } else {
            out.push(c);
        }
    }
    out
}

fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe))
}

async fn run_inner(cmd: Command, ctx: &Ctx) -> Result<()> {
    match cmd {
        Command::SendTest { dsn, message } => commands::send_test(&dsn, &message, ctx).await,
        Command::Info => commands::info(ctx).await,
        Command::Projects(c) => commands::projects(ctx, c).await,
        Command::Issues(c) => commands::issues(ctx, c).await,
        Command::Events(c) => commands::events(ctx, c).await,
        Command::Stats { since, interval } => commands::stats(ctx, since, interval).await,
        Command::Sourcemaps(SourcemapsCmd::List) => commands::sourcemaps(ctx).await,
        Command::Maintenance => {
            ctx.client.maintenance().await.map_err(|e| ctx.err(e))?;
            if ctx.json {
                out!("{{\"ok\":true}}");
            } else {
                out!("{} maintenance done (flushed, compacted, expired)", p::green("✓"));
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
        let e = sdl_client::Error::Api { status: 401, message: String::new() };
        assert_eq!(describe(&e, "http://x", &Hints::SDL), "invalid token (set SDL_TOKEN or --token)");
        assert_eq!(describe(&e, "http://x", &MALLARD), "invalid token (set MALLARD_TOKEN or --token)");
        let e = sdl_client::Client::new("nope", "").unwrap_err();
        assert!(describe(&e, "nope", &MALLARD).contains("set MALLARD_URL or --url"));
    }

    #[test]
    fn describe_maps_every_kind_of_error() {
        let api = |status: u16, m: &str| sdl_client::Error::Api { status, message: m.into() };
        let d = |e: sdl_client::Error| describe(&e, "http://x", &Hints::SDL);
        assert_eq!(d(api(403, "")), "token not allowed to do this (check SDL_TOKEN)");
        assert_eq!(d(api(404, "")), "not found");
        assert_eq!(d(api(404, "no issue")), "not found: no issue");
        assert_eq!(d(api(400, "bad since")), "request rejected (400): bad since");
        assert_eq!(d(api(503, "down\x1b[2J")), "server error (503): down\u{FFFD}[2J");
        let e = block_on(sdl_client::Client::new("http://x", "").unwrap().project("..")).unwrap_err();
        assert_eq!(d(e), "invalid path segment \"..\": ids and slugs can't be empty, '.' or '..'");
    }

    #[test]
    fn err_keeps_the_typed_source_and_cleans_it() {
        let ctx = Ctx::new(sdl_client::Client::new("http://x", "").unwrap()).with_hints(MALLARD);
        let e = ctx.err(sdl_client::Error::Api { status: 404, message: "gone\x1b]0;x\x07".into() });
        assert_eq!(e.to_string(), "not found: gone\u{FFFD}]0;x\u{FFFD}");
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
        let out = json_safe(&serde_json::to_string_pretty(&v).unwrap());
        assert!(!out.chars().any(|c| c.is_control() && c != '\n'), "{out:?}");
        assert!(out.contains("\\u009b") && out.contains("\\u202e"));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&out).unwrap(), v);
    }
}
