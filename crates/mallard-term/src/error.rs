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
    /// What runs the commands, e.g. `mallard`, `mallard errors`.
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
    /// The server answered with a non-success status. `code` is the API's
    /// error code (`not_found`, `disabled`, …) or empty.
    Status {
        status: u16,
        code: &'a str,
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

/// What kind of failure, for scripts and agents: each kind has its own
/// exit code (ADR 0008).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Anything else, e.g. a server error (500) or an unreadable answer.
    Internal,
    /// Bad flag, id, window or field; HTTP 400.
    Usage,
    /// Missing or wrong token (401, 403).
    Auth,
    /// No such project, issue, event or trace (404); `find` without a match.
    NotFound,
    /// Already exists (409).
    Conflict,
    /// Server down, timeout, busy (429, 502–504): try again later.
    Unavailable,
    /// Turned off on the server, e.g. SQL (403 `disabled`, 501).
    Disabled,
    /// A condition the command checks did not hold.
    Unmet,
}

impl Kind {
    pub const ALL: [Kind; 8] = [
        Kind::Internal,
        Kind::Usage,
        Kind::Auth,
        Kind::NotFound,
        Kind::Conflict,
        Kind::Unavailable,
        Kind::Disabled,
        Kind::Unmet,
    ];

    /// The process exit code.
    pub fn exit_code(self) -> u8 {
        match self {
            Kind::Internal => 1,
            Kind::Usage => 2,
            Kind::Auth => 3,
            Kind::NotFound => 4,
            Kind::Conflict => 5,
            Kind::Unavailable => 6,
            Kind::Disabled => 7,
            Kind::Unmet => 8,
        }
    }

    /// The `code` in JSON errors.
    pub fn code(self) -> &'static str {
        match self {
            Kind::Internal => "internal",
            Kind::Usage => "usage",
            Kind::Auth => "unauthorized",
            Kind::NotFound => "not_found",
            Kind::Conflict => "conflict",
            Kind::Unavailable => "unavailable",
            Kind::Disabled => "disabled",
            Kind::Unmet => "condition_not_met",
        }
    }

    /// Whether the same call may succeed later without changes.
    pub fn retryable(self) -> bool {
        self == Kind::Unavailable
    }

    /// The kind of an HTTP error answer.
    pub fn of_status(status: u16, code: &str) -> Kind {
        match status {
            401 => Kind::Auth,
            403 if code == "disabled" => Kind::Disabled,
            403 => Kind::Auth,
            404 => Kind::NotFound,
            409 => Kind::Conflict,
            501 => Kind::Disabled,
            429 | 502..=504 => Kind::Unavailable,
            400..=499 => Kind::Usage,
            _ => Kind::Internal,
        }
    }
}

/// A message written for the user: what went wrong, what to try, and the
/// [`Kind`]. Attach it with
/// `anyhow::Error::new(client_error).context(described)`: the typed error
/// stays downcastable, and [`report`] prints this message alone instead of
/// the whole chain (which would repeat the cause). Displays as
/// `message (hint)`. All text in it is cleaned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Described {
    pub message: String,
    pub hint: Option<String>,
    pub kind: Kind,
    /// HTTP status, when the server answered.
    pub status: Option<u16>,
}

impl Described {
    pub fn new(kind: Kind, message: impl AsRef<str>) -> Self {
        Described { message: clean(message.as_ref()).into_owned(), hint: None, kind, status: None }
    }

    pub fn hint(mut self, hint: impl AsRef<str>) -> Self {
        self.hint = Some(clean(hint.as_ref()).into_owned());
        self
    }

    pub fn status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// The inside of the `{"error": …}` line printed on stderr in JSON mode;
    /// also used for per-item errors in JSON output.
    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            code: self.kind.code(),
            message: self.message.clone(),
            status: self.status,
            retryable: self.kind.retryable(),
            hint: self.hint.clone(),
        }
    }
}

/// An error in JSON output: `{"code","message","status","retryable","hint"}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
    pub status: Option<u16>,
    pub retryable: bool,
    pub hint: Option<String>,
}

