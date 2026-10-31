//! Legacy `/api/<project>/store/` bodies.
//!
//! Modern SDKs send plain (optionally gzip'd via Content-Encoding) JSON.
//! Very old SDKs (raven) send base64 of zlib-compressed JSON.

use base64::Engine;
use std::io::Read;

/// Decode a store body to JSON bytes. Content-Encoding has already been
/// handled by the HTTP layer.
pub fn decode_store_body(body: &[u8]) -> Option<Vec<u8>> {
    let trimmed = trim_ascii(body);
    if trimmed.first() == Some(&b'{') {
        return Some(trimmed.to_vec());
    }
    let decoded = base64::engine::general_purpose::STANDARD.decode(trimmed).ok()?;
    let mut out = Vec::new();
    if flate2::read::ZlibDecoder::new(&decoded[..]).read_to_end(&mut out).is_ok() {
        return Some(out);
    }
    (decoded.first() == Some(&b'{')).then_some(decoded)
}

fn trim_ascii(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !c.is_ascii_whitespace()).map_or(start, |i| i + 1);
    &b[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn plain_json() {
        assert_eq!(decode_store_body(b" {\"a\":1}\n").unwrap(), b"{\"a\":1}");
    }

    #[test]
    fn base64_zlib() {
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(b"{\"message\":\"m\"}").unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(enc.finish().unwrap());
        assert_eq!(decode_store_body(b64.as_bytes()).unwrap(), b"{\"message\":\"m\"}");
    }

    #[test]
    fn garbage() {
        assert!(decode_store_body(b"%%%").is_none());
    }
}
