//! Error messages: one wording for both clients' failures, hints that name
//! the command and env vars of the program that runs, and one way to print
//! them (`error: …` on stderr, exit code 1).

use std::fmt;
use std::io::IsTerminal;
use std::process::ExitCode;

use crate::color::{ColorChoice, alert};
use crate::text::clean;

/// How the host names its commands and env vars, for the hints in messages
/// ("set MALLARD_TOKEN or --token", "create one: mallard errors projects
/// create <name>").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hints {
    /// What runs the commands, e.g. `sdl`, `odl`, `mallard errors`.
    pub command: &'static str,
    pub url_env: &'static str,
    pub token_env: &'static str,
    /// Only the errors side has projects.
    pub project_env: &'static str,
}

/// A failed request, as far as the message cares. Each CLI maps its
/// client's error to this.
#[derive(Debug)]
pub enum Failure<'a> {
    /// The server answered with a non-success status.
    Status {
        status: u16,
        message: &'a str,
    },
    InvalidUrl(&'a str),
    /// No connection (server down, wrong port, DNS).
    Connect,
    Timeout,
    /// The answer could not be read: probably another server version.
    Decode(String),
    Other(String),
}

/// One line that says what went wrong and what to try. Server text in it is
/// cleaned, so it is safe to print.
pub fn describe(f: Failure<'_>, url: &str, h: &Hints) -> String {
    let msg = match f {
        Failure::Status { status: 401, .. } => format!("invalid token (set {} or --token)", h.token_env),
        Failure::Status { status: 403, .. } => format!("token not allowed to do this (check {})", h.token_env),
        Failure::Status { status: 404, message: "" } => "not found".into(),
        Failure::Status { status: 404, message } => format!("not found: {message}"),
        Failure::Status { status, message } if status >= 500 => format!("server error ({status}): {message}"),
        Failure::Status { status, message } => format!("request rejected ({status}): {message}"),
        Failure::InvalidUrl(m) => format!("invalid server url {url:?}: {m} (set {} or --url)", h.url_env),
        Failure::Connect => format!("cannot reach server at {url} (is it running? set {})", h.url_env),
        Failure::Timeout => format!("request to {url} timed out"),
        Failure::Decode(e) => format!("unexpected response from {url} (version mismatch?): {e}"),
        Failure::Other(e) => format!("request to {url} failed: {e}"),
    };
    clean(&msg).into_owned()
}

/// A message written for the user (by [`describe`]). Attach it with
/// `anyhow::Error::new(client_error).context(Described(msg))`: the typed
/// error stays downcastable, and [`report`] prints this message alone
/// instead of the whole chain (which would repeat the cause).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Described(pub String);

impl fmt::Display for Described {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether the error is a closed stdout (`… | head`): not a failure.
pub fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe))
}

/// The one line [`report`] prints after `error: `: a [`Described`] message
/// alone, else the whole chain (`a: b: c`). Cleaned.
pub fn message(e: &anyhow::Error) -> String {
    let msg = if e.downcast_ref::<Described>().is_some() { e.to_string() } else { format!("{e:#}") };
    clean(&msg).into_owned()
}

/// End a CLI run: success, a closed pipe (also success) or `error: …` on
/// stderr (the label red when stderr gets colour) and exit code 1.
pub fn report(result: anyhow::Result<()>, color: ColorChoice) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if is_broken_pipe(&e) => ExitCode::SUCCESS,
        Err(e) => {
            let label = if color.enabled(std::io::stderr().is_terminal()) { alert("error:") } else { "error:".into() };
            eprintln!("{label} {}", message(&e));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: Hints = Hints { command: "mallard errors", url_env: "U", token_env: "T", project_env: "P" };

    #[test]
    fn describe_every_kind() {
        let d = |f| describe(f, "http://x", &H);
        assert_eq!(d(Failure::Status { status: 401, message: "" }), "invalid token (set T or --token)");
        assert_eq!(d(Failure::Status { status: 403, message: "" }), "token not allowed to do this (check T)");
        assert_eq!(d(Failure::Status { status: 404, message: "" }), "not found");
        assert_eq!(d(Failure::Status { status: 404, message: "no issue" }), "not found: no issue");
        assert_eq!(d(Failure::Status { status: 400, message: "bad since" }), "request rejected (400): bad since");
        assert_eq!(d(Failure::Status { status: 503, message: "down\x1b[2J" }), "server error (503): down\\u{1b}[2J");
        assert_eq!(d(Failure::InvalidUrl("no scheme")), "invalid server url \"http://x\": no scheme (set U or --url)");
        assert_eq!(d(Failure::Connect), "cannot reach server at http://x (is it running? set U)");
        assert_eq!(d(Failure::Timeout), "request to http://x timed out");
        assert_eq!(d(Failure::Other("boom".into())), "request to http://x failed: boom");
    }

    #[test]
    fn described_errors_print_alone_others_print_their_chain() {
        let io = std::io::Error::other("disk on fire");
        let e = anyhow::Error::new(io).context(Described("cannot save".into()));
        assert_eq!(message(&e), "cannot save");
        assert!(e.downcast_ref::<std::io::Error>().is_some(), "typed source kept");
        let e = anyhow::Error::new(std::io::Error::other("disk on fire")).context("loading config");
        assert_eq!(message(&e), "loading config: disk on fire");
        let e = anyhow::anyhow!("bad\x1b]0;x\x07");
        assert_eq!(message(&e), "bad\\u{1b}]0;x\\u{7}");
    }

    #[test]
    fn a_closed_pipe_is_success() {
        let e = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::BrokenPipe)).context("writing");
        assert!(is_broken_pipe(&e));
        assert_eq!(report(Err(e), ColorChoice::Never), ExitCode::SUCCESS);
        assert_eq!(report(Err(anyhow::anyhow!("x")), ColorChoice::Never), ExitCode::FAILURE);
    }
}
