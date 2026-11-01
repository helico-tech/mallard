---
status: todo
type: bug
created: 2026-11-01
---
# A failed Sentry compaction takes events down with it

## Problem
Compaction and the writer share one `duckdb.memory_limit`. When a merge uses it all, the writer's
next commit gets an out-of-memory error too. The batch fails and every waiting client gets 503.
The writer retries conflicts (ADR 0014), not this. The memory comes back within seconds.

Measured on 2026-11-01 (`bench/mallard/memory`): with a limit the merge does not fit, one failed
compaction cost about 200-250 events each time (run `b-sentry`: 256 rejected, 1 batch; run
`d-mod`: 1124 rejected, 5 batches). As far as we know Sentry SDKs do not resend a 503, so these events are likely lost.

With the settings of [issue 0123](0123-chart-memory-defaults-below-the-rule.md) and 16 MB target
files this did not happen in the measured runs. It still happens when compaction runs out of
memory for any other reason.

## How to reproduce
`bench/mallard/memory/run.sh` with a 768MB limit and `SIDES=sentry`, then grep the log for
`batch failed`.

## Notes
Probably: retry out-of-memory errors in the writer the same way as conflicts, with the same bound.
