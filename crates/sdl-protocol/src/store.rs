//! Legacy `/api/<project>/store/` bodies.
//!
//! Modern SDKs send plain (optionally gzip'd via Content-Encoding) JSON.
//! Very old SDKs (raven) send base64 of zlib-compressed JSON.

use base64::Engine;
use std::io::Read;

/// Decode a store body to JSON bytes, inflating at most `max_len` bytes
/// (returns `None` beyond that: zlib bombs). Content-Encoding has already
/// been handled by the HTTP layer.
pub fn decode_store_body(body: &[u8], max_len: usize) -> Option<Vec<u8>> {
    let trimmed = trim_ascii(body);
    if trimmed.first() == Some(&b'{') {
        return Some(trimmed.to_vec());
    }
    let decoded = base64::engine::general_purpose::STANDARD.decode(trimmed).ok()?;
    let mut out = Vec::new();
    let limit = max_len as u64 + 1;
    if flate2::read::ZlibDecoder::new(&decoded[..]).take(limit).read_to_end(&mut out).is_ok() {
        return (out.len() <= max_len).then_some(out);
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
        assert_eq!(decode_store_body(b" {\"a\":1}\n", 100).unwrap(), b"{\"a\":1}");
    }

    #[test]
    fn base64_zlib() {
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(b"{\"message\":\"m\"}").unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(enc.finish().unwrap());
        assert_eq!(decode_store_body(b64.as_bytes(), 100).unwrap(), b"{\"message\":\"m\"}");
    }

    #[test]
    fn zlib_bomb_is_rejected() {
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&vec![b'A'; 10_000_000]).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(enc.finish().unwrap());
        assert!(decode_store_body(b64.as_bytes(), 1024 * 1024).is_none());
    }

    #[test]
    fn garbage() {
        assert!(decode_store_body(b"%%%", 100).is_none());
    }
}
