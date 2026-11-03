---
status: done
type: bug
created: 2026-11-02
---
# CI on main went red: a flush lost all its retries, and a route outlived its test

## Problem
CI run 41405760215 on `b8378ba` failed in two places:

- `db::tests::ingest_continues_during_maintenance`: `ducklake_flush_inlined_data` used up DuckLake's 10 retries
  (`Conflict on update`).
- Playwright `selection stays on a row by id across a refresh`: `route.fetch: Test ended`. The route belongs to the
  test before it, `issues hold still past the first page, until r`: its last read was still under way when it ended.

## How to reproduce
The conflict did not happen here, in 1,184 runs at up to 48 at once on 18 CPUs. So the fix for it is seen working only
in its unit tests; CI has to confirm it.

The stress runs found something else: a hang. See below. It came back in 1 of 64 runs at 48 at once:

    target/debug/deps/mallard_sentry_server-* --exact db::tests::ingest_continues_during_maintenance

## Notes

### The hang: a deadlock in DuckDB when `threads` changes at run time
`sample` of a hung test showed, in `TaskScheduler::RelaunchThreads`, which runs after every query:

- the maintenance thread, after `SET threads`, holds the scheduler's `thread_lock` and is in `thread::join`, waiting for
  a worker it wants to remove;
- the workers wait in `ScanReadAhead::WaitForProgress`, it seems for a scan of the writer's (the stacks do not name it);
- the writer's nested catalog query ended and waits for `thread_lock`.

Each waits for the next, for ever. Without the `SET threads` the hang is gone, which supports this reading. This fits [issue 0158](0158-shutdown-test-hangs-sometimes.md), which saw idle
threads in a test on the same settings; that hang itself was not caught again.

`mallard serve` cannot hit this: it always passes `maintenance_threads = 0`
([issue 0120](0120-sentry-compaction-threads-in-mallard.md)), and `SET threads` otherwise runs only while opening the
connection. Only the tests set 1. So the tests ran a path the product does not, and it hung them.

**Fix:** `Host::maintenance_threads` and the `SET threads` around compaction are gone. They also went against the rule
that libraries do not change global settings on a shared connection. After that: 0 hangs in 960 runs at 48 at once.

This is a DuckDB bug. The owner chose not to report it, so the workaround stays: [ADR 0034](../../docs/adr/0034-duckdb-threads-only-at-startup.md).

### The flush conflict
`Maintainer::run_now` now tries the flush up to 3 times after a conflict, unless the server is stopping. The periodic
flush is unchanged: it can wait for its next turn.

**Tried first and dropped: the flush under `CommitLock`.** ADR 0030 measured this as "no clear gain", and it does harm:
`two_processes_on_sqlite` went from 0 of 32 failures to 25 of 32, with `database is locked` on the flush's commit.
A likely reason, not confirmed: the writer waits for the lock with its catalog transaction open. Do not try this again
without looking at that. ADR 0030 stands as it was.

### The route
The test now ends with `page.unrouteAll({ behavior: "ignoreErrors" })`, as Playwright's error message says. The other test
with `route.fetch` does too.

### What ran
`cargo test --workspace`: 680 passed. clippy and fmt clean. Playwright `e2e/app.spec.ts`: 57 passed. `screenshots.spec.ts` did
not run here. **Build the UI first** (`npm run build`, `cargo build -p mallard`): with an old UI compiled in, 4 tests
fail on features that UI does not have yet.
