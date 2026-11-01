# Operations

## Health and monitoring

- `GET /api/v1/health`: liveness/readiness (no auth).
- `mallard telemetry status` / `GET /api/v1/stats`: requests, accepted, throttled and
  invalid items per signal, and `throttled_requests` (requests refused before their body was read); buffer fill; flush count, errors and duration; rows, files,
  bytes and inlined rows per table; last maintenance run.
- Prometheus (mallard's `/metrics`): `mallard_otel_ingest_requests_total`,
  `mallard_otel_ingest_items_total` (label `signal`),
  `mallard_otel_ingest_rejected_items_total` (labels `signal`, `reason` =
  `throttled` | `invalid`), `mallard_otel_ingest_throttled_requests_total` (label
  `signal`), `mallard_otel_buffered_rows`, `mallard_otel_buffered_bytes`, `mallard_otel_flushes_total`,
  `mallard_otel_flush_seconds`, `mallard_otel_flush_rows`, `mallard_otel_flush_errors_total`,
  `mallard_otel_dropped_rows_total` (label `table`), `mallard_otel_maintenance_seconds` and
  `mallard_otel_maintenance_errors_total` (label `task`).
- mallard's `/readyz` fails while the last flush failed or the catalog does
  not answer.
- Logs: `RUST_LOG=info` (default). Flush failures are logged at `error`,
  maintenance failures at `warn`. Use `server.log_format = "json"` in
  clusters.

What to watch:

| signal | meaning | action |
|---|---|---|
| `throttled` grows, 429s at exporters | buffer full: storage slower than ingest, or a burst. Exporters retry, so data arrives late, not lost | check `last_flush_error`; raise `max_buffer_bytes`; check object store / catalog latency |
| `throttled_requests` grows, `throttled` does not | requests refused before their body was read: too many bytes in flight (see `max_buffer_bytes` in [configuration](configuration.md)). Their items are unknown, so only the request is counted; it is also in `requests`. Exporters retry | many large or slow concurrent requests (gRPC and compressed HTTP reserve `max_request_bytes` each): raise `max_buffer_bytes`, or lower exporter concurrency / batch size |
| `invalid` grows | items refused for good: spans without a valid trace/span id (OTLP partial success), or a request too large for `max_buffer_bytes` | fix the sender; see the partial-success message it gets back |
| `flush_errors` grows | catalog or object store unreachable / credentials | see logs; data is retried, not dropped |
| `dropped_rows` grows | a table refuses our rows (permanent error, e.g. a column changed by hand); other tables are still written | see the `error` log line "dropping a batch" and `mallard_otel_dropped_rows_total{table}`; restore the table's columns (the server adds missing ones at start-up), or report it as a bug |
| many `files` per table | compaction not keeping up or disabled | check `last_maintenance`; lower `compact_interval` |
| `inlined_rows` keeps growing | compaction not running | `maintenance.enabled` |
| queries return 503 | all `query.max_concurrent` connections busy | raise it, or reduce dashboard refresh rates |

## One-shot maintenance

`mallard maintenance [--full]` runs both sides' maintenance once and
exits: Sentry's flush, retention and compaction, then otel's flush and merge
of small files (`--full`: all files below the target size), retention, snapshot
expiry and file cleanup. Stop the server first with a DuckDB-file or SQLite
catalog (a running server holds the file; the command then fails). Handy to
compact a backfilled lake before timing queries.

It ignores the lease below, so it also works while a pod holds it. Next to
running pods on Postgres it can lose a conflict against their maintenance; it
then exits with an error and is safe to run again.

## Several replicas

Replicas on one Postgres catalog elect one maintainer, as the Sentry side does
([ADR 0021](../adr/0021-sentry-several-processes.md)): before each task a replica
tries the `maintenance_lease` row in `otel.meta`. Only the holder runs compaction,
retention, snapshot expiry and file cleanup; for the others the task is skipped
until its next interval.

- The lease is valid for 3 of the shortest maintenance interval (30 minutes by
  default). After a holder stops, or restarts under a new identity, maintenance
  pauses until the lease runs out and the next task of another replica (or of the
  restarted process) takes it.
- `last_maintenance` in `/api/v1/stats` is per process: it stays empty on a replica
  that never held the lease. Check all replicas before you conclude nothing runs.
- A task that outlives the lease can overlap with the next holder. That is safe:
  the loser logs a failed task and the next run tries again.

## Sizing

See [capacity.md](capacity.md). Short version: 1-2 vCPU and 2 GiB RAM
handle small/mid traffic with large headroom. Keep container memory ≥
`duckdb.memory_limit` + `ingest.max_buffer_bytes` + 1 GB.

## Retention and disk usage

Disk ≈ rows/day × bytes/row × retention days. Measured about 30 bytes per
span and per log record with varied synthetic attributes
([capacity.md](capacity.md)); plan for 2-3× with real data. Example:
10 M spans/day × 60 B × 7 days ≈ 4 GB.

Configuration is read at start-up: after changing `[retention]`, restart;
the next retention run (hourly) applies it.

## Backups

The lake is (a) the catalog database and (b) the Parquet files. Files are
never modified in place, only added and (after expiry) deleted.

- **DuckDB / SQLite catalog**: stop the server (or snapshot the volume), copy
  the catalog file and the data directory.
- **PostgreSQL catalog**: `pg_dump` the catalog database, and use bucket
  versioning or replication for the data path. Restore the dump, then make
  sure the files it references still exist. Keep `file_cleanup_delay` longer
  than your backup window if you copy files after the dump.

## Upgrades

- Tables are migrated on start-up: new columns are added, existing data stays.
- The catalog format follows DuckLake (1.1). A DuckLake spec upgrade will be
  documented in the release notes; DuckLake requires explicit migration
  (`AUTOMATIC_MIGRATION`), which mallard will not run silently.
- Rolling upgrades with a PostgreSQL catalog: rows are appended by column
  name, so a replica of the previous version keeps writing after a newer one
  has added columns (those columns stay NULL for its rows), and an older
  binary starts against a table with extra columns (it logs a warning). A
  release that changes or removes columns would say so in its notes.
- Duplicates are possible in one rare case: the catalog commits a flush but
  the connection breaks before the server sees the reply. The flush is then
  retried (at-least-once). Nothing removes such duplicates.

## Querying the raw data

See "Opening the lake without the server" in [data-model.md](data-model.md).
With a DuckDB-file catalog, stop the server first (one writer per file), or
copy the catalog file and open the copy read-only.

## Troubleshooting

| symptom | cause / fix |
|---|---|
| `DATA_PATH parameter ... does not match existing data path` | `storage.data_path` differs from the path stored when the catalog was created. Fix the config, or move data and attach once with `OVERRIDE_DATA_PATH`. |
| `extension ... is not installed and duckdb.autoinstall_extensions = false` | image built without that extension: `mallard install-extensions --dir <dir>` at build time. |
| `Could not set lock on file` | a DuckDB-file catalog is open in another process. Only one process may use it; switch to PostgreSQL for several. |
| HTTP 415 from `/v1/...` | send `Content-Type: application/x-protobuf` or `application/json`. |
| 401 | missing or wrong bearer token (`auth.*_tokens`). |
| empty results right after sending | data is visible after the next flush (`ingest.flush_interval`, 5 s). |
| `query exceeded the 30s time limit` | narrow the window or filters; raise `query.timeout`. |
