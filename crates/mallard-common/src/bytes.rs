//! Sizes in bytes written as `512MB`, `2GiB` or a plain number.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// A size in bytes. Parsed from `512MB`, `2GiB` or `1024` (bytes): decimal
/// units are powers of 1000, binary units (`KiB`, `MiB`, `GiB`) powers of 1024.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteSize(pub u64);

impl ByteSize {
    pub const fn mb(n: u64) -> Self {
        Self(n * 1_000_000)
    }

    pub const fn mib(n: u64) -> Self {
        Self(n << 20)
    }

    /// The size as `usize`, saturating on 32-bit targets.
    pub fn as_usize(self) -> usize {
        usize::try_from(self.0).unwrap_or(usize::MAX)
    }
}

/// Error parsing a [`ByteSize`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid size {0:?}: expected a number with an optional unit, e.g. 512MB or 2GiB")]
pub struct ByteSizeError(String);

impl FromStr for ByteSize {
    type Err = ByteSizeError;

    fn from_str(s: &str) -> Result<Self, ByteSizeError> {
        let s = s.trim();
        let err = || ByteSizeError(s.to_string());
        let idx = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
        let (num, unit) = s.split_at(idx);
        let num: f64 = num.parse().map_err(|_| err())?;
        let mult: f64 = match unit.trim().to_ascii_lowercase().as_str() {
            "" | "b" => 1.0,
            "kb" | "k" => 1e3,
            "mb" | "m" => 1e6,
            "gb" | "g" => 1e9,
            "tb" | "t" => 1e12,
            "kib" => 1024.0,
            "mib" => 1024.0 * 1024.0,
            "gib" => 1024.0 * 1024.0 * 1024.0,
            "tib" => 1024f64.powi(4),
            _ => return Err(err()),
        };
        Ok(Self((num * mult) as u64))
    }
}

impl fmt::Display for ByteSize {
    /// The largest unit that divides the value exactly (decimal first).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.0;
        const UNITS: [(&str, u64); 8] = [
            ("TB", 1_000_000_000_000),
            ("GB", 1_000_000_000),
            ("MB", 1_000_000),
            ("KB", 1_000),
            ("TiB", 1 << 40),
            ("GiB", 1 << 30),
            ("MiB", 1 << 20),
            ("KiB", 1 << 10),
        ];
        for (unit, m) in UNITS {
            if n != 0 && n.is_multiple_of(m) {
                return write!(f, "{}{unit}", n / m);
            }
        }
        write!(f, "{n}")
    }
}

impl Serialize for ByteSize {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    /// A string with a unit, or a plain number of bytes.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(Visitor).and_then(|v| v.ok_or_else(|| de::Error::custom("a size cannot be empty")))
    }
}

/// Serde for `Option<ByteSize>` where an empty string means `None`:
/// `#[serde(with = "mallard_common::bytes::optional")]`.
pub mod optional {
    use super::*;

    pub fn serialize<S: Serializer>(v: &Option<ByteSize>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(b) => b.serialize(s),
            None => s.serialize_str(""),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<ByteSize>, D::Error> {
        d.deserialize_any(Visitor)
    }
}

/// Reads a size; an empty string is `None`.
struct Visitor;

impl de::Visitor<'_> for Visitor {
    type Value = Option<ByteSize>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a size such as \"512MB\" or a number of bytes")
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        if v.trim().is_empty() { Ok(None) } else { v.parse().map(Some).map_err(E::custom) }
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        Ok(Some(ByteSize(v)))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        u64::try_from(v).map(|v| Some(ByteSize(v))).map_err(|_| E::custom("a size cannot be negative"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_display() {
        assert_eq!("512MB".parse(), Ok(ByteSize(512_000_000)));
        assert_eq!("1GiB".parse(), Ok(ByteSize(1 << 30)));
        assert_eq!("100".parse(), Ok(ByteSize(100)));
        assert!("5XB".parse::<ByteSize>().is_err());
        assert_eq!(ByteSize::mb(128).to_string(), "128MB");
        assert_eq!(ByteSize::mib(256).to_string(), "256MiB");
        assert_eq!(ByteSize(1000 * 1024).to_string(), "1024KB");
        assert_eq!(ByteSize(1023).to_string(), "1023");
        for b in [ByteSize(0), ByteSize(1 << 30), ByteSize::mb(3), ByteSize(12345)] {
            assert_eq!(b.to_string().parse(), Ok(b));
        }
    }

    #[derive(Serialize, Deserialize)]
    struct T {
        size: ByteSize,
        #[serde(with = "optional")]
        limit: Option<ByteSize>,
    }

    #[test]
    fn serde() {
        let t: T = serde_json::from_str(r#"{"size":"2GB","limit":""}"#).unwrap();
        assert_eq!((t.size, t.limit), (ByteSize(2_000_000_000), None));
        let t: T = serde_json::from_str(r#"{"size":1024,"limit":"1MiB"}"#).unwrap();
        assert_eq!((t.size, t.limit), (ByteSize(1024), Some(ByteSize(1 << 20))));
        assert!(serde_json::from_str::<T>(r#"{"size":"","limit":""}"#).is_err());
        assert!(serde_json::from_str::<T>(r#"{"size":-1,"limit":""}"#).is_err());
    }
}
