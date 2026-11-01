//! Small formatting helpers shared by the CLI and the TUI.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Unicode sparkline, one character per value. Zero is always the lowest
/// bar and any non-zero value is at least one step higher, so a single
/// event stays visible next to a spike.
pub fn sparkline(values: &[u64]) -> String {
    let max = values.iter().copied().max().unwrap_or(0);
    values
        .iter()
        .map(|&v| {
            if v == 0 || max == 0 {
                BARS[0]
            } else {
                let step = ((v as f64 / max as f64) * 6.0).round() as usize;
                BARS[(1 + step).min(7)]
            }
        })
        .collect()
}

/// Resample `values` to exactly `width` points. Shrinking sums the values
/// that fall into each point (so totals are kept); growing repeats values.
pub fn resample(values: &[u64], width: usize) -> Vec<u64> {
    if values.is_empty() || width == 0 {
        return vec![0; width];
    }
    let n = values.len();
    if width >= n {
        return (0..width).map(|i| values[i * n / width]).collect();
    }
    let mut out = vec![0u64; width];
    for (i, v) in values.iter().enumerate() {
        out[i * width / n] += v;
    }
    out
}

/// "now", "42s ago", "3m ago", "5h ago", "2d ago", "3mo ago", "1y ago".
pub fn relative_time(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - t).num_seconds();
    if secs < 0 {
        return "now".into();
    }
    match secs {
        0..5 => "now".into(),
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

/// 1234 -> "1.2k", 3_400_000 -> "3.4M". Small numbers stay exact.
pub fn compact(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..999_950 => trim_dot0(format!("{:.1}k", n as f64 / 1e3)),
        999_950..999_950_000 => trim_dot0(format!("{:.1}M", n as f64 / 1e6)),
        _ => trim_dot0(format!("{:.1}B", n as f64 / 1e9)),
    }
}

fn trim_dot0(s: String) -> String {
    s.replace(".0k", "k").replace(".0M", "M").replace(".0B", "B")
}

/// 1536 -> "1.5 KiB".
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

/// Cut `s` to at most `max` characters, ending with "…" when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// Cut `s` to at most `max` characters keeping the end: "…/src/app.js:10".
pub fn truncate_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::from("…");
    out.extend(s.chars().skip(n - (max - 1)));
    out
}

/// Short local-free timestamp for tables: "2026-10-31 20:59:13".
pub fn timestamp(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Parse "90s", "5m", "24h", "14d", "2w" into seconds.
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let (num, unit) = s.split_at(split);
    let n: u64 = num.parse().ok()?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    Some(n * mult)
}