impl fmt::Display for Described {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.hint {
            Some(h) => write!(f, "{} ({h})", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for Described {}

/// A usage error (exit code 2): a bad flag, id or value.
pub fn usage(message: impl AsRef<str>) -> anyhow::Error {
    anyhow::Error::new(Described::new(Kind::Usage, message))
}

/// A not-found error (exit code 4).
pub fn not_found(message: impl AsRef<str>) -> anyhow::Error {
    anyhow::Error::new(Described::new(Kind::NotFound, message))
}

/// What went wrong, what to try and the [`Kind`] of a failed request.
pub fn classify(f: Failure<'_>, url: &str, h: &Hints) -> Described {
    let d = |kind, m: String| Described::new(kind, m);
    match f {
        Failure::Status { status, code, message } => {
            let kind = Kind::of_status(status, code);
            let msg = match (status, kind) {
                (401, _) => d(kind, "invalid token".into()).hint(format!("set {} or --token", h.token_env)),
                (_, Kind::Disabled) if message.is_empty() => d(kind, "turned off on the server".into()),
                (_, Kind::Disabled) => d(kind, format!("turned off on the server: {message}")),
                (403, _) => d(kind, "token not allowed to do this".into()).hint(format!("check {}", h.token_env)),
                // Both servers already say what was not found ("issue not
                // found", "trace … not found in the time window").
                (404, _) if message.is_empty() => d(kind, "not found".into()),
                (404, _) => d(kind, message.to_string()),
                (_, Kind::Unavailable) => d(kind, format!("server busy ({status}): {message}")).hint("try again"),
                (500.., _) => d(kind, format!("server error ({status}): {message}")),
                _ => d(kind, format!("request rejected ({status}): {message}")),
            };
            msg.status(status)
        }
        Failure::InvalidUrl(m) => {
            d(Kind::Usage, format!("invalid server url {url:?}: {m}")).hint(format!("set {} or --url", h.url_env))
        }
        Failure::Connect => d(Kind::Unavailable, format!("cannot reach server at {url}"))
            .hint(format!("is it running? set {}", h.url_env)),
        Failure::Timeout => d(Kind::Unavailable, format!("request to {url} timed out")),
        Failure::Decode(e) => d(Kind::Internal, format!("unexpected response from {url} (version mismatch?): {e}")),
        Failure::Other(e) => d(Kind::Internal, format!("request to {url} failed: {e}")),
    }
}

/// One line that says what went wrong and what to try. Server text in it is
/// cleaned, so it is safe to print.
pub fn describe(f: Failure<'_>, url: &str, h: &Hints) -> String {
    classify(f, url, h).to_string()
}

/// Whether the error is a closed stdout (`… | head`): not a failure.
pub fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe))
}

/// The [`Described`] message of an error, or an `Internal` one made of the
/// whole chain (`a: b: c`). Cleaned.
pub fn described(e: &anyhow::Error) -> Described {
    if let Some(d) = e.downcast_ref::<Described>().or_else(|| e.chain().find_map(|c| c.downcast_ref::<Described>())) {
        return d.clone();
    }
    Described::new(Kind::Internal, format!("{e:#}"))
}

/// The one line [`report`] prints after `error: `. Cleaned.
pub fn message(e: &anyhow::Error) -> String {
    described(e).to_string()
}

/// The exit code of a run's result: 0 for success and a closed pipe, else
/// the [`Kind`]'s code.
pub fn exit_code(result: &anyhow::Result<()>) -> u8 {
    match result {
        Ok(()) => 0,
        Err(e) if is_broken_pipe(e) => 0,
        Err(e) => described(e).kind.exit_code(),
    }
}

/// End a CLI run: success, a closed pipe (also success) or `error: …` on
/// stderr (the label red when stderr gets colour) and the [`Kind`]'s exit
/// code.
pub fn report(result: anyhow::Result<()>, color: ColorChoice) -> ExitCode {
    report_as(result, color, false)
}

/// [`report`], with `json`: the error is one JSON line on stderr
/// (`{"error":{"code","message","status","retryable","hint"}}`).
pub fn report_as(result: anyhow::Result<()>, color: ColorChoice, json: bool) -> ExitCode {
    let code = exit_code(&result);
    if let Err(e) = &result
        && code != 0
    {
        eprintln!("{}", error_line(&described(e), color, json));
    }
    ExitCode::from(code)
}

