//! DSN parsing: `https://<public_key>@<host>[:port][/path]/<project_id>`.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dsn {
    pub scheme: String,
    pub public_key: String,
    pub host: String,
    pub port: Option<u16>,
    /// Path prefix before the project id, without leading/trailing slash.
    pub path: String,
    pub project_id: u32,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DsnError {
    #[error("invalid DSN url")]
    InvalidUrl,
    #[error("DSN has no public key")]
    MissingKey,
    #[error("DSN has no numeric project id")]
    MissingProject,
}

impl Dsn {
    pub fn parse(s: &str) -> Result<Self, DsnError> {
        let u = url::Url::parse(s).map_err(|_| DsnError::InvalidUrl)?;
        let public_key = u.username().to_string();
        if public_key.is_empty() {
            return Err(DsnError::MissingKey);
        }
        let host = u.host_str().ok_or(DsnError::InvalidUrl)?.to_string();
        let segs: Vec<&str> = u.path().trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
        let (last, prefix) = segs.split_last().ok_or(DsnError::MissingProject)?;
        let project_id = last.parse().map_err(|_| DsnError::MissingProject)?;
        Ok(Dsn { scheme: u.scheme().to_string(), public_key, host, port: u.port(), path: prefix.join("/"), project_id })
    }

    /// Build a DSN string for a project on a public base url such as
    /// `https://errors.example.com` or `http://localhost:8000/prefix`.
    /// Fails when the url cannot carry a key (no host, e.g. `data:x`).
    pub fn build(base_url: &str, public_key: &str, project_id: u32) -> Result<String, DsnError> {
        let mut u = url::Url::parse(base_url).map_err(|_| DsnError::InvalidUrl)?;
        u.set_username(public_key).map_err(|()| DsnError::InvalidUrl)?;
        let path = u.path().trim_end_matches('/').to_string();
        u.set_path(&format!("{path}/{project_id}"));
        Ok(u.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_dsn() {
        let d = Dsn::parse("https://abc123@o1.ingest.example.com/42").unwrap();
        assert_eq!(d.public_key, "abc123");
        assert_eq!(d.host, "o1.ingest.example.com");
        assert_eq!(d.project_id, 42);
        assert_eq!(d.path, "");
    }

    #[test]
    fn parses_dsn_with_port_path_and_secret() {
        let d = Dsn::parse("http://key:secret@localhost:8000/sub/path/7").unwrap();
        assert_eq!(d.port, Some(8000));
        assert_eq!(d.path, "sub/path");
        assert_eq!(d.project_id, 7);
    }

    #[test]
    fn rejects_bad_dsns() {
        assert_eq!(Dsn::parse("https://host/1"), Err(DsnError::MissingKey));
        assert_eq!(Dsn::parse("https://k@host/abc"), Err(DsnError::MissingProject));
        assert_eq!(Dsn::parse("nope"), Err(DsnError::InvalidUrl));
    }

    #[test]
    fn builds_dsn() {
        assert_eq!(Dsn::build("http://localhost:8000", "k", 3).unwrap(), "http://k@localhost:8000/3");
        assert_eq!(Dsn::build("https://e.example.com/x/", "k", 3).unwrap(), "https://k@e.example.com/x/3");
        assert_eq!(Dsn::build("data:x", "k", 3), Err(DsnError::InvalidUrl), "a DSN without its key");
        assert_eq!(Dsn::build("nope", "k", 3), Err(DsnError::InvalidUrl));
    }
}
