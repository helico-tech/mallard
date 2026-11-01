# 0001. DuckLake with append-only tables and a single writer

## Status
accepted (2026-10-31)

## Context
Error events must be stored cheaply on object storage (S3 by default), be
queryable with SQL for issue lists, facets and time series, and the system
must be simple to run for small-mid companies. DuckLake (DuckDB 2.0, spec
1.1) keeps metadata in a SQL catalog and data in Parquet on any storage
DuckDB supports. Updates in a lakehouse rewrite data or add delete files;
many small commits create many snapshots and small files.

## Decision
- Store everything in DuckLake. Tables are append-only; mutable issue state is
  an append-only activity log (newest row wins).
- One writer thread per process does all writes, with group commit (one
  transaction per batch); readers use cloned connections.
- Small batches are inlined in the catalog and flushed to sorted Parquet by
  time or row count; events are partitioned by `epoch_day(received_at)` and
  sorted by `(project_id, group_hash, received_at)`.
- Requests are acknowledged only after their batch committed.

## Consequences
- No write conflicts inside a process; no UPDATE/DELETE cost except retention.
- An accepted event is durable; ingest latency is about one commit (~50-150 ms).
- In-memory caches (projects, issue status) assume one process per catalog.
  Horizontal scaling needs cache refresh and duplicate-tolerant queries
  (issue 0001). Scale up first: one process handles thousands of events/s.
- All timestamps are UTC `TIMESTAMP` (the bundled DuckDB has no ICU).
