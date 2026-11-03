# 0034 — DuckDB's thread count is set once, at startup

## Status
accepted (by the owner, 2026-11-03). Supersedes the `maintenance_threads` consequence of ADR 0014.

## Context
Changing `threads` while another connection runs a query with a nested query in it (as a DuckLake scan has) can deadlock DuckDB
([issue 0193](../../work/issues/0193-red-ci-after-0192.md)). 3 of 192 of our stress runs hung. A later reproduction without mallard
code, with a small script, also hung on 1.4.5, 1.5.6, 2.0.0 and a 2.1.0 dev build of 2026-10-31. A hang left for 10 minutes never
recovered. Not tested: DuckDB `main` after its PR 28204, Linux, other clients and other catalogs.

`RelaunchThreads` holds the scheduler's `thread_lock` while it joins a worker it wants to remove, and it runs at the end of every
query, nested ones too. So a nested query ending inside a scan waits for that lock, and the join cannot finish because of that
scan. Four shapes of this were seen, and one hang can mix them: the worker in `ScanReadAhead::WaitForProgress`; the worker on a
mutex under the scan; the worker itself in the nested query; or the nested query's thread doing the join while `SET threads` waits
for the lock. It can also abort the process instead, with `thread::join failed: Invalid argument` (cause not worked out).

Sentry's `maintenance_threads` did this around compaction. `mallard serve` always left it at 0, so only tests could hit it.

The owner chose not to report this to DuckDB, so it may stay in later versions.

Options:

- **Never change `threads` after startup** (chosen): removes the cause, and the setting had no user.
- A lock around every query and `SET threads`: it would serialize the whole server, for a setting nobody uses.
- Wait for a fix in DuckDB: no report was made, so none may come.

## Decision
`threads` is set only in `mallard_duck::configure`, on a new DuckDB instance, before any other query runs. Nothing changes it
after that. `Maintenance.threads` and `Host::maintenance_threads` are removed.

To give a job fewer threads, use a second DuckDB instance or a separate process.

## Consequences
- 0 hangs in 960 stress runs, and this holds no matter when compaction runs.
- Compaction uses all of `duckdb.threads`. That was already so in `mallard serve`.
- The SQL endpoints accept a single `SELECT` only, so a user cannot send `SET threads` (`mallard_duck::sql`, with a test).
- After a DuckDB upgrade this ADR can be looked at again, but there is no reason to: nothing needs the setting.
