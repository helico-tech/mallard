# Deployment

> **Retired binary.** This page was written for the old standalone binary; its
> flags and chart are gone (story 0002/06). Names are updated, the rest waits for
> [issue 0016](../../work/issues/0016-per-side-docs-for-mallard.md). Deploy mallard: `deploy/` and the
> [repo README](../../README.md). The storage and multi-replica notes below
> still apply to the Sentry side.

mallard is one binary (`mallard`) with two moving parts around it:

- **Storage**: a bucket (or local folder) for Parquet data files and
  sourcemap bundles: `storage.data_path` (DuckLake data) and
  `storage.artifacts_path` (sourcemaps, in `<path>/artifacts/`).
- **Catalog**: the DuckLake metadata database, `storage.catalog`. Either a DuckDB
  file (one process only) or PostgreSQL. It also holds recent small batches
  of events until they are flushed into Parquet (every `MALLARD_SENTRY__MAINTENANCE__FLUSH_INTERVAL`).

The catalog and the bucket belong together: the catalog says which files
exist; without it the Parquet files cannot be read as tables.

Assets in this repo:

| Path | What |
|---|---|
| `deploy/docker/Dockerfile` | Image: Debian slim, non-root (uid 10001), all DuckDB extensions pre-installed (no internet needed at run time). |
| `deploy/compose/docker-compose.yml` | Local stack: server + RustFS (S3) + PostgreSQL catalog. |
| `deploy/helm/mallard/` | Helm chart. |

## Docker

```sh
docker build -f deploy/docker/Dockerfile -t mallard .

# Everything on a local volume (DuckDB catalog + local storage):
docker run -d --name mallard -p 8080:8080 -v mallard-data:/data \
  -e MALLARD_SERVER__API_TOKENS=change-me \
  -e MALLARD_SERVER__PUBLIC_URL=http://localhost:8080 \
  -e MALLARD_STORAGE__ARTIFACTS_PATH=/data/artifacts \
  mallard
```

Image facts:

- Working directory and `HOME` is `/data`; the default catalog is
  `/data/data/catalog.ducklake`. DuckDB writes spill files to `/data/.tmp`.
  Mount a volume on `/data`.
- Works with `--read-only` (needs a writable `/data`; add `--tmpfs /tmp`).
- `MALLARD_DUCKDB__EXTENSION_DIRECTORY=/opt/duckdb/extensions` is set; the extensions there
  match the image's platform. Do not override it.
- Logs are text; `MALLARD_SERVER__LOG_FORMAT=json` makes them JSON. Set `RUST_LOG` to change the level.
- Built-in `HEALTHCHECK` calls `/healthz`.
- On `SIGTERM` the server commits queued events and flushes inlined data.
  Give it time: `docker stop -t 60`.

## Docker Compose

```sh
docker compose -f deploy/compose/docker-compose.yml up -d --build
TOKEN=dev-token
curl -s -XPOST -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  localhost:8080/sentry/v1/projects -d '{"name":"web"}'   # -> {"dsn": "http://<key>@localhost:8080/sentry/1", ...}
```

The stack: RustFS (S3-compatible store; why RustFS is explained at the top
of the compose file), a one-shot bucket creation step, PostgreSQL as the
catalog, and the server with data under `s3://sentry/errors`.

Simpler mode (DuckDB-file catalog on the `mallard-data` volume, data still on S3):

```sh
MALLARD_CATALOG=/data/catalog.ducklake docker compose -f deploy/compose/docker-compose.yml up -d
```

Override `MALLARD_PORT`, `MALLARD_API_TOKEN`, `S3_ACCESS_KEY`, `S3_SECRET_KEY`,
`POSTGRES_PASSWORD` (in the shell or a `.env` file next to the compose file)
before you expose it anywhere. `docker compose ... down -v` deletes all data.

## Helm

```sh
helm install errors deploy/helm/mallard \
  --set publicUrl=https://sentry.example.com \
  --set apiToken=$(openssl rand -hex 24) \
  --set storage.url=s3://my-bucket/sentry \
  --set storage.credentials.existingSecret=sentry-s3   # AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY
```

