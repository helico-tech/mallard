---
status: done
type: debt
created: 2026-11-01
---
# One table with a permanent write error blocks all signals

## Problem
All tables are flushed in one transaction. A permanent error on one table (e.g. a hand-edited incompatible column type) blocks traces, logs and metrics until fixed, and the buffer fills. Detect permanent errors and park the affected table's batches.

## Notes
Found during the v0.1 build and review (epic 0001).

Fixed (2026-11-01) in `odl-server/src/store/writer.rs`. A failed flush now
says which table it was appending to. Errors are classified: commit errors
and append errors naming a transient cause (IO/HTTP/connection/memory/
interrupt/timeout/conflict) are retried with back-off and never dropped;
any other append error is permanent. On a permanent error the table's
batches are written one by one at once, those that still fail permanently
are dropped (`error` log with the table, `dropped_rows`,
`odl_dropped_rows_total{table}`), and the other tables are flushed without
waiting. This replaces the old "after 3 failures, drop what fails while
others succeed" heuristic, which could drop data on a transient error and
kept everything when only the broken table had data. Appender errors often
have no DuckDB error type, hence a list of transient causes rather than of
permanent ones (docs/otel/architecture.md). Dropped, not parked: parking
would hold memory for a table that needs a manual fix. Tests:
`a_broken_table_does_not_block_the_others` (column renamed by hand; spans
keep flushing, no back-off), `poison_batch_is_dropped_and_the_rest_written`,
`classifies_flush_errors`.
