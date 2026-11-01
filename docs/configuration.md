# Configuration

`otel-ducklake` reads, in this order (later wins):

1. built-in defaults,
2. a TOML file: `--config path` or `ODL_CONFIG=path` (optional),
3. environment variables `ODL_<SECTION>__<KEY>`, e.g.
   `ODL_STORAGE__DATA_PATH=s3://bucket/otel/`, `ODL_QUERY__MAX_CONCURRENT=8`.
   Values are parsed as TOML when the setting is a number, boolean or list
   (`ODL_AUTH__API_TOKENS='["a","b"]'`), and taken literally for text
   settings.

Inside any text value, `${NAME}` is replaced by environment variable `NAME`
(start-up fails if it is not set). Use this to keep secrets out of files:

```toml
[auth]
api_tokens = ["${ODL_API_TOKEN}"]
```

Print a commented example with `otel-ducklake config`. Check a config
(attach storage, create tables, exit) with `otel-ducklake check`. The full
example is [config/otel-ducklake.example.toml](../config/otel-ducklake.example.toml).

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
| `schema` | `otel` | DuckLake schema for the tables. Lets several applications share one catalog. |
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
| `max_buffer_bytes` | `512MB` | Above this, receivers answer HTTP 429 / gRPC `RESOURCE_EXHAUSTED` (retryable). |
| `max_request_bytes` | `32MB` | Max OTLP request size after decompression. |

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
| `enabled` | `true` | Run maintenance in this process. |
| `compact_interval` | `10m` | Flush inlined rows and merge files smaller than `small_file_size`. |
| `small_file_size` | `16MB` | |
| `full_compact_interval` | `6h` | Merge files up to `storage.target_file_size`. |
| `retention_interval` | `1h` | Retention deletes, snapshot expiry and file cleanup. |
| `snapshot_retention` | `1h` | Keep old snapshots (time travel) this long. |
| `file_cleanup_delay` | `15m` | Delete unreferenced files only after this grace period, so running queries don't lose files. |
| `catalog_wide` | `true` | Run snapshot expiry and file cleanup. They affect the whole catalog; when several applications share one, enable it in exactly one. |

## `[query]`

| key | default | meaning |
|---|---|---|
| `max_concurrent` | `4` | Concurrent queries (DuckDB connections). Each query uses all `duckdb.threads`. |
| `timeout` | `30s` | Queries are interrupted after this. Also the max wait for a free connection. |
| `max_window` | `31d` | Largest allowed time window per request. |
| `enable_sql` | `false` | Enable `POST /api/v1/sql`. Requires `auth.api_tokens`. See [ADR 0005](adr/0005-sql-endpoint.md). |

## `[auth]`

| key | default | meaning |
|---|---|---|
| `api_tokens` | `[]` | Accepted `Authorization: Bearer <token>` values for the query API. Empty = no auth. `/api/v1/health`, `/openapi.json` and `/docs` are always open. |
| `ingest_tokens` | `[]` | Bearer tokens for OTLP receivers. Empty = no auth. |

For OTel SDKs set `OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer <token>"`.
Use TLS in front (ingress, load balancer, or service mesh); the server
itself speaks plain HTTP/gRPC.

## `[duckdb]`

| key | default | meaning |
|---|---|---|
| `memory_limit` | DuckDB default (80% of RAM) | Set to ~70% of the container memory limit, e.g. `1400MB` for 2 GiB. |
| `threads` | all cores | DuckDB worker threads. |
| `temp_directory` | DuckDB default | Spill location for large queries. |
| `extension_directory` | `~/.duckdb/extensions` | Where extensions are installed. Images use `/opt/duckdb/extensions`. |
| `autoinstall_extensions` | `true` | Download missing extensions at start-up. Images set `false` and ship them pre-installed. |

## Environment variables for the clients

| variable | used by | meaning |
|---|---|---|
| `ODL_URL` | `odl` | Query API base URL (default `http://localhost:8080`). |
| `ODL_TOKEN` | `odl` | API bearer token. |
| `OTLP_ENDPOINT` | `odl-loadgen` | OTLP/HTTP base URL (default `http://localhost:4318`). |
| `ODL_INGEST_TOKEN` | `odl-loadgen` | Ingest bearer token. |
