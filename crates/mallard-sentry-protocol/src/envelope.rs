//! Envelope parsing (https://develop.sentry.dev/sdk/envelopes/).
//!
//! ```text
//! {"event_id":"...","dsn":"..."}\n
//! {"type":"event","length":41}\n
//! {"message":"hello","level":"error"}\n
//! {"type":"attachment"}\n            <- no length: payload runs to next \n
//! ...
//! ```

use bytes::Bytes;
use serde_json::{Map, Value};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("envelope is empty")]
    Empty,
    #[error("invalid envelope header: {0}")]
    InvalidHeader(String),
    #[error("invalid item header: {0}")]
    InvalidItemHeader(String),
    #[error("item payload is shorter than its declared length")]
    Truncated,
}

#[derive(Debug, Clone)]
pub struct Envelope {
    pub headers: Map<String, Value>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub headers: Map<String, Value>,
    pub payload: Bytes,
}

impl Item {
    pub fn ty(&self) -> &str {
        self.headers.get("type").and_then(Value::as_str).unwrap_or("")
    }
}

impl Envelope {
    pub fn event_id(&self) -> Option<&str> {
        self.headers.get("event_id").and_then(Value::as_str)
    }
    pub fn dsn(&self) -> Option<&str> {
        self.headers.get("dsn").and_then(Value::as_str)
    }
    /// When the SDK sent it, by its own clock.
    pub fn sent_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.headers.get("sent_at").and_then(crate::event::parse_timestamp)
    }

    pub fn parse(data: Bytes) -> Result<Self, EnvelopeError> {
        let mut pos = 0usize;
        let header_line = next_line(&data, &mut pos).ok_or(EnvelopeError::Empty)?;
        let headers = parse_obj(header_line).map_err(EnvelopeError::InvalidHeader)?;
        let mut items = Vec::new();
        loop {
            // Skip blank lines between items (some SDKs emit them).
            while pos < data.len() && (data[pos] == b'\n' || data[pos] == b'\r') {
                pos += 1;
            }
            if pos >= data.len() {
                break;
            }
            let line = next_line(&data, &mut pos).unwrap_or_default();
            let item_headers = parse_obj(line).map_err(EnvelopeError::InvalidItemHeader)?;
            let payload = match item_headers.get("length").and_then(Value::as_u64) {
                Some(len) => {
                    let len = usize::try_from(len).unwrap_or(usize::MAX);
                    if len > data.len() - pos {
                        return Err(EnvelopeError::Truncated);
                    }
                    let p = data.slice(pos..pos + len);
                    pos += len;
                    // Consume the optional newline after the payload.
                    if pos < data.len() && data[pos] == b'\n' {
                        pos += 1;
                    }
                    p
                }
                None => {
                    let start = pos;
                    let end = data[pos..].iter().position(|b| *b == b'\n').map_or(data.len(), |i| pos + i);
                    pos = (end + 1).min(data.len());
                    data.slice(start..end)
                }
            };
            items.push(Item { headers: item_headers, payload });
        }
        Ok(Envelope { headers, items })
    }
}

fn next_line<'a>(data: &'a Bytes, pos: &mut usize) -> Option<&'a [u8]> {
    if *pos >= data.len() {
        return None;
    }
    let rest = &data[*pos..];
    let end = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
    *pos += (end + 1).min(rest.len());
    let line = &rest[..end];
    Some(line.strip_suffix(b"\r").unwrap_or(line))
}

fn parse_obj(line: &[u8]) -> Result<Map<String, Value>, String> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(line) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err("not a JSON object".into()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_items_with_and_without_length() {
        let raw = b"{\"event_id\":\"9ec79c33ec9942ab8353589fcb2e04dc\",\"dsn\":\"https://e12d836b15bb49d7bbf99e64295d995b:@sentry.io/42\"}\n{\"type\":\"attachment\",\"length\":10,\"content_type\":\"text/plain\",\"filename\":\"hello.txt\"}\n\xef\xbb\xbfHello\r\n\n{\"type\":\"event\",\"length\":41,\"content_type\":\"application/json\",\"filename\":\"application.log\"}\n{\"message\":\"hello world\",\"level\":\"error\"}\n";
        let env = Envelope::parse(Bytes::from_static(raw)).unwrap();
        assert_eq!(env.event_id(), Some("9ec79c33ec9942ab8353589fcb2e04dc"));
        assert_eq!(env.items.len(), 2);
        assert_eq!(env.items[0].ty(), "attachment");
        assert_eq!(env.items[0].payload.len(), 10);
        assert_eq!(env.items[1].ty(), "event");
        assert_eq!(&env.items[1].payload[..], b"{\"message\":\"hello world\",\"level\":\"error\"}");
    }

    #[test]
    fn parses_implicit_length_and_no_trailing_newline() {
        let raw = b"{}\n{\"type\":\"session\"}\n{\"started\":\"2020-02-07T14:16:00Z\"}\n{\"type\":\"event\"}\n{\"message\":\"x\"}";
        let env = Envelope::parse(Bytes::from_static(raw)).unwrap();
        assert_eq!(env.items.len(), 2);
        assert_eq!(&env.items[1].payload[..], b"{\"message\":\"x\"}");
    }

    #[test]
    fn empty_payload_and_header_only() {
        let env = Envelope::parse(Bytes::from_static(b"{\"event_id\":\"a\"}\n")).unwrap();
        assert!(env.items.is_empty());
        let env = Envelope::parse(Bytes::from_static(b"{}\n{\"type\":\"attachment\",\"length\":0}\n\n")).unwrap();
        assert_eq!(env.items.len(), 1);
        assert!(env.items[0].payload.is_empty());
    }

    #[test]
    fn huge_length_does_not_overflow() {
        let raw = b"{}\n{\"type\":\"event\",\"length\":18446744073709551615}\n{}";
        assert_eq!(Envelope::parse(Bytes::from_static(raw)).unwrap_err(), EnvelopeError::Truncated);
    }

    #[test]
    fn errors() {
        assert_eq!(Envelope::parse(Bytes::new()).unwrap_err(), EnvelopeError::Empty);
        assert!(matches!(Envelope::parse(Bytes::from_static(b"nope\n")), Err(EnvelopeError::InvalidHeader(_))));
        assert_eq!(
            Envelope::parse(Bytes::from_static(b"{}\n{\"type\":\"event\",\"length\":99}\n{}")).unwrap_err(),
            EnvelopeError::Truncated
        );
    }
}
