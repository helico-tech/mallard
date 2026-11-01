//! Extracting the public key from a Sentry request.
//!
//! SDKs authenticate with one of:
//! - `X-Sentry-Auth: Sentry sentry_key=..., sentry_version=7, ...`
//! - `Authorization: Sentry sentry_key=...` (older SDKs)
//! - query string `?sentry_key=...&sentry_version=7` (browser SDKs, CSP)
//! - the `dsn` field in the envelope header (tunnels)

/// Parse the key=value pairs of a `Sentry ...` auth header.
pub fn parse_auth_header(value: &str) -> Option<String> {
    let rest = value.trim();
    let rest = rest.strip_prefix("Sentry ").or_else(|| rest.strip_prefix("sentry ")).unwrap_or(rest);
    rest.split(',').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k.trim() == "sentry_key").then(|| v.trim().trim_matches('"').to_string())
    })
}

/// Find `sentry_key` in a raw query string.
pub fn parse_query(query: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes()).find(|(k, _)| k == "sentry_key").map(|(_, v)| v.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header() {
        let h = "Sentry sentry_version=7, sentry_client=sentry.python/2.0, sentry_key=abc, sentry_secret=x";
        assert_eq!(parse_auth_header(h).as_deref(), Some("abc"));
        assert_eq!(parse_auth_header("Sentry sentry_key=k1,sentry_version=7").as_deref(), Some("k1"));
        assert_eq!(parse_auth_header("Bearer x"), None);
    }

    #[test]
    fn query() {
        assert_eq!(parse_query("sentry_version=7&sentry_key=abc").as_deref(), Some("abc"));
        assert_eq!(parse_query("x=1"), None);
    }
}
