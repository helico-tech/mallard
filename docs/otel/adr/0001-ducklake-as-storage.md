# 0001. DuckLake (DuckDB) as the storage engine

Date: 2026-11-01

## Status

accepted

## Context

We need storage for traces, logs and metrics for small and mid-sized companies
(thousands to tens of thousands of visitors a day, i.e. tens to a few
thousand spans per second at peak). Requirements: cheap to run, data in open
formats, works on a single machine *and* on object storage, fast analytical
queries, retention without heavy operations.

Options:
- **ClickHouse**: very fast, but a separate stateful server to operate.
  Storage is its own format, not open files.
- **Elasticsearch/OpenSearch**: heavy and expensive at rest.
- **Plain Parquet files + DuckDB**: open and cheap, but no transactions, no
  file catalog (listing S3 for every query), no deletes or compaction.
- **Iceberg/Delta**: open table formats, but the Rust writers are less mature
  and they need a separate catalog service (Iceberg REST) or JSON log files.
- **DuckLake 1.1** (DuckDB 2.0): an open table format whose metadata lives
  in a normal SQL database (DuckDB file, SQLite or PostgreSQL) and whose data
  is Parquet anywhere DuckDB can write (local, S3, GCS, Azure, R2, ...).
  Transactions, snapshots, partitioning, sorting, compaction, data inlining,
  and per-file column statistics in the catalog.

## Decision

Embed DuckDB 2.0.0 (`duckdb` crate `=1.20000.1`, bundled) in the server and
store everything in a DuckLake 1.1 catalog. The server is the only required
component; the catalog database is a file by default and PostgreSQL for
multi-replica setups.

## Consequences

- One binary, no extra services for a single-node install.
- Data is Parquet plus SQL metadata: any DuckDB can query it directly, and
  backups are file copies plus a catalog dump.
- DuckDB extensions (`ducklake`, `httpfs`, `postgres`, ...) are loaded at
  run time. Container images pre-install them (`otel-ducklake
  install-extensions`) so they work offline.
- DuckDB must be built without `DEBUG` (see the workspace `Cargo.toml`):
  debug builds abort when loading release-built extensions.
- A DuckDB-file or SQLite catalog allows exactly one process. Horizontal
  scaling needs PostgreSQL as the catalog.
- We depend on DuckLake's maintenance functions (merge, expire, cleanup), and
  run them ourselves on a schedule.
