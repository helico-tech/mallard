//! `--wait <dur>`: run a lookup again until it finds something, because
//! ingest is asynchronous (data shows up a few seconds after it is sent).
//! When the time is up the command prints what it has (the empty result)
//! and exits with [`Kind::Unmet`] (exit code 8, ADR 0008).

use std::future::Future;
use std::time::Duration;

use crate::error::{Described, Kind};
use crate::output::Output;
use crate::window::parse_duration;

/// How long to sleep between two tries.
pub const INTERVAL: Duration = Duration::from_secs(1);

/// The `--wait` flag.
#[derive(clap::Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct WaitArgs {
    /// Poll until there is a result, up to this long (e.g. 30s); exit 8 if nothing arrives in time.
    #[arg(long, value_name = "DUR", value_parser = crate::window::parse_since)]
    pub wait: Option<String>,
}

impl WaitArgs {
    /// `--wait 30s`.
    pub fn of(d: &str) -> Self {
        WaitArgs { wait: Some(d.to_string()) }
    }

    /// The wait, or `None` without the flag.
    pub fn duration(&self) -> anyhow::Result<Option<Duration>> {
        self.wait.as_deref().map(|w| parse_duration(w).map_err(crate::error::usage)).transpose()
    }
}

/// The last result of [`until`] and, when the wait ran out without a
/// result, the `--wait` as typed.
#[derive(Debug)]
pub struct Waited<T> {
    pub value: T,
    pub timed_out: Option<String>,
}

impl<T> Waited<T> {
    /// The value, and the exit-8 error when the wait ran out. Return the
    /// error after printing the value, so the (empty) result is still on
    /// stdout.
    pub fn split(self) -> (T, anyhow::Result<()>) {
        let res = match &self.timed_out {
            None => Ok(()),
            Some(d) => Err(timed_out(d)),
        };
        (self.value, res)
    }
}

/// The error for "nothing arrived within `wait`" (exit code 8).
pub fn timed_out(wait: &str) -> anyhow::Error {
    anyhow::Error::new(
        Described::new(Kind::Unmet, format!("nothing arrived within {wait}"))
            .hint("raise --wait, or widen the window with --since"),
    )
}

/// Run `attempt`; with a wait, again every [`INTERVAL`] until `found` says
/// yes or the wait is over. Without a wait it runs once and never times
/// out. An error ends the wait at once. The first retry prints a note
/// (`--quiet` drops it).
pub async fn until<T, Fut>(
    out: &Output,
    wait: &WaitArgs,
    mut attempt: impl FnMut() -> Fut,
    found: impl Fn(&T) -> bool,
) -> anyhow::Result<Waited<T>>
where
    Fut: Future<Output = anyhow::Result<T>>,
{
    let (Some(d), Some(typed)) = (wait.duration()?, wait.wait.as_deref()) else {
        return Ok(Waited { value: attempt().await?, timed_out: None });
    };
    let deadline = tokio::time::Instant::now() + d;
    let mut noted = false;
    loop {
        let value = attempt().await?;
        if found(&value) {
            return Ok(Waited { value, timed_out: None });
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(Waited { value, timed_out: Some(typed.to_string()) });
        }
        if !noted {
            out.note(format!("nothing yet; trying again for up to {typed}"));
            noted = true;
        }
        tokio::time::sleep(INTERVAL.min(deadline - now)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn quiet() -> Output {
        Output::default().with_quiet(true)
    }

    #[test]
    fn the_flag() {
        assert_eq!(WaitArgs::default().duration().unwrap(), None);
        assert_eq!(WaitArgs::of("30s").duration().unwrap(), Some(Duration::from_secs(30)));
        assert_eq!(crate::error::exit_code(&WaitArgs::of("soon").duration().map(|_| ())), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn polls_until_found() {
        let n = Cell::new(0);
        let w = until(&quiet(), &WaitArgs::of("10s"), || async { Ok(n.replace(n.get() + 1)) }, |v| *v >= 3).await;
        let (v, res) = w.unwrap().split();
        assert_eq!(v, 3);
        assert!(res.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_with_exit_8_and_keeps_the_last_value() {
        let start = tokio::time::Instant::now();
        let w = until(&quiet(), &WaitArgs::of("3s"), || async { Ok(Vec::<u8>::new()) }, |v| !v.is_empty()).await;
        let (v, res) = w.unwrap().split();
        assert!(v.is_empty());
        assert_eq!(start.elapsed(), Duration::from_secs(3), "stops at the deadline");
        assert_eq!(crate::error::exit_code(&res), 8);
        let d = crate::error::described(&res.unwrap_err());
        assert_eq!((d.kind.code(), d.message.as_str()), ("condition_not_met", "nothing arrived within 3s"));
    }

    #[tokio::test]
    async fn without_a_wait_it_runs_once() {
        let n = Cell::new(0);
        let w = until(&quiet(), &WaitArgs::default(), || async { Ok(n.replace(n.get() + 1)) }, |_| false).await;
        assert_eq!((w.unwrap().timed_out, n.get()), (None, 1));
    }

    #[tokio::test(start_paused = true)]
    async fn errors_end_the_wait() {
        let n = Cell::new(0);
        let res = until(
            &quiet(),
            &WaitArgs::of("10s"),
            || async {
                n.set(n.get() + 1);
                Err::<u8, _>(crate::error::not_found("gone"))
            },
            |_| true,
        )
        .await;
        assert_eq!((crate::error::exit_code(&res.map(|_| ())), n.get()), (4, 1));
    }
}
