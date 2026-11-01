//! Formatting helpers of the CLI and the TUI. What both sides share comes
//! from `mallard_term`; what is here is specific to error tracking.

pub(crate) use mallard_term::fmt::{age, bytes, relative_time, timestamp};
pub(crate) use mallard_term::text::{clean, clean_text, pad, truncate, truncate_left, width};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Unicode sparkline, one character per value. Zero is always the lowest
/// bar and any non-zero value is at least one step higher, so a single
/// event stays visible next to a spike.
pub(crate) fn sparkline(values: &[u64]) -> String {
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
pub(crate) fn resample(values: &[u64], width: usize) -> Vec<u64> {
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

/// 1234 -> "1.2k", 3_400_000 -> "3.4M". Small numbers stay exact.
pub(crate) fn compact(n: u64) -> String {
    mallard_term::fmt::compact(n as f64)
}

/// Parse "90s", "5m", "24h", "14d", "2w" into seconds.
pub(crate) fn parse_duration(s: &str) -> Option<u64> {
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
    n.checked_mul(mult)
}

/// Human interval: 1800 -> "30m", 86400 -> "1d".
pub(crate) fn duration(secs: u64) -> String {
    match secs {
        s if s >= 86_400 && s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s >= 3600 && s % 3600 == 0 => format!("{}h", s / 3600),
        s if s >= 60 && s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

/// Horizontal bar of `width` cells filled to `value / max`, using eighth
/// blocks for the last cell.
pub(crate) fn hbar(value: u64, max: u64, width: usize) -> String {
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

fn clean_line_or_text(s: &str, keep_newlines: bool) -> String {
    if keep_newlines { clean_text(s).into_owned() } else { clean(s).into_owned() }
}

fn clean_value(v: &mut Value, keep_newlines: bool) {
    match v {
        Value::String(s) => *s = clean_line_or_text(s, keep_newlines),
        Value::Array(a) => a.iter_mut().for_each(|x| clean_value(x, keep_newlines)),
        Value::Object(o) => {
            let old = std::mem::take(o);
            for (k, mut x) in old {
                clean_value(&mut x, keep_newlines);
                o.insert(clean_line_or_text(&k, keep_newlines), x);
            }
        }
        _ => {}
    }
}

/// [`clean_text`] (or [`clean`]) every string and object key inside an API response, so
/// no field of it can carry terminal escape sequences. Used for all
/// non-JSON output; `--json` output is escaped by serde instead. Fails
/// closed: a value that does not survive the JSON round trip is an error,
/// never returned uncleaned.
pub(crate) fn sanitize<T: Serialize + DeserializeOwned>(v: T, keep_newlines: bool) -> Result<T, serde_json::Error> {
    let mut value = serde_json::to_value(&v)?;
    clean_value(&mut value, keep_newlines);
    serde_json::from_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(compact(1234), "1.2k");
        assert_eq!(parse_duration("24h"), Some(86_400));
        assert_eq!(parse_duration("14d"), Some(14 * 86_400));
        assert_eq!(parse_duration("x"), None);
        assert_eq!(parse_duration("5y"), None);
        assert_eq!(parse_duration("99999999999999999999d"), None);
        assert_eq!(parse_duration("9999999999999999w"), None, "overflow is None, not a panic");
        assert_eq!(duration(1800), "30m");
        assert_eq!(duration(86_400), "1d");
        assert_eq!(duration(90), "90s");
        assert_eq!(hbar(5, 10, 4), "██");
        assert_eq!(hbar(10, 10, 4), "████");
        assert_eq!(hbar(1, 16, 2), "▏");
    }
}
