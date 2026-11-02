---
status: done
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

With the defaults of [issue 0123](0123-chart-memory-defaults-below-the-rule.md) this did not happen at
medium or heavy load. It still happens when compaction runs out of memory, as it did at full speed (run
`g-sat`: 404 rejected).

## How to reproduce
`bench/mallard/memory/run.sh` with a 768MB limit and `SIDES=sentry`, then grep the log for
`batch failed`.

## Notes
Probably: retry out-of-memory errors in the writer the same way as conflicts, with the same bound.

## What changed
The writer tries a batch again when memory is short (`mallard_duck::is_out_of_memory`, the list of issue
0131), with the bound of conflicts: 7 times, 20 ms doubling, about 2.5 s; then the batch fails as before.
The whole batch, not only the commit as the note above suggested: with a real allocation error the first
statement to fail is reading the issues, before the transaction. Project, status, bundle and retention
writes retry in `in_tx` too. New metric: `mallard_sentry_write_memory_retries_total`.

Clearing the staging table after the commit could fail the batch, so a retry would have stored committed
events twice; it now only logs (the next batch clears it too). That path has no test of its own.

Test: `short_memory_does_not_fail_a_batch` (real allocation errors from a low `memory_limit`). Before the fix
the batch failed at once. `bench/mallard/memory` was not run again.
