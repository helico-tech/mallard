//! Static bearer-token authentication.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use subtle::{Choice, ConstantTimeEq};

/// What an empty token list means. Set it explicitly for every API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthPolicy {
    /// Every request needs a valid token. With no tokens, all are refused.
    Required,
    /// With no tokens configured, every request is allowed (e.g. open OTLP ingest).
    OpenIfEmpty,
}

/// Accepted bearer tokens, kept as SHA-256 digests, plus the [`AuthPolicy`].
#[derive(Clone)]
pub struct Tokens {
    digests: Arc<Vec<[u8; 32]>>,
    policy: AuthPolicy,
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl Tokens {
    /// Empty and whitespace-only tokens are ignored.
    pub fn new(tokens: impl IntoIterator<Item = impl AsRef<str>>, policy: AuthPolicy) -> Self {
        let digests = tokens.into_iter().map(|t| t.as_ref().trim().to_string()).filter(|t| !t.is_empty());
        Self { digests: Arc::new(digests.map(|t| digest(&t)).collect()), policy }
    }

    /// No token is configured.
    pub fn is_empty(&self) -> bool {
        self.digests.is_empty()
    }

    /// Check an `Authorization` header value (`Bearer <token>`; the scheme
    /// is case-insensitive, RFC 7235).
    pub fn allows(&self, header: Option<&str>) -> bool {
        if self.is_empty() {
            return self.policy == AuthPolicy::OpenIfEmpty;
        }
        let Some((scheme, token)) = header.and_then(|h| h.trim_start().split_once(' ')) else {
            return false;
        };
        if !scheme.eq_ignore_ascii_case("bearer") {
            return false;
        }
        // Fixed-length digests compared in constant time against every
        // token: neither the token nor its length leaks through timing.
        let presented = digest(token.trim());
        self.digests.iter().fold(Choice::from(0), |acc, t| acc | t.ct_eq(&presented)).into()
    }
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens").field("count", &self.digests.len()).field("policy", &self.policy).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_lists_follow_the_policy() {
        let none: [&str; 0] = [];
        assert!(!Tokens::new(none, AuthPolicy::Required).allows(None));
        assert!(!Tokens::new(["", " "], AuthPolicy::Required).allows(Some("Bearer ")));
        assert!(Tokens::new(none, AuthPolicy::OpenIfEmpty).allows(None));
        assert!(Tokens::new([""], AuthPolicy::OpenIfEmpty).allows(None), "empty tokens are ignored");
    }

    #[test]
    fn checks_scheme_and_token() {
        for policy in [AuthPolicy::Required, AuthPolicy::OpenIfEmpty] {
            let t = Tokens::new(["abc", "def"], policy);
            assert!(t.allows(Some("Bearer def")));
            assert!(t.allows(Some("bearer def")));
            assert!(t.allows(Some("BEARER  def ")));
            assert!(!t.allows(Some("Bearer xyz")));
            assert!(!t.allows(Some("Basic def")));
            assert!(!t.allows(Some("def")));
            assert!(!t.allows(Some("Bearer de")));
            assert!(!t.allows(Some("Bearer defg")));
            assert!(!t.allows(None));
        }
    }

    #[test]
    fn debug_hides_tokens() {
        assert!(!format!("{:?}", Tokens::new(["hunter2"], AuthPolicy::Required)).contains("hunter2"));
    }
}
