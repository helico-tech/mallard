//! Formatting helpers of the CLI and the TUI. What both sides share comes
//! from `mallard_term`; what is here is specific to error tracking.

pub(crate) use mallard_term::fmt::{age, bytes, relative_time, timestamp};
pub(crate) use mallard_term::text::{clean, pad, truncate, truncate_left, width};

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

pub(crate) use mallard_term::text::sanitize;

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
