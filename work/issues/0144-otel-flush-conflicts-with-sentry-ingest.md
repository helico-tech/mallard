---
status: todo
type: question
created: 2026-11-01
---
# OTLP ingest is much slower next to saturated Sentry ingest

## Problem
Seen while filling a lake for story 0004/01 (`mallard serve`, 10 DuckDB threads, DuckDB-file
catalog, local disk): `mallard-otel-loadgen --backfill` reached 15,000-90,000 spans/s while
`mallard-sentry-loadgen ingest --rate 0 --concurrency 512` ran at 3,000-4,000 events/s, and
300,000-385,000 spans/s as soon as that finished. The server log has 55 of

    flush conflicted with another transaction; will retry
    Failed to flush changes into DuckLake: Duplicate key "snapshot_id: N" violates primary key constraint

The flushes retried and `flush_errors` stayed 0, but the buffer filled: 5,843 throttled and 71
failed requests at the load generator. Their cause was not looked into.

Paced traffic does not show it: 2,242 spans/s with 121 events/s had nothing throttled.

## How to reproduce
Run both load generators at once, as above, and compare with OTLP alone.

## Notes
Not measured with a Postgres catalog. Each side commits on its own, so this may be the price of
one catalog (ADR 0001), or a retry that waits too long. Worth knowing before
`docs/otel/capacity.md`, whose numbers come from the old binary without a Sentry side, is
measured again.
