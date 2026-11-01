//! Query time windows: `since` (relative) or `from`/`to`, a default length,
//! a maximum length, and bucket steps for time series.

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};

use crate::duration;

/// A time window that is not usable. The text is meant for API clients.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum WindowError {
    #[error("invalid since={0:?}: {1}")]
    InvalidSince(String, duration::DurationError),
    #[error("time window larger than query.max_window ({})", humantime::format_duration(*.0))]
    TooLarge(Duration),
    #[error("time window starts before the earliest supported time")]
    OutOfRange,
    #[error("`from` must be before `to`")]
    Empty,
    #[error("invalid step={0:?}: {1}")]
    InvalidStep(String, duration::DurationError),
    #[error("step too small: more than {MAX_BUCKETS} buckets")]
    TooManyBuckets,
}

/// Most buckets one time series may have.
pub const MAX_BUCKETS: u64 = 5000;

/// A resolved time window in UTC: `from` inclusive, `to` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

impl Window {
    /// Resolve query parameters. `to` defaults to now. Without `from` and
    /// `since` the window is `default` long. Never longer than `max`.
    pub fn resolve(
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        since: Option<&str>,
        default: Duration,
        max: Duration,
    ) -> Result<Self, WindowError> {
        let to = to.unwrap_or_else(Utc::now);
        // `since` is user input: check it against `max` before any date
        // arithmetic, and never let the subtraction overflow (chrono panics).
        let back = |d: Duration| -> Result<DateTime<Utc>, WindowError> {
            if d > max {
                return Err(WindowError::TooLarge(max));
            }
            TimeDelta::from_std(d).ok().and_then(|d| to.checked_sub_signed(d)).ok_or(WindowError::OutOfRange)
        };
        let from = match (from, since) {
            (Some(f), _) => f,
            (None, Some(s)) => back(duration::parse(s).map_err(|e| WindowError::InvalidSince(s.into(), e))?)?,
            (None, None) => back(default.min(max))?,
        };
        if from >= to {
            return Err(WindowError::Empty);
        }
        if (to - from).to_std().unwrap_or(Duration::MAX) > max {
            return Err(WindowError::TooLarge(max));
        }
        Ok(Self { from, to })
    }

    /// The newest part of the window, growing: its last 1 hour, 6 hours, 1 day and
    /// 1 week, as far as those are shorter than the window, and the whole window.
    /// For suggestions, which need a sample rather than every row: stop at the
    /// first part with enough rows, and the cost no longer grows with the window.
    pub fn newest_first(&self) -> Vec<Window> {
        let parts = [1, 6, 24, 7 * 24].into_iter().filter_map(|hours| {
            let from = self.to.checked_sub_signed(TimeDelta::hours(hours))?;
            (from > self.from).then_some(Window { from, to: self.to })
        });
        parts.chain([*self]).collect()
    }

    /// Length in seconds.
    pub fn secs(&self) -> f64 {
        (self.to - self.from).num_microseconds().unwrap_or(i64::MAX) as f64 / 1e6
    }

    /// Bucket width in seconds: an explicit `step` (at least 1s, at most the
    /// whole window), else about 60 buckets on a round boundary.
    pub fn step(&self, step: Option<&str>) -> Result<u64, WindowError> {
        let secs = self.secs();
        let s = match step {
            Some(s) => {
                let d = duration::parse(s).map_err(|e| WindowError::InvalidStep(s.into(), e))?;
                // At most one bucket for the whole window (also bounds all
                // arithmetic on the step below).
                d.as_secs().clamp(1, (secs.ceil() as u64).clamp(1, 366 * 86400))
            }
            None => {
                const NICE: [u64; 17] =
                    [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 10800, 21600, 86400];
                let raw = (secs / 60.0).ceil() as u64;
                *NICE.iter().find(|n| **n >= raw).unwrap_or(&86400)
            }
        };
        if secs / s as f64 > MAX_BUCKETS as f64 {
            return Err(WindowError::TooManyBuckets);
        }
        Ok(s)
    }

