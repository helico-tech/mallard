---
status: todo
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
