//! Static bearer-token authentication.

use std::sync::Arc;

/// A set of accepted bearer tokens. Empty = authentication disabled.
#[derive(Clone, Default)]
pub struct Tokens(Arc<Vec<Vec<u8>>>);

impl Tokens {
    pub fn new(tokens: &[String]) -> Self {
        Self(Arc::new(tokens.iter().filter(|t| !t.is_empty()).map(|t| t.as_bytes().to_vec()).collect()))
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
        let token = token.trim().as_bytes();
        // Compare against every token without early exit to avoid timing leaks.
        self.0.iter().fold(false, |acc, t| acc | constant_time_eq(t, token))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        assert!(Tokens::new(&[]).allows(None));
        let t = Tokens::new(&["abc".into(), "def".into()]);
        assert!(t.allows(Some("Bearer def")));
        assert!(t.allows(Some("BEARER def")));
        assert!(!t.allows(Some("Bearer xyz")));
        assert!(!t.allows(Some("def")));
        assert!(!t.allows(None));
    }
}
