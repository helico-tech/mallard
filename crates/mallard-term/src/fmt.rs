//! Numbers, sizes, durations and times, the same on both sides.
//!
//! Times are always shown in UTC: the server stores UTC, `--from`/`--to`
//! take RFC 3339, and a CLI line, a TUI cell and a server log then agree.

use chrono::{DateTime, Utc};

/// 1234 -> "1.2k", 3_400_000 -> "3.4M", 2e9 -> "2G". Values under 1000
/// stay exact when whole, else keep 2-3 significant decimals.
pub fn compact(v: f64) -> String {
    let a = v.abs();
    if a < 1000.0 {
        return if a == a.trunc() || a >= 100.0 {
            format!("{v:.0}")
        } else if a >= 1.0 {
            format!("{v:.2}")
        } else {
            format!("{v:.3}")
        };
    }
    let mut out = String::new();
    for (scale, unit) in [(1e3, "k"), (1e6, "M"), (1e9, "G"), (1e12, "T")] {
        let x = format!("{:.1}", v / scale);
        out = format!("{}{unit}", x.strip_suffix(".0").unwrap_or(&x));
        // 999_960 rounds to "1000.0k": use the next unit instead.
        if (v / scale).abs() < 999.95 {
            break;
        }
    }
    out
}

/// Byte size with decimal units, as the config files write them:
/// 512 -> "512 B", 1_500_000 -> "1.5 MB".
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

/// A duration in milliseconds: "500µs", "12.3ms", "2.50s", "1.5m".
pub fn ms(v: f64) -> String {
    if v >= 60_000.0 {
        format!("{:.1}m", v / 60_000.0)
    } else if v >= 1000.0 {
        format!("{:.2}s", v / 1000.0)
    } else if v >= 1.0 {
        format!("{v:.1}ms")
    } else {
        format!("{:.0}µs", v * 1000.0)
    }
}

/// "now", "42s ago", "3m ago", "5h ago", "2d ago", "3mo ago", "1y ago".
pub fn relative_time(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - t).num_seconds();
    match secs {
        ..5 => "now".into(),
        5..60 => format!("{secs}s ago"),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86_400 => format!("{}h ago", secs / 3600),
        86_400..2_592_000 => format!("{}d ago", secs / 86_400),
        2_592_000..31_536_000 => format!("{}mo ago", secs / 2_592_000),
        _ => format!("{}y ago", secs / 31_536_000),
    }
}

/// Like [`relative_time`] but without "ago": "3m", "2d" (for "age" columns).
pub fn age(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let s = relative_time(t, now);
    s.strip_suffix(" ago").map(str::to_string).unwrap_or(s)
}

/// A full timestamp, UTC: "2026-10-31 20:59:13".
pub fn timestamp(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn compact_numbers() {
        // sdl-cli's cases
        assert_eq!(compact(999.0), "999");
        assert_eq!(compact(1000.0), "1k");
        assert_eq!(compact(1234.0), "1.2k");
        assert_eq!(compact(999_999.0), "1M");
        assert_eq!(compact(3_400_000.0), "3.4M");
        assert_eq!(compact(2_000_000_000.0), "2G");
        // odl-cli's cases
        assert_eq!(compact(12_345.0), "12.3k");
        assert_eq!(compact(3.0), "3");
        assert_eq!(compact(2.5), "2.50");
        assert_eq!(compact(0.25), "0.250");
        assert_eq!(compact(-1500.0), "-1.5k");
    }

    #[test]
    fn sizes_and_durations() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 kB");
        assert_eq!(bytes(1_500_000), "1.5 MB");
        assert_eq!(ms(0.5), "500µs");
        assert_eq!(ms(12.34), "12.3ms");
        assert_eq!(ms(2500.0), "2.50s");
        assert_eq!(ms(90_000.0), "1.5m");
    }

    #[test]
    fn relative_time_buckets() {
        let now = Utc::now();
        assert_eq!(relative_time(now, now), "now");
        assert_eq!(relative_time(now + Duration::seconds(10), now), "now");
        assert_eq!(relative_time(now - Duration::seconds(42), now), "42s ago");
        assert_eq!(relative_time(now - Duration::minutes(3), now), "3m ago");
        assert_eq!(relative_time(now - Duration::hours(5), now), "5h ago");
        assert_eq!(relative_time(now - Duration::days(2), now), "2d ago");
        assert_eq!(relative_time(now - Duration::days(65), now), "2mo ago");
        assert_eq!(relative_time(now - Duration::days(800), now), "2y ago");
        assert_eq!(age(now - Duration::minutes(3), now), "3m");
    }

    #[test]
    fn timestamps_are_utc() {
        let t = DateTime::parse_from_rfc3339("2026-10-31T22:59:13+02:00").unwrap().with_timezone(&Utc);
        assert_eq!(timestamp(t), "2026-10-31 20:59:13");
    }
}