/// The stderr line for an error: JSON, or `error: message (hint)`.
pub fn error_line(d: &Described, color: ColorChoice, json: bool) -> String {
    if json {
        #[derive(serde::Serialize)]
        struct Line {
            error: ErrorBody,
        }
        let line = serde_json::to_string(&Line { error: d.body() }).unwrap_or_default();
        return crate::text::json_safe(&line).into_owned();
    }
    let label = if color.enabled(std::io::stderr().is_terminal()) { alert("error:") } else { "error:".into() };
    format!("{label} {d}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: Hints = Hints { command: "mallard errors", url_env: "U", token_env: "T", project_env: "P" };

    #[test]
    fn describe_every_kind() {
        let d = |f| describe(f, "http://x", &H);
        let st = |status, message| Failure::Status { status, code: "", message };
        assert_eq!(d(st(401, "")), "invalid token (set T or --token)");
        assert_eq!(d(st(403, "")), "token not allowed to do this (check T)");
        assert_eq!(d(st(404, "")), "not found");
        assert_eq!(d(st(404, "issue not found")), "issue not found", "said once");
        assert_eq!(d(st(400, "bad since")), "request rejected (400): bad since");
        assert_eq!(d(st(500, "down\x1b[2J")), "server error (500): down\\u{1b}[2J");
        assert_eq!(d(st(503, "busy")), "server busy (503): busy (try again)");
        let off = Failure::Status { status: 403, code: "disabled", message: "SQL is off" };
        assert_eq!(d(off), "turned off on the server: SQL is off");
        assert_eq!(d(Failure::InvalidUrl("no scheme")), "invalid server url \"http://x\": no scheme (set U or --url)");
        assert_eq!(d(Failure::Connect), "cannot reach server at http://x (is it running? set U)");
        assert_eq!(d(Failure::Timeout), "request to http://x timed out");
        assert_eq!(d(Failure::Other("boom".into())), "request to http://x failed: boom");
    }

    #[test]
    fn described_errors_print_alone_others_print_their_chain() {
        let io = std::io::Error::other("disk on fire");
        let e = anyhow::Error::new(io).context(Described::new(Kind::Usage, "cannot save"));
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

    #[test]
    fn every_kind_has_its_own_exit_code_and_code() {
        let codes: std::collections::HashSet<u8> = Kind::ALL.iter().map(|k| k.exit_code()).collect();
        assert_eq!(codes, (1..=8).collect());
        let names: std::collections::HashSet<&str> = Kind::ALL.iter().map(|k| k.code()).collect();
        assert_eq!(names.len(), Kind::ALL.len());
        assert_eq!(Kind::ALL.iter().filter(|k| k.retryable()).collect::<Vec<_>>(), [&Kind::Unavailable]);
    }

    #[test]
    fn http_statuses_map_to_kinds() {
        let k = Kind::of_status;
        assert_eq!(k(400, "bad_request"), Kind::Usage);
        assert_eq!(k(401, "unauthorized"), Kind::Auth);
        assert_eq!(k(403, ""), Kind::Auth);
        assert_eq!(k(403, "disabled"), Kind::Disabled);
        assert_eq!(k(404, "not_found"), Kind::NotFound);
        assert_eq!(k(409, "conflict"), Kind::Conflict);
        assert_eq!(k(429, ""), Kind::Unavailable);
        assert_eq!(k(500, "internal"), Kind::Internal);
        assert_eq!(k(501, ""), Kind::Disabled);
        assert_eq!(k(503, "busy"), Kind::Unavailable);
        assert_eq!(k(504, "timeout"), Kind::Unavailable);
        assert_eq!(classify(Failure::Connect, "http://x", &H).kind, Kind::Unavailable);
        assert_eq!(classify(Failure::InvalidUrl("x"), "x", &H).kind, Kind::Usage);
        assert_eq!(classify(Failure::Decode("x".into()), "x", &H).kind, Kind::Internal);
    }

    #[test]
    fn exit_codes_come_from_the_kind_anywhere_in_the_chain() {
        assert_eq!(exit_code(&Ok(())), 0);
        assert_eq!(exit_code(&Err(usage("bad"))), 2);
        assert_eq!(exit_code(&Err(not_found("gone").context("while looking"))), 4);
        let e = anyhow::Error::new(std::io::Error::other("x")).context(Described::new(Kind::Auth, "no"));
        assert_eq!(exit_code(&Err(e)), 3);
        assert_eq!(exit_code(&Err(anyhow::anyhow!("plain"))), 1);
    }

    #[test]
    fn json_error_line() {
        let d = Described::new(Kind::Unavailable, "down\x1b").hint("start it").status(503);
        let line = error_line(&d, ColorChoice::Never, true);
        assert!(!line.contains('\n'));
        assert_eq!(
            line,
            r#"{"error":{"code":"unavailable","message":"down\\u{1b}","status":503,"retryable":true,"hint":"start it"}}"#
        );
        assert_eq!(error_line(&d, ColorChoice::Never, false), "error: down\\u{1b} (start it)");
    }
}
