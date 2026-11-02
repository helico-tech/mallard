---
status: done
type: bug
created: 2026-11-02
---
# `shutdown_stops_a_running_maintenance` sometimes hangs for good

## Problem
`db::tests::shutdown_stops_a_running_maintenance` (`crates/mallard-sentry-server/src/db/mod.rs`)
hung during `cargo test --workspace`: 52 minutes at 0% CPU, until it was killed. So
`cargo test` can hang without a message. If the code behind the test has the same fault, a
server could also hang while shutting down.

## How to reproduce
Seen once in three runs of the full suite, on `fix/azure-config-check`, which does not change
`db/`. Alone, the test passed 8 out of 8 times in about a second.

## Notes
Not investigated. A `sample` of the process showed the threads idle, with DuckDB's workers and
tokio's waiting, but it does not show which `await` the test is stuck on. The test has
`let _ = rx.await` with no time limit, so a `reply` that is neither answered nor dropped would
wait for ever; that is a guess.

2026-11-02: the same kind of hang was caught in another test: a deadlock in DuckDB after `SET threads`, which only the tests ran ([issue 0193](0193-red-ci-after-0192.md)). That is gone. A second way to hang, with the same symptom, is fixed in [issue 0170](0170-stop-after-invalidated-catalog-hangs.md). This test has the `rx.await` that note guessed at, so either could have been it; the test did not hang again in 32 stress runs, too few to prove much.