`NOTES.txt` (printed after install) shows how to read the token and create a
project to get a DSN. Key values (see `values.yaml` for all):

| Value | Meaning |
|---|---|
| `publicUrl` | Required. Base URL SDKs use; it ends up in every DSN. |
| `apiToken` / `existingSecret` + `existingSecretKey` | Required. API token(s). |
| `storage.url` | `s3://`, `gs://`, `az://` or a local path. |
| `storage.s3.endpoint`, `.region`, `.pathStyle` | For non-AWS S3. |
| `storage.credentials.existingSecret` | Secret exported as env vars (`AWS_*`, `GCS_HMAC_*`, `AZURE_STORAGE_CONNECTION_STRING`). |
| `storage.credentials.env` | Same, but the chart creates the Secret. |
| `serviceAccount.annotations` | IRSA / workload identity instead of keys. |
| `catalog.type` | `duckdb` (PVC, 1 pod, `Recreate`) or `postgres` (stateless pods). |
| `catalog.postgres.connectionString` / `.existingSecret` | libpq string, e.g. `host=pg port=5432 dbname=ducklake user=mallard password=... sslmode=require`. |
| `retentionDays` | Delete events older than this (0 = keep). |
| `duckdb.memoryLimit` | DuckDB memory cap; keep it about half of the memory limit. |
| `extraEnv` | Any other `MALLARD_*` setting. |
| `ingress.*`, `service.*`, `serviceMonitor.*`, `podDisruptionBudget.*` | As usual. |

Built in: non-root pod, read-only root filesystem (an `emptyDir` or the PVC
on `/data`, an `emptyDir` on `/tmp`), all capabilities dropped, startup probe
on `/healthz` (catalog attach can take a while), liveness on `/healthz`,
readiness on `/readyz`, `terminationGracePeriodSeconds: 90` for the flush on
stop. The chart refuses to render without `publicUrl` and a token, and
refuses a local storage path with the Postgres catalog.

Ingress notes:

- Browser SDKs post cross-origin. The server sends the CORS headers itself;
  do not add CORS annotations (duplicate headers make browsers reject the
  response).
- Sourcemap uploads send up to 33 MiB per request and envelopes up to
  `MALLARD_SENTRY__INGEST__MAX_BODY_BYTES` (50 MiB). Raise the proxy body limit, e.g.
  `nginx.ingress.kubernetes.io/proxy-body-size: 64m`.
- `/metrics`, `/docs` and `/openapi.json` are on the same port and need no
  token. Block `/metrics` at the ingress if it must not be public, or serve
  it elsewhere (`MALLARD_SERVER__METRICS_ADDR`, which the Helm chart sets to port
  9090).
- If the ingress sets `X-Forwarded-For`, set `MALLARD_SENTRY__INGEST__TRUST_FORWARDED_FOR=true`
  (via `extraEnv`) so client IPs are recorded correctly. The server uses the
  last address in the header (the one the ingress adds), so this is right
  for exactly one proxy hop.
- When the server is mounted below a path, put that path in `publicUrl`
  (`https://host/sentry`) and route the prefix with a rewrite.

## Configuration reference

Every setting has an environment variable. The most used:

