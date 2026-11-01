//! Durations written as `5s`, `10m`, `7d`, `2w`, `1h30m`; `0` means zero.

use std::{fmt, time::Duration};

use serde::{Deserializer, Serializer, de};

pub use humantime::DurationError;

/// Parse `5s`, `10m`, `7d`, `2w`, `1h30m`. `0` means zero.
pub fn parse(s: &str) -> Result<Duration, DurationError> {
    let s = s.trim();
    if s == "0" {
        return Ok(Duration::ZERO);
    }
    humantime::parse_duration(s)
}

/// Serde for a [`Duration`] field: `#[serde(with = "mallard_common::duration")]`.
/// Accepts a string (see [`parse`]) or the number `0`.
pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    if d.is_zero() { s.serialize_str("0") } else { s.collect_str(&humantime::format_duration(*d)) }
}

/// See [`serialize`].
pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    struct V;
    impl de::Visitor<'_> for V {
        type Value = Duration;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a duration such as \"30s\", \"10m\" or \"7d\"")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<Duration, E> {
            parse(v).map_err(|e| E::custom(format!("invalid duration {v:?}: {e}")))
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Duration, E> {
            if v == 0 { Ok(Duration::ZERO) } else { Err(E::custom("a duration needs a unit, e.g. \"30s\"")) }
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Duration, E> {
            self.visit_u64(u64::try_from(v).map_err(|_| E::custom("a duration cannot be negative"))?)
        }
    }
    d.deserialize_any(V)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_zero_and_units() {
        assert_eq!(parse("0").unwrap(), Duration::ZERO);
        assert_eq!(parse(" 7d ").unwrap(), Duration::from_secs(7 * 86_400));
        assert_eq!(parse("2w").unwrap(), Duration::from_secs(14 * 86_400));
        assert_eq!(parse("1h30m").unwrap(), Duration::from_secs(5400));
        assert!(parse("5").is_err(), "a unit is required");
        assert!(parse("5 parsecs").is_err());
    }

    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct T {
        #[serde(with = "super")]
        d: Duration,
    }

    #[test]
    fn serde_round_trip() {
        let t: T = serde_json::from_str(r#"{"d":"250ms"}"#).unwrap();
        assert_eq!(t.d, Duration::from_millis(250));
        assert_eq!(serde_json::from_str::<T>(r#"{"d":0}"#).unwrap().d, Duration::ZERO);
        assert!(serde_json::from_str::<T>(r#"{"d":5}"#).is_err());
        assert_eq!(serde_json::to_string(&T { d: Duration::ZERO }).unwrap(), r#"{"d":"0"}"#);
        let back: T = serde_json::from_str(&serde_json::to_string(&T { d: Duration::from_secs(90) }).unwrap()).unwrap();
        assert_eq!(back.d, Duration::from_secs(90));
    }
}
