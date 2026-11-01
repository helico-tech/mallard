# Deployment

sentry-ducklake is one binary (`sentry-ducklake`) with two moving parts
around it:

- **Storage**: a bucket (or local folder) for Parquet data files and
  sourcemap bundles. One URL, `SDL_STORAGE_URL`, holds both:
  `<url>/lake/` (DuckLake data) and `<url>/artifacts/` (sourcemaps).
- **Catalog**: the DuckLake metadata database, `SDL_CATALOG`. Either a DuckDB
  file (one process only) or PostgreSQL. It also holds recent small batches
  of events until they are flushed into Parquet (every `SDL_FLUSH_INTERVAL`).

The catalog and the bucket belong together: the catalog says which files
exist; without it the Parquet files cannot be read as tables.

Assets in this repo:

| Path | What |
|---|---|
| `deploy/docker/Dockerfile` | Image: Debian slim, non-root (uid 10001), all DuckDB extensions pre-installed (no internet needed at run time). |
| `deploy/compose/docker-compose.yml` | Local stack: server + RustFS (S3) + PostgreSQL catalog. |
| `deploy/helm/sentry-ducklake/` | Helm chart. |

## Docker

```sh
docker build -f deploy/docker/Dockerfile -t sentry-ducklake .

# Everything on a local volume (DuckDB catalog + local storage):
docker run -d --name sdl -p 8000:8000 -v sdl-data:/data \
  -e SDL_API_TOKEN=change-me \
  -e SDL_PUBLIC_URL=http://localhost:8000 \
  -e SDL_STORAGE_URL=/data/storage \
  sentry-ducklake
```

Image facts:

- Working directory and `HOME` is `/data`; the default catalog is
  `/data/catalog.ducklake`. DuckDB writes spill files to `/data/.tmp`.
  Mount a volume on `/data`.
- Works with `--read-only` (needs a writable `/data`; add `--tmpfs /tmp`).
- `SDL_EXTENSION_DIR=/opt/duckdb/extensions` is set; the extensions there
  match the image's platform. Do not override it.
- Logs are JSON (`SDL_LOG_JSON=true`); set `RUST_LOG` to change the level.
- Built-in `HEALTHCHECK` calls `/healthz`.
- On `SIGTERM` the server commits queued events and flushes inlined data.
  Give it time: `docker stop -t 60`.

## Docker Compose

```sh
docker compose -f deploy/compose/docker-compose.yml up -d --build
TOKEN=dev-token
curl -s -XPOST -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  localhost:8000/v1/projects -d '{"name":"web"}'          # -> {"dsn": "http://<key>@localhost:8000/1", ...}
```

The stack: RustFS (S3-compatible store; why RustFS is explained at the top
of the compose file), a one-shot bucket creation step, PostgreSQL as the
catalog, and the server with data under `s3://sentry/errors`.

Simpler mode (DuckDB-file catalog on the `sdl-data` volume, data still on S3):

```sh
SDL_CATALOG=/data/catalog.ducklake docker compose -f deploy/compose/docker-compose.yml up -d
```

Override `SDL_PORT`, `SDL_API_TOKEN`, `S3_ACCESS_KEY`, `S3_SECRET_KEY`,
`POSTGRES_PASSWORD` (in the shell or a `.env` file next to the compose file)
before you expose it anywhere. `docker compose ... down -v` deletes all data.

## Helm