| Variable | Default | Meaning |
|---|---|---|
| `MALLARD_SERVER__API_TOKENS` | (none, required) | Bearer token(s) for `/v1` and sourcemap uploads, comma separated. |
| `MALLARD_SERVER__PUBLIC_URL` | `http://localhost:8080` | Base URL; DSNs use `<url>/sentry`. |
| `MALLARD_STORAGE__DATA_PATH` | `data/lake/` | Parquet data: `s3://bucket/prefix/`, `gs://...`, `az://...`, local path. |
| `MALLARD_STORAGE__ARTIFACTS_PATH` | (off) | Sourcemap bundles, outside the data path. Empty: upload answers 501. |
| `MALLARD_STORAGE__CATALOG` | `data/catalog.ducklake` | DuckDB file path, `postgres:<libpq>`, `sqlite:<path>`, `mysql:<string>`. |
| `[[storage.secrets]]` | | Object storage credentials, endpoint and region (file only, see [Storage](#storage)). |
| `MALLARD_SENTRY__RETENTION__EVENTS` | `90d` | 0 = keep forever. |
| `MALLARD_SENTRY__MAINTENANCE__FLUSH_INTERVAL` | `5m` | How often inlined data becomes Parquet. |
| `MALLARD_SENTRY__MAINTENANCE__COMPACT_INTERVAL` | `1h` | Compaction, retention, snapshot expiry. |
| `MALLARD_DUCKDB__MEMORY_LIMIT` | 80% of RAM | e.g. `1GB` (`1GiB` = powers of 1024). |
| `MALLARD_DUCKDB__THREADS` | CPUs | DuckDB threads. |
| `MALLARD_SENTRY__QUERY__READERS` | `4` | Read connections for the query API. |
| `MALLARD_SENTRY__QUERY__ENABLE_SQL` | `false` | `POST /v1/sql`: read-only SQL over `events`, `issues`, `issue_activity` (ADR 0011). Tokens become admin access. `MALLARD_SENTRY__QUERY__SQL_TIMEOUT` limits a query (`30s`). |
| `MALLARD_SENTRY__INGEST__BATCH_MAX` / `MALLARD_SENTRY__INGEST__BATCH_WINDOW` | `2000` / `500ms` | Max events per commit / how long a quiet server collects before committing (200+ queued events commit at once). |
| `MALLARD_SENTRY__INGEST__CONCURRENCY` | `256` | Ingest requests processed at once (bounds request memory). |
| `MALLARD_SENTRY__MAINTENANCE__FLUSH_ROWS` | `50000` | Also flush inlined rows to Parquet after this many (bounds catalog size). |
| `MALLARD_OTEL__MAINTENANCE__SNAPSHOT_RETENTION` | `1h` | Keep snapshots and replaced files this long; set above your catalog backup age. |
| `MALLARD_DUCKDB__TEMP_DIRECTORY` | DuckDB default | DuckDB spill directory. |
| `MALLARD_SERVER__METRICS_ADDR` | (main port) | Serve `/metrics` on a separate address, e.g. `0.0.0.0:9090`. |
| `MALLARD_SENTRY__INGEST__QUEUE_CAPACITY` | `20000` | Queued events before ingest answers 429. |
| `MALLARD_SENTRY__INGEST__MAX_EVENT_BYTES` / `MALLARD_SENTRY__INGEST__MAX_BODY_BYTES` | 1 MiB / 20 MiB | Size limits (after decompression), e.g. `2MiB` or a number of bytes. Requests with the key only inside the envelope are limited to 2 MiB. |
| `MALLARD_SENTRY__SOURCEMAPS__CACHE_SIZE` | `256MiB` | Sourcemap cache. |
| `MALLARD_SENTRY__INGEST__TRUST_FORWARDED_FOR` | `false` | Use the last `X-Forwarded-For` address as client IP. |
| `MALLARD_SERVER__HTTP_ADDR` | `0.0.0.0:8080` | Listen address. |
| `MALLARD_SERVER__LOG_FORMAT`, `RUST_LOG` | `text` | Logging. |

`config/mallard.example.toml` lists all of them. The tables live in the
DuckLake schema `sentry` of the catalog attached as `lake` (both fixed). The
library settings are the `[sentry.*]` sections; the host settings (URL, tokens,
storage, DuckDB) come from `[server]`, `[storage]` and `[duckdb]`.

## Storage

Prefer a dedicated bucket (or prefix) per deployment. The server needs list,
read, write and delete on `<prefix>/*`.

### AWS S3

```toml
[storage]
data_path = "s3://my-bucket/lake/"
artifacts_path = "s3://my-bucket/artifacts"
[[storage.secrets]]
type = "s3"
region = "eu-west-1"
key_id = "${AWS_ACCESS_KEY_ID}"            # optional session_token
secret = "${AWS_SECRET_ACCESS_KEY}"
```

With `provider = "credential_chain"` instead of keys, the server uses the AWS credential chain (DuckDB's
`credential_chain` and the `object_store` crate's environment chain): EC2
instance roles, ECS task roles, EKS IRSA (`serviceAccount.annotations:
eks.amazonaws.com/role-arn: ...`) and EKS Pod Identity. Minimal IAM policy:
`s3:ListBucket` on the bucket, and `s3:GetObject`, `s3:PutObject`,
`s3:DeleteObject` on `arn:aws:s3:::my-bucket/*`.

### Cloudflare R2

```toml
[storage]
data_path = "s3://my-bucket/lake/"
artifacts_path = "s3://my-bucket/artifacts"
[[storage.secrets]]
type = "s3"
endpoint = "<account-id>.r2.cloudflarestorage.com"
region = "auto"
url_style = "path"
key_id = "<R2 token access key>"
secret = "<R2 token secret>"
```

Use an R2 API token with "Object Read & Write" on the bucket.

### Google Cloud Storage

GCS is used through its S3-compatible XML API with HMAC keys (one credential
works for both DuckDB and the sourcemap store):

```toml
[storage]
data_path = "gs://my-bucket/lake/"
artifacts_path = "gs://my-bucket/artifacts"
[[storage.secrets]]
type = "gcs"
key_id = "GOOG1E..."
secret = "..."
```

Create the key under Cloud Storage > Settings > Interoperability for a
service account with `roles/storage.objectAdmin` on the bucket. GKE Workload
Identity alone is not supported; the HMAC keys are required.

### Azure Blob Storage

```toml
[storage]
data_path = "az://my-container/lake/"
artifacts_path = "az://my-container/artifacts"
[[storage.secrets]]
type = "azure"
connection_string = "${AZURE_STORAGE_CONNECTION_STRING}"   # DefaultEndpointsProtocol=https;AccountName=...
```

Sourcemaps read `AZURE_STORAGE_CONNECTION_STRING` from the environment. With
`provider = "credential_chain"` the server uses the Azure credential chain with
`AZURE_STORAGE_ACCOUNT_NAME` set (managed identity, AKS workload identity via
`serviceAccount.annotations: azure.workload.identity/client-id: ...` plus the
pod label `azure.workload.identity/use: "true"` in `podLabels`).

### Self-hosted S3 (RustFS, Garage, SeaweedFS, MinIO, Ceph RGW, ...)

```toml
[storage]
data_path = "s3://sentry/lake/"
artifacts_path = "s3://sentry/artifacts"
[[storage.secrets]]
type = "s3"
endpoint = "rustfs:9000"
use_ssl = false
url_style = "path"
region = "us-east-1"                    # Garage: the s3_region from garage.toml (default "garage")
key_id = "..."
secret = "..."
```

Always set `url_style = "path"` with a custom endpoint unless the server
is set up for virtual-host style buckets (wildcard DNS). Create the bucket
first; the server does not create it.

### Local disk

```toml
[storage]
data_path = "/data/lake/"
artifacts_path = "/data/artifacts"
```

Only for a single process with the storage on a persistent volume (for
example the same PVC as the DuckDB catalog). Not usable with several pods.

## Catalog: DuckDB file or PostgreSQL

| | DuckDB file (`MALLARD_STORAGE__CATALOG=/data/catalog.ducklake`) | PostgreSQL (`MALLARD_STORAGE__CATALOG=postgres:host=... dbname=...`) |
|---|---|---|
| Moving parts | None extra. | A PostgreSQL database (managed: RDS, Cloud SQL, Azure DB, CloudNativePG, ...). |
| Processes | Exactly one. The file is locked by the process. | Several (see "Running more than one pod"). |
| Kubernetes | PVC `ReadWriteOnce`, 1 replica, `Recreate`: a few seconds of downtime on every upgrade or node move; SDKs retry or drop events during that time. | Pods keep no state; rolling updates without downtime. |
| Backups | Stop the server (or snapshot the volume) and copy the file. | `pg_dump` or the provider's point-in-time recovery. |
| Good for | Small teams, single VM, compose. | Production on Kubernetes, HA databases you already run. |

The catalog stays small (metadata plus not yet flushed events: a few MB to a
few hundred MB). Give the PVC 5-10 GiB; most of it is DuckDB spill space.

### Running more than one pod

With the PostgreSQL catalog several pods can share one catalog
([ADR 0021](../adr/0021-sentry-several-processes.md)). Ingest and API work on
every pod. Maintenance runs on one of them: the holder of a lease in the catalog
(valid for 3 flush intervals, so a replacement takes over within about 15
minutes by default).

What to expect:

- A project created through one pod works on all pods at once: an unknown
  project makes a pod read the catalog (at most once a second).
- A status change (resolve, ignore) or a sourcemap bundle reaches the other pods
  within 30 s. In that time an event on another pod does not reopen a
  just-resolved issue, and may be stored without symbolication.
- Both pods can write the first event of one new group at the same moment. Then
  `issues` has two rows for it; the API shows one issue. Raw SQL
  (`POST /v1/sql`) sees both rows.
- The limit on new issues (1000 per project and hour) and the dedupe of SDK
  retries are per pod.
- Clocks must agree within 5 minutes.

Storage must be object storage, not a local path. Tested with two processes on
one Postgres catalog and on one SQLite file (`db::tests::two_processes_*`).
OTel maintenance uses the same kind of lease, with its own row in `otel.meta`
([operations](../otel/operations.md#several-replicas)).

## Upgrades

- Read the release notes. The server records its schema version in the
  catalog and creates new tables/columns at start; there is no separate
  migration step.
- Back up the catalog first (see below).
- DuckDB file catalog: the pod is replaced (`Recreate`); expect a short gap.
- Postgres catalog: a rolling update starts the new pod before stopping the
  old one; both write for a moment, as any two pods do.
- The duckdb crate (and so the DuckDB/DuckLake format) is pinned per
  release. Downgrading across a DuckLake catalog format change is not
  supported: restore the backup instead.

## Backups

What to back up: the **catalog** and the **bucket**, together.

- Catalog: PostgreSQL with point-in-time recovery or regular `pg_dump`;
  DuckDB file: volume snapshots or a copy taken while the server is stopped.
- Bucket: turn on object versioning (or soft delete) with a retention of a
  few days. Compaction rewrites Parquet files and the server deletes files
  that are no longer referenced `MALLARD_OTEL__MAINTENANCE__SNAPSHOT_RETENTION` (default 1 hour)
  after they stop being used. Raise it above your backup interval, or rely
  on versioning. A
  catalog backup older than that points at deleted files; versioning lets you
  restore them.
- Restoring: restore the catalog, restore missing objects from versions,
  start the server.

## Sizing

Measured numbers are in [capacity.md](capacity.md). In short: one process
handles thousands of events per second; set `MALLARD_DUCKDB__MEMORY_LIMIT` to
about half the container memory (DuckDB uses some memory outside its limit).

| Traffic | Events/day | CPU | Memory | `MALLARD_DUCKDB__MEMORY_LIMIT` |
|---|---|---|---|---|
| ~10k visitors/day | 1k-20k | 0.5 | 1-2 GiB | 512MB-1GB |
| ~100k visitors/day | 10k-200k | 1 | 4 GiB | 2GB |
| bigger / error storms | 1M+ | 2 | 4-8 GiB | 2-4GB |

Watch `/metrics` (`mallard_sentry_write_batch_seconds`, `mallard_sentry_ingest_rejected_total`,
`mallard_sentry_write_errors_total`, `mallard_sentry_maintenance_errors_total`,
`mallard_sentry_issue_overflow_events_total`) and `/v1/info`
(queue depth). Do not set a CPU limit below 1 core or compaction throttles.

## Health and monitoring

- `GET /healthz`: liveness, answers `ok` while the process runs.
- `GET /readyz`: readiness, runs a query through the reader pool.
- `GET /metrics`: Prometheus metrics (no token).
- `POST /v1/maintenance` (token): flush inlined data and compact now.
- `mallard maintenance [--full]` (no server running with a DuckDB-file
  or SQLite catalog): both sides' maintenance once, then exit. `--full` only
  changes the telemetry tables. Next to running pods on Postgres it can lose a
  conflict against their maintenance; it then exits with an error and is safe
  to run again.
