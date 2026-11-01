# Operations

## Health and monitoring

- `GET /api/v1/health`: liveness/readiness (no auth).
- `odl status` / `GET /api/v1/stats`: requests, accepted and rejected items
  per signal; buffer fill; flush count, errors and duration; rows, files,
  bytes and inlined rows per table; last maintenance run.
- Prometheus (mallard's `/metrics`): `odl_ingest_requests_total`,
  `odl_ingest_items_total`, `odl_ingest_rejected_items_total` (label
  `signal`), `odl_buffered_rows`, `odl_buffered_bytes`, `odl_flushes_total`,
  `odl_flush_seconds`, `odl_flush_rows`, `odl_flush_errors_total`,
  `odl_dropped_rows_total`, `odl_maintenance_seconds` and
  `odl_maintenance_errors_total` (label `task`).
- mallard's `/readyz` fails while the last flush failed or the catalog does
  not answer.
- Logs: `RUST_LOG=info` (default). Flush failures are logged at `error`,
  maintenance failures at `warn`. Use `server.log_format = "json"` in
  clusters.

What to watch:

| signal | meaning | action |
|---|---|---|
| `rejected` grows, 429s at exporters | buffer full: storage slower than ingest, or a burst | check `last_flush_error`; raise `max_buffer_bytes`; check object store / catalog latency |
| `flush_errors` grows | catalog or object store unreachable / credentials | see logs; data is retried, not dropped |
| `dropped_rows` grows | a batch could not be written while other data could | see the `error` log line "dropping a batch"; report it as a bug |
| many `files` per table | compaction not keeping up or disabled | check `last_maintenance`; lower `compact_interval` |
| `inlined_rows` keeps growing | compaction not running | `maintenance.enabled` |
| queries return 503 | all `query.max_concurrent` connections busy | raise it, or reduce dashboard refresh rates |

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
  (`AUTOMATIC_MIGRATION`), which otel-ducklake will not run silently.
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
| `extension ... is not installed and duckdb.autoinstall_extensions = false` | image built without that extension: `otel-ducklake install-extensions <name>` at build time. |
| `Could not set lock on file` | a DuckDB-file catalog is open in another process. Only one process may use it; switch to PostgreSQL for several. |
| HTTP 415 from `/v1/...` | send `Content-Type: application/x-protobuf` or `application/json`. |
| 401 | missing or wrong bearer token (`auth.*_tokens`). |
| empty results right after sending | data is visible after the next flush (`ingest.flush_interval`, 5 s). |
| `query exceeded the 30s time limit` | narrow the window or filters; raise `query.timeout`. |