```sh
helm install errors deploy/helm/sentry-ducklake \
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
| `catalog.postgres.connectionString` / `.existingSecret` | libpq string, e.g. `host=pg port=5432 dbname=ducklake user=sdl password=... sslmode=require`. |
| `retentionDays` | Delete events older than this (0 = keep). |
| `duckdb.memoryLimit` | DuckDB memory cap; keep it about half of the memory limit. |
| `extraEnv` | Any other `SDL_*` setting. |
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
  `SDL_MAX_BODY_BYTES` (50 MiB). Raise the proxy body limit, e.g.
  `nginx.ingress.kubernetes.io/proxy-body-size: 64m`.
- `/metrics`, `/docs` and `/openapi.json` are on the same port and need no
  token. Block `/metrics` at the ingress if it must not be public.
- If the ingress sets `X-Forwarded-For`, set `SDL_TRUST_FORWARDED_FOR=true`
  (via `extraEnv`) so client IPs are recorded correctly.
- When the server is mounted below a path, put that path in `publicUrl`
  (`https://host/sentry`) and route the prefix with a rewrite.

## Configuration reference

Every flag has an environment variable. The most used:

| Variable | Default | Meaning |
|---|---|---|
| `SDL_API_TOKEN` | (none, required) | Bearer token(s) for `/v1` and sourcemap uploads, comma separated. |
| `SDL_PUBLIC_URL` | `http://localhost:8000` | Base URL in DSNs. |
| `SDL_STORAGE_URL` | (none, required) | `s3://bucket/prefix`, `gs://...`, `az://...`, local path. |
| `SDL_CATALOG` | `./.data/catalog.ducklake` (image: `/data/catalog.ducklake`) | DuckDB file path, `postgres:<libpq>`, `sqlite:<path>`, `mysql:<string>`. |
| `SDL_S3_ENDPOINT` | (AWS) | Custom S3 endpoint with scheme. |
| `SDL_S3_REGION` | `us-east-1` | S3 region. |
| `SDL_S3_PATH_STYLE` | `false` | Path-style URLs. |
| `SDL_RETENTION_DAYS` | `90` | 0 = keep forever. |
| `SDL_FLUSH_INTERVAL` | `5m` | How often inlined data becomes Parquet. |
| `SDL_COMPACT_INTERVAL` | `1h` | Compaction, retention, snapshot expiry. |
| `SDL_CATALOG_WIDE_MAINTENANCE` | `true` | Snapshot expiry and file cleanup; only one service per catalog should do it. |
| `SDL_DUCKDB_MEMORY_LIMIT` | 80% of RAM | e.g. `1GB`. |
| `SDL_DUCKDB_THREADS` | CPUs | DuckDB threads. |
| `SDL_READERS` | `4` | Read connections for the query API. |
| `SDL_BATCH_MAX` / `SDL_BATCH_WINDOW` | `2000` / `500ms` | Max events per commit / how long a quiet server collects before committing (200+ queued events commit at once). |
| `SDL_INGEST_CONCURRENCY` | `256` | Ingest requests processed at once (bounds request memory). |
| `SDL_MAINTENANCE_THREADS` | `1` | DuckDB threads during compaction (sorted merges need memory per thread); 0 = unchanged. |
| `SDL_FLUSH_ROWS` | `50000` | Also flush inlined rows to Parquet after this many (bounds catalog size). |
| `SDL_SNAPSHOT_RETENTION` | `1h` | Keep snapshots and replaced files this long; set above your catalog backup age. |
| `SDL_DUCKDB_TEMP_DIR` | `./.tmp` | DuckDB spill directory. |
| `SDL_METRICS_LISTEN` | (main port) | Serve `/metrics` on a separate address, e.g. `0.0.0.0:9090`. |
| `SDL_QUEUE_CAPACITY` | `20000` | Queued events before ingest answers 429. |
| `SDL_MAX_EVENT_BYTES` / `SDL_MAX_BODY_BYTES` | 1 MiB / 20 MiB | Size limits (after decompression). Requests with the key only inside the envelope are limited to 2 MiB. |
| `SDL_SOURCEMAP_CACHE_MB` | `256` | Sourcemap cache. |
| `SDL_TRUST_FORWARDED_FOR` | `false` | Use `X-Forwarded-For` as client IP. |
| `SDL_LISTEN` | `0.0.0.0:8000` | Listen address. |
| `SDL_LOG_JSON`, `RUST_LOG` | | Logging. |