/// Human interval: 1800 -> "30m", 86400 -> "1d".
pub fn duration(secs: u64) -> String {
    match secs {
        s if s >= 86_400 && s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s >= 3600 && s % 3600 == 0 => format!("{}h", s / 3600),
        s if s >= 60 && s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

/// Horizontal bar of `width` cells filled to `value / max`, using eighth
/// blocks for the last cell.
pub fn hbar(value: u64, max: u64, width: usize) -> String {
    if max == 0 || width == 0 {
        return String::new();
    }
    let eighths = ((value as f64 / max as f64) * (width * 8) as f64).round() as usize;
    let full = eighths / 8;
    let rest = eighths % 8;
    let mut s = "█".repeat(full.min(width));
    if rest > 0 && full < width {
        s.push([' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'][rest]);
    }
    s
}

/// Make untrusted text (event data, server messages) safe to print to a
/// terminal: tabs become spaces, other control characters (C0, DEL, C1, so
/// ESC, OSC, CSI, BEL, CR, ...) become U+FFFD, and bidi overrides are
/// dropped. Newlines are kept when `keep_newlines`, else they become spaces.
pub fn clean_text(s: &str, keep_newlines: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' if keep_newlines => out.push('\n'),
            '\n' => out.push(' '),
            '\r' if keep_newlines => {}
            '\r' => out.push(' '),
            '\t' => out.push_str("    "),
            '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => {}
            c if c.is_control() => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

/// One-line [`clean_text`].
pub fn clean(s: &str) -> String {
    clean_text(s, false)
}

fn clean_value(v: &mut Value, keep_newlines: bool) {
    match v {
        Value::String(s) => *s = clean_text(s, keep_newlines),
        Value::Array(a) => a.iter_mut().for_each(|x| clean_value(x, keep_newlines)),
        Value::Object(o) => {
            let old = std::mem::take(o);
            for (k, mut x) in old {
                clean_value(&mut x, keep_newlines);
                o.insert(clean_text(&k, keep_newlines), x);
            }
        }
        _ => {}
    }
}

/// [`clean_text`] every string (and object key) inside an API response, so
/// no field of it can carry terminal escape sequences. Used for all
/// non-JSON output; `--json` output is escaped by serde instead. Fails
/// closed: a value that does not survive the JSON round trip is an error,
/// never returned uncleaned.
pub fn sanitize<T: Serialize + DeserializeOwned>(v: T, keep_newlines: bool) -> Result<T, serde_json::Error> {
    let mut value = serde_json::to_value(&v)?;
    clean_value(&mut value, keep_newlines);
    serde_json::from_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn sparkline_scales_and_keeps_small_values_visible() {
        assert_eq!(sparkline(&[]), "");
        assert_eq!(sparkline(&[0, 0, 0]), "▁▁▁");
        assert_eq!(sparkline(&[0, 1, 100]), "▁▂█");
        assert_eq!(sparkline(&[5, 5]), "██");
        assert_eq!(sparkline(&[0, 50, 100]), "▁▅█");
    }

    #[test]
    fn resample_keeps_totals_when_shrinking() {
        assert_eq!(resample(&[1, 2, 3, 4], 2), vec![3, 7]);
        assert_eq!(resample(&[1, 2], 4), vec![1, 1, 2, 2]);
        assert_eq!(resample(&[], 3), vec![0, 0, 0]);
        let v: Vec<u64> = (0..24).collect();
        assert_eq!(resample(&v, 12).iter().sum::<u64>(), v.iter().sum::<u64>());
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
    fn compact_numbers() {
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1000), "1k");
        assert_eq!(compact(1234), "1.2k");
        assert_eq!(compact(999_999), "1M");
        assert_eq!(compact(3_400_000), "3.4M");
        assert_eq!(compact(2_000_000_000), "2B");
    }

    #[test]
    fn clean_strips_terminal_escapes() {
        let evil = "a\x1b]52;c;ZXZpbA==\x07b\x1b[2Jc\u{9b}31md\re\tf\u{202e}g";
        let out = clean(evil);
        assert!(!out.chars().any(|c| c.is_control()), "{out:?}");
        assert_eq!(out, "a\u{FFFD}]52;c;ZXZpbA==\u{FFFD}b\u{FFFD}[2Jc\u{FFFD}31md e    fg");
        assert_eq!(clean_text("l1\r\nl2", true), "l1\nl2");
        assert_eq!(clean("l1\nl2"), "l1 l2");
    }

    #[test]
    fn sanitize_cleans_nested_strings_and_keys() {
        let v = serde_json::json!({"t\x1b[2J": ["x\x1b]0;pwn\x07", {"k": "\u{9b}"}], "n": 1});
        let s = sanitize(v, false).unwrap().to_string();
        assert!(!s.contains('\u{1b}') && !s.contains('\u{9b}') && !s.contains('\u{7}'), "{s}");
        assert!(s.contains("\"n\":1"));
    }

    #[test]
    fn sanitize_fails_closed() {
        #[derive(Serialize, serde::Deserialize, Debug)]
        struct Ratio {
            label: String,
            value: f64,
        }
        // NaN serializes to null, which does not deserialize as f64.
        let r = sanitize(Ratio { label: "\x1b[2Jx".into(), value: f64::NAN }, false);
        assert!(r.is_err(), "{r:?}");
    }

    #[test]
    fn misc_helpers() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate_left("/a/b/c/app.js:10", 10), "…app.js:10");
        assert_eq!(truncate_left("app.js", 10), "app.js");
        assert_eq!(parse_duration("24h"), Some(86_400));
        assert_eq!(parse_duration("14d"), Some(14 * 86_400));
        assert_eq!(parse_duration("x"), None);
        assert_eq!(parse_duration("5y"), None);
        assert_eq!(duration(1800), "30m");
        assert_eq!(duration(86_400), "1d");
        assert_eq!(duration(90), "90s");
        assert_eq!(hbar(5, 10, 4), "██");
        assert_eq!(hbar(10, 10, 4), "████");
        assert_eq!(hbar(1, 16, 2), "▏");
    }
}
