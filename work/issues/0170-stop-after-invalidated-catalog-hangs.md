---
status: done
type: bug
created: 2026-11-02
---
# The server can hang for good after an invalidated catalog

## Problem
After the DuckLake catalog is invalidated, the server starts its own stop, but the process may never exit. It then
serves nothing, its port is closed, and nothing restarts it unless a health check does.

Seen once, in the run `g2h-dl-1` for [issue 0133](0133-rss-spikes-above-duckdb-limit.md): `main` at 625bf66, 2 GiB container,
1GB + 128MB, 4 threads, 1,000 events/s + 11,200 spans/s, DuckDB-file catalog. What the log shows:

- 05:25:16 `Checkpoint failed for database "__ducklake_metadata_lake"` (`failed to pin block`, 948.2/953.6 MiB), then
  `the DuckLake catalog has been invalidated and nothing can be stored; exiting`, and `writer stopped`.
- 05:25:20 OTel: `the buffered rows are lost rows=34114`, `shutting down: flushing buffered data`, `flusher stopped`.
- 05:25:36 `requests still open after the grace period; stopping without them`. This is the last line. `stopped` never came.

Almost two hours later (07:13) the process was still alive: 44 threads in `futex_wait`, RSS 415 MiB, `/healthz` not answering,
the container "unhealthy". The Sentry load generator also went on past its 30 minute duration against it.

So [issue 0152](0152-stopping-has-no-deadline.md) limited the wait for requests, but something else in
`Platform::serve_on` waits with no limit. Which part is not known: no thread stacks were taken.

## How to reproduce
`bench/mallard/memory/run.sh` with a 2 GiB container, 1GB + 128MB and 4 threads, at heavy load, until the log says
`has been invalidated`. It did not happen every time. Then see if `stopped` follows.

## Notes
Heavy load on 2 GiB is above what ADR 0025 supports; its out-of-memory errors are expected. The hang is the bug: the
stop should end the process, so a restart attaches the catalog again.

### Cause and fix (2026-11-02)
Both Sentry queues were `crossbeam_channel::bounded`. In crossbeam-channel 0.5.17 a bounded channel keeps its messages until the
last `Sender` is gone too, and `Db` holds one. So a command queued after the writer's last read, during its final flush, stayed there:
its `reply` was neither answered nor dropped, and whoever awaited it waited for ever. An unbounded channel drops its messages with
the `Receiver`. Both queues are now unbounded, with the limit in `send_below`. A few callers at once can pass the limit by a few.

The test `what_is_queued_during_the_stop_gets_an_answer` sends events during a stop until the queue says it stopped. With the
bounded queue it fails 3 of 3 (`a reply that never ends`), with the fix it passes.

**Not proven:** that this is what hung `g2h-dl-1`. It fits the log, with the writer stopped first and then a wait with no limit, but no
stacks were taken then, and the run was not repeated. The `sample` of another hang is in [issue 0193](0193-red-ci-after-0192.md); that one cannot happen in `mallard serve`.