`sentry-ducklake --help` lists all of them.

## Storage

Prefer a dedicated bucket (or prefix) per deployment. The server needs list,
read, write and delete on `<prefix>/*`.

### AWS S3

```sh
SDL_STORAGE_URL=s3://my-bucket/sentry
SDL_S3_REGION=eu-west-1
AWS_ACCESS_KEY_ID=...  AWS_SECRET_ACCESS_KEY=...   # optional AWS_SESSION_TOKEN
```

Without keys, the server uses the AWS credential chain (DuckDB's
`credential_chain` and the `object_store` crate's environment chain): EC2
instance roles, ECS task roles, EKS IRSA (`serviceAccount.annotations:
eks.amazonaws.com/role-arn: ...`) and EKS Pod Identity. Minimal IAM policy:
`s3:ListBucket` on the bucket, and `s3:GetObject`, `s3:PutObject`,
`s3:DeleteObject` on `arn:aws:s3:::my-bucket/sentry/*`.

### Cloudflare R2

```sh
SDL_STORAGE_URL=s3://my-bucket/sentry
SDL_S3_ENDPOINT=https://<account-id>.r2.cloudflarestorage.com
SDL_S3_REGION=auto
SDL_S3_PATH_STYLE=true
AWS_ACCESS_KEY_ID=<R2 token access key>  AWS_SECRET_ACCESS_KEY=<R2 token secret>
```

Use an R2 API token with "Object Read & Write" on the bucket.

### Google Cloud Storage

GCS is used through its S3-compatible XML API with HMAC keys (one credential
works for both DuckDB and the sourcemap store):

```sh
SDL_STORAGE_URL=gs://my-bucket/sentry
GCS_HMAC_KEY_ID=GOOG1E...  GCS_HMAC_SECRET=...
```

Create the key under Cloud Storage > Settings > Interoperability for a
service account with `roles/storage.objectAdmin` on the bucket. GKE Workload
Identity alone is not supported; the HMAC keys are required.

### Azure Blob Storage

```sh
SDL_STORAGE_URL=az://my-container/sentry
AZURE_STORAGE_CONNECTION_STRING='DefaultEndpointsProtocol=https;AccountName=...;AccountKey=...;EndpointSuffix=core.windows.net'
```

Without a connection string the server uses the Azure credential chain with
`AZURE_STORAGE_ACCOUNT_NAME` set (managed identity, AKS workload identity via
`serviceAccount.annotations: azure.workload.identity/client-id: ...` plus the
pod label `azure.workload.identity/use: "true"` in `podLabels`).

### Self-hosted S3 (RustFS, Garage, SeaweedFS, MinIO, Ceph RGW, ...)

```sh
SDL_STORAGE_URL=s3://sentry/errors
SDL_S3_ENDPOINT=http://rustfs:9000      # http:// turns TLS off
SDL_S3_PATH_STYLE=true
SDL_S3_REGION=us-east-1                 # Garage: the s3_region from garage.toml (default "garage")
AWS_ACCESS_KEY_ID=...  AWS_SECRET_ACCESS_KEY=...
```

Always set `SDL_S3_PATH_STYLE=true` with a custom endpoint unless the server
is set up for virtual-host style buckets (wildcard DNS). Create the bucket
first; the server does not create it.

### Local disk

```sh
SDL_STORAGE_URL=/data/storage          # or file:///data/storage
```

Only for a single process with the storage on a persistent volume (for
example the same PVC as the DuckDB catalog). Not usable with several pods.

## Catalog: DuckDB file or PostgreSQL

| | DuckDB file (`SDL_CATALOG=/data/catalog.ducklake`) | PostgreSQL (`SDL_CATALOG=postgres:host=... dbname=...`) |
|---|---|---|
| Moving parts | None extra. | A PostgreSQL database (managed: RDS, Cloud SQL, Azure DB, CloudNativePG, ...). |
| Processes | Exactly one. The file is locked by the process. | Several processes can attach (see limits below). |
| Kubernetes | PVC `ReadWriteOnce`, 1 replica, `Recreate`: a few seconds of downtime on every upgrade or node move; SDKs retry or drop events during that time. | Pods keep no state; rolling updates without downtime. |
| Backups | Stop the server (or snapshot the volume) and copy the file. | `pg_dump` or the provider's point-in-time recovery. |
| Good for | Small teams, single VM, compose. | Production on Kubernetes, HA databases you already run. |

The catalog stays small (metadata plus not yet flushed events: a few MB to a
few hundred MB). Give the PVC 5-10 GiB; most of it is DuckDB spill space.

### Running more than one pod

With the PostgreSQL catalog several pods can share one catalog, but the
current server keeps an in-memory cache of projects and issue states that it
loads at start and does not refresh. So:

- A project created through one pod is unknown to the other pods (their
  ingest rejects its DSN) until they restart.
- Issue status changes (resolve, ignore) made through one pod are not seen
  by the others until restart.
- Every pod runs catalog-wide maintenance (`SDL_CATALOG_WIDE_MAINTENANCE`
  defaults to `true`).

This is tracked in [work/issues/0001](../work/issues/0001-multi-replica-caches.md).
Until the server refreshes these caches, run one replica (the chart
default). The Postgres catalog still gives you stateless pods, rolling
updates with a brief overlap and easy backups. If you do run more pods:
create projects, then restart the deployment.

## Upgrades

- Read the release notes. The server records its schema version in the
  catalog and creates new tables/columns at start; there is no separate
  migration step.
- Back up the catalog first (see below).
- DuckDB file catalog: the pod is replaced (`Recreate`); expect a short gap.
- Postgres catalog: a rolling update starts the new pod before stopping the
  old one; both write for a moment, which DuckLake handles with optimistic
  commits.
- The duckdb crate (and so the DuckDB/DuckLake format) is pinned per
  release. Downgrading across a DuckLake catalog format change is not
  supported: restore the backup instead.

## Backups

What to back up: the **catalog** and the **bucket**, together.

- Catalog: PostgreSQL with point-in-time recovery or regular `pg_dump`;
  DuckDB file: volume snapshots or a copy taken while the server is stopped.
- Bucket: turn on object versioning (or soft delete) with a retention of a
  few days. Compaction rewrites Parquet files and the server deletes files
  that are no longer referenced `SDL_SNAPSHOT_RETENTION` (default 1 hour)
  after they stop being used. Raise it above your backup interval, or rely
  on versioning. A
  catalog backup older than that points at deleted files; versioning lets you
  restore them.
- Restoring: restore the catalog, restore missing objects from versions,
  start the server.

## Sizing

Measured numbers are in [capacity.md](capacity.md). In short: one process
handles thousands of events per second; set `SDL_DUCKDB_MEMORY_LIMIT` to
about half the container memory (DuckDB uses some memory outside its limit).

| Traffic | Events/day | CPU | Memory | `SDL_DUCKDB_MEMORY_LIMIT` |
|---|---|---|---|---|
| ~10k visitors/day | 1k-20k | 0.5 | 1-2 GiB | 512MB-1GB |
| ~100k visitors/day | 10k-200k | 1 | 4 GiB | 2GB |
| bigger / error storms | 1M+ | 2 | 4-8 GiB | 2-4GB |

Watch `/metrics` (`sdl_write_batch_seconds`, `sdl_ingest_rejected_total`,
`sdl_write_errors_total`, `sdl_maintenance_errors_total`) and `/v1/info`
(queue depth). Do not set a CPU limit below 1 core or compaction throttles.

## Health and monitoring

- `GET /healthz`: liveness, answers `ok` while the process runs.
- `GET /readyz`: readiness, runs a query through the reader pool.
- `GET /metrics`: Prometheus metrics (no token).
- `POST /v1/maintenance` (token): flush inlined data and compact now.
