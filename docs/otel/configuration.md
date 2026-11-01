# Configuration

`mallard serve` reads, in this order (later wins):

1. built-in defaults,
2. a TOML file: `-c path` or `MALLARD_CONFIG=path` (optional),
3. environment variables `MALLARD_<SECTION>__<KEY>`, e.g.
   `MALLARD_STORAGE__DATA_PATH=s3://bucket/otel/`, `MALLARD_OTEL__QUERY__MAX_CONCURRENT=8`.
   Values are parsed as TOML when the setting is a number, boolean or list
   (`MALLARD_SERVER__API_TOKENS='["a","b"]'`, or comma separated: `a,b`), and
   taken literally for text settings (`MALLARD_OTEL__RETENTION__TRACES=0`).
   Unknown keys are errors.

Inside any text value, `${NAME}` is replaced by environment variable `NAME`
after the file is parsed, so the value may contain quotes (start-up fails if
it is not set). Use this to keep secrets out of files:

```toml
[server]
api_tokens = ["${MALLARD_API_TOKEN}"]
```

The keys below were written for the old standalone binary. In mallard they
live under `[otel.*]` (`ingest`, `retention`,
`maintenance`, `query`); `[server]`, `[storage]`, `[duckdb]` and the tokens
are mallard's. See [config/mallard.example.toml](../../config/mallard.example.toml)
and check a config with `mallard check --connect`.

Durations: `500ms`, `5s`, `10m`, `6h`, `7d`, `1h30m`; `0` means "off" or
"forever" where noted. Sizes: `512MB`, `2GB` (powers of 1000) or `1GiB`
(powers of 1024).

## `[server]`

| key | default | meaning |
|---|---|---|
| `otlp_grpc_addr` | `0.0.0.0:4317` | OTLP/gRPC listen address |
| `otlp_http_addr` | `0.0.0.0:4318` | OTLP/HTTP listen address |
| `api_addr` | `0.0.0.0:8080` | Query API listen address (`/api/v1`, `/openapi.json`, `/docs`) |
| `log_format` | `text` | `text` or `json` (log level via `RUST_LOG`, default `info`) |

## `[storage]`

| key | default | meaning |
|---|---|---|
| `catalog` | `data/catalog.ducklake` | DuckLake catalog: a DuckDB file path, `sqlite:<path>`, or `postgres:<libpq connection string>`. See [storage-backends.md](storage-backends.md). |
| `data_path` | `data/lake/` | Where Parquet files go: a local directory or `s3://`, `gs://`, `az://`, `abfss://`, `r2://`, ... It must match the path stored in an existing catalog. |
| `secrets` | `[]` | List of DuckDB secrets (credentials), see below. |
| `extensions` | `[]` | Extra DuckDB extensions to load. Needed ones are loaded automatically. |
| `data_inlining_row_limit` | `1000` | Inserts below this many rows are stored in the catalog until the next compaction. `0` turns inlining off. |
| `target_file_size` | `128MB` | Size compaction aims for. |
| `row_group_size` | `122880` | Parquet row group size in rows. |

### Secrets