    /// All bucket starts (epoch seconds) for a step.
    pub fn buckets(&self, step: u64) -> Vec<i64> {
        let step = step as i64;
        let first = self.from.timestamp().div_euclid(step) * step;
        let last = (self.to.timestamp() - 1).div_euclid(step) * step;
        (0..=((last - first) / step).max(0)).map(|i| first + i * step).collect()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);
    const DAY: Duration = Duration::from_secs(86_400);

    fn since(s: &str, default: Duration, max: Duration) -> Result<Window, WindowError> {
        Window::resolve(None, None, Some(s), default, max)
    }

    #[test]
    fn since_and_steps() {
        let r = since("1h", Duration::from_secs(60), 2 * HOUR).unwrap();
        assert!((r.secs() - 3600.0).abs() < 1.0);
        assert_eq!(r.step(None).unwrap(), 60);
        assert_eq!(since("3h", HOUR, 2 * HOUR), Err(WindowError::TooLarge(2 * HOUR)));
        assert!(since("3h", HOUR, 2 * HOUR).unwrap_err().to_string().contains("max_window"));
        assert_eq!(r.step(Some("100ms")).unwrap(), 1);
        assert_eq!(r.step(Some("0s")).unwrap(), 1);
        assert_eq!(r.step(Some("2w")).unwrap(), 3600, "at most one bucket");
        assert!(matches!(r.step(Some("x")), Err(WindowError::InvalidStep(..))));
        assert_eq!(since("2w", DAY, Duration::MAX).unwrap().step(Some("1s")), Err(WindowError::TooManyBuckets));
        assert!(matches!(since("soon", DAY, DAY), Err(WindowError::InvalidSince(..))));
    }

    #[test]
    fn huge_since_is_an_error_not_a_panic() {
        assert!(since("300000y", DAY, 31 * DAY).is_err());
        // Even when the window limit allows it, the subtraction must not overflow.
        assert_eq!(since("300000y", DAY, Duration::MAX), Err(WindowError::OutOfRange));
    }

    #[test]
    fn default_window_is_capped_at_max_window() {
        let r = Window::resolve(None, None, None, 7 * DAY, DAY).unwrap();
        assert!((r.secs() - 86_400.0).abs() < 1.0);
    }

    #[test]
    fn from_and_to() {
        let t = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(Window::resolve(Some(t), Some(t), None, HOUR, HOUR), Err(WindowError::Empty));
        let w = Window::resolve(Some(t), Some(t + TimeDelta::minutes(5)), Some("1h"), HOUR, HOUR).unwrap();
        assert_eq!(w.secs(), 300.0, "from wins over since");
        assert!(Window::resolve(Some(t), Some(t + TimeDelta::hours(2)), None, HOUR, HOUR).is_err());
    }

    #[test]
    fn newest_first_grows_up_to_the_window() {
        let t = Utc.with_ymd_and_hms(2026, 1, 31, 0, 0, 0).unwrap();
        let lengths = |days: i64| {
            let w = Window { from: t - TimeDelta::days(days), to: t };
            let parts = w.newest_first();
            assert!(parts.iter().all(|p| p.to == t));
            assert_eq!(parts.last(), Some(&w));
            parts.iter().map(|p| (p.to - p.from).num_hours()).collect::<Vec<_>>()
        };
        assert_eq!(lengths(30), [1, 6, 24, 168, 720]);
        assert_eq!(lengths(7), [1, 6, 24, 168], "no empty step at the end");
        let short = Window { from: t - TimeDelta::minutes(5), to: t };
        assert_eq!(short.newest_first(), [short]);
    }

    #[test]
    fn buckets_cover_window() {
        let w = Window {
            from: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 30).unwrap(),
            to: Utc.with_ymd_and_hms(2026, 1, 1, 0, 3, 0).unwrap(),
        };
        let b = w.buckets(60);
        assert_eq!(b.len(), 3);
        assert_eq!(b[0] % 60, 0);
    }
}
