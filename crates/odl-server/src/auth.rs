//! Static bearer-token authentication.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use subtle::{Choice, ConstantTimeEq};

/// A set of accepted bearer tokens, kept as SHA-256 digests. Empty =
/// authentication disabled.
#[derive(Clone, Default)]
pub struct Tokens(Arc<Vec<[u8; 32]>>);

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl Tokens {
    pub fn new(tokens: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        Self(Arc::new(tokens.into_iter().filter(|t| !t.as_ref().is_empty()).map(|t| digest(t.as_ref())).collect()))
    }

    pub fn enabled(&self) -> bool {
        !self.0.is_empty()
    }

    /// Check an `Authorization` header value (`Bearer <token>`).
    pub fn allows(&self, header: Option<&str>) -> bool {
        if !self.enabled() {
            return true;
        }
        let Some(h) = header else { return false };
        // The scheme is case-insensitive (RFC 7235).
        let Some((scheme, token)) = h.trim_start().split_once(' ') else {
            return false;
        };
        if !scheme.eq_ignore_ascii_case("bearer") {
            return false;
        }
        // Fixed-length digests compared in constant time against every
        // token: neither the token nor its length leaks through timing.
        let presented = digest(token.trim());
        self.0.iter().fold(Choice::from(0), |acc, t| acc | t.ct_eq(&presented)).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        assert!(Tokens::new(Vec::<String>::new()).allows(None));
        assert!(Tokens::new([""]).allows(None), "empty tokens are ignored");
        let t = Tokens::new(["abc", "def"]);
        assert!(t.allows(Some("Bearer def")));
        assert!(t.allows(Some("BEARER def")));
        assert!(!t.allows(Some("Bearer xyz")));
        assert!(!t.allows(Some("def")));
        assert!(!t.allows(Some("Bearer de")));
        assert!(!t.allows(Some("Bearer defg")));
        assert!(!t.allows(None));
    }
}