Each `[[storage.secrets]]` table becomes a DuckDB
[`CREATE SECRET`](https://duckdb.org/docs/stable/configuration/secrets_manager).
`type` and `provider` are keywords. Other keys are passed as options,
strings quoted:

```toml
[[storage.secrets]]
type = "s3"
provider = "credential_chain"     # AWS env vars, ~/.aws, IRSA / instance profile

[[storage.secrets]]
name = "minio"
type = "s3"
key_id = "${S3_ACCESS_KEY_ID}"
secret = "${S3_SECRET_ACCESS_KEY}"
endpoint = "minio:9000"
url_style = "path"
use_ssl = false
scope = "s3://otel"               # optional: only use for this prefix
```

## `[ingest]`

| key | default | meaning |
|---|---|---|
| `flush_interval` | `5s` | Max time data waits in memory. Also the max loss on a hard crash. |
| `flush_rows` | `200000` | Flush early at this many buffered rows. |
| `max_buffer_bytes` | `512MB` | Above this, receivers answer HTTP 429 / gRPC `UNAVAILABLE` (retryable). Request bodies being received and decoded have a budget of the same size (a gRPC request or a compressed HTTP body reserves `max_request_bytes`); when it is used up, new requests get the same retryable answer. |
| `max_request_bytes` | `32MB` | Max OTLP request size after decompression. A request whose in-memory form would exceed `max_buffer_bytes` (resource attributes repeat on every row) is refused with 413 / `INVALID_ARGUMENT`. |

## `[retention]`

| key | default | meaning |
|---|---|---|
| `traces` | `7d` | Keep spans this long. `0` = forever. |
| `logs` | `7d` | |
| `metrics` | `30d` | |

Retention of one day or more is applied per whole UTC day: data is kept at
least the given time and at most one day longer. Expired days are dropped
without rewriting files.

## `[maintenance]`

| key | default | meaning |
|---|---|---|
| `enabled` | `true` | Run maintenance in this process. Replicas sharing a catalog elect one by a lease ([operations](operations.md#several-replicas)). |
| `compact_interval` | `10m` | Flush inlined rows and merge files smaller than `small_file_size`. |
| `small_file_size` | `16MB` | |
| `full_compact_interval` | `6h` | Merge files up to `storage.target_file_size`. |
| `retention_interval` | `1h` | Retention deletes, snapshot expiry and file cleanup. |
| `snapshot_retention` | `1h` | Keep old snapshots (time travel) this long. |
| `file_cleanup_delay` | `15m` | Delete unreferenced files only after this grace period, so running queries don't lose files. |

## `[query]`

| key | default | meaning |
|---|---|---|
| `max_concurrent` | `4` | Concurrent queries (DuckDB connections). Each query uses all `duckdb.threads`. |
| `timeout` | `30s` | Queries are interrupted after this. Also the max wait for a free connection. |
| `max_window` | `31d` | Largest allowed time window per request. |
| `enable_sql` | `false` | Enable `POST /api/v1/sql`. Requires `auth.api_tokens`. Treat these tokens as admin access: queries are read-only and limited to the lake tables, but their memory and CPU use is bounded only by `timeout` and `duckdb.memory_limit`, not fully. See [ADR 0005](adr/0005-sql-endpoint.md). |

## `[auth]`

| key | default | meaning |
|---|---|---|
| `api_tokens` | `[]` | Accepted `Authorization: Bearer <token>` values for the query API. Empty = no auth. `/api/v1/health`, `/openapi.json` and `/docs` are always open. |
| `ingest_tokens` | `[]` | Bearer tokens for OTLP receivers. Empty = no auth. |

An empty string in either list is refused at start-up (it usually means a
secret or environment variable was not set). Intervals must be greater than 0.

For OTel SDKs set `OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer <token>"`.
Use TLS in front (ingress, load balancer, or service mesh); the server
itself speaks plain HTTP/gRPC.

## `[duckdb]`

| key | default | meaning |
|---|---|---|
| `memory_limit` | DuckDB default (80% of RAM) | Always set it in containers. Process memory peaks at about `memory_limit` + `ingest.max_buffer_bytes` + 0.5 GB, e.g. `1GB` with a 256 MB buffer in 2 GiB. |
| `threads` | all cores | DuckDB worker threads. |
| `temp_directory` | DuckDB default | Spill location for large queries. |
| `extension_directory` | `~/.duckdb/extensions` | Where extensions are installed. Images use `/opt/duckdb/extensions`. |
| `autoinstall_extensions` | `true` | Download missing extensions at start-up. Images set `false` and ship them pre-installed. |

## Environment variables for the clients

| variable | used by | meaning |
|---|---|---|
| `MALLARD_URL` | `mallard` | Query API base URL (default `http://localhost:8080`). |
| `MALLARD_TOKEN` | `mallard` | API bearer token. |
| `OTLP_ENDPOINT` | `mallard-otel-loadgen` | OTLP/HTTP base URL (default `http://localhost:4318`). |
| `MALLARD_INGEST_TOKEN` | `mallard-otel-loadgen` | Ingest bearer token. |
