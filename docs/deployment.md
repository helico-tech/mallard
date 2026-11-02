# Deployment

How to run mallard: Docker, compose, storage, catalog and backups, for the whole
server. mallard ships as a Docker image; there is no Helm chart for now
([ADR 0026](adr/0026-no-helm-chart-for-now.md)). Telemetry extras (pointing SDKs at it,
a Collector in front, TLS) are in [otel/deployment.md](otel/deployment.md).

mallard is one binary, `mallard`, with two moving parts around it:

- **Storage**: a bucket (or local folder) for Parquet data files and sourcemap
  bundles: `storage.data_path` (DuckLake data) and `storage.artifacts_path`
  (sourcemaps, in `<path>/artifacts/`).
- **Catalog**: the DuckLake metadata database, `storage.catalog`: a DuckDB file or
  SQLite (one process), or PostgreSQL. It also holds recent small batches of data
  until they are flushed into Parquet.

The catalog and the bucket belong together: the catalog says which files exist;
without it the Parquet files cannot be read as tables.

Assets in this repo:

| Path | What |
|---|---|
| `deploy/docker/Dockerfile` | Image: Debian slim, non-root (uid 10001), all DuckDB extensions pre-installed (no internet needed at run time). |
| `deploy/compose/docker-compose.yml` | Local stack: server + RustFS (S3) + PostgreSQL catalog. |

Ports: 8080 (Sentry ingest, both APIs, OTLP/HTTP on `/v1/*`, `/metrics`), 4318
(OTLP/HTTP), 4317 (OTLP/gRPC).

A version tag publishes the image to `ghcr.io/helico-tech/mallard` for amd64 and arm64
(`.github/workflows/release.yml`). No version has been tagged yet, so for now build it
yourself.

## Docker

```sh
docker build -f deploy/docker/Dockerfile -t mallard .

# Everything on a local volume (DuckDB catalog + local storage):
docker run -d --name mallard --restart unless-stopped -p 8080:8080 -p 4318:4318 -p 4317:4317 -v mallard-data:/data \
  -e MALLARD_SERVER__API_TOKENS=change-me \
  -e MALLARD_SERVER__PUBLIC_URL=http://localhost:8080 \
  -e MALLARD_STORAGE__ARTIFACTS_PATH=/data/artifacts \
  mallard
docker exec mallard mallard --token change-me status
```

Image facts:

- The default command is `mallard serve`. There is no config file in the image: use
  `MALLARD_<SECTION>__<KEY>` variables, or mount a file and set `MALLARD_CONFIG`
  (or pass `serve -c <file>`).
- Working directory and `HOME` is `/data`; the default catalog is
  `/data/data/catalog.ducklake` and the lake `/data/data/lake/`. DuckDB writes spill
  files to `/data/.tmp`. Mount a volume on `/data`.
- Works with `--read-only` (needs a writable `/data`; add `--tmpfs /tmp`).
- `MALLARD_DUCKDB__EXTENSION_DIRECTORY=/opt/duckdb/extensions` is set and
  auto-install is off; the extensions there match the image's platform. Do not
  override them.
- Logs go to stderr, as text; `MALLARD_SERVER__LOG_FORMAT=json` makes them JSON. Set
  `RUST_LOG` to change the level.
- Built-in `HEALTHCHECK` calls `/healthz`.
- On `SIGTERM` the server stops the receivers, stores what it holds and flushes.
  Give it time: `docker stop -t 60`.

## Docker Compose

Local use only: the defaults in the compose file are development credentials.

```sh
docker compose -f deploy/compose/docker-compose.yml up -d --build
export MALLARD_URL=http://localhost:8080 MALLARD_TOKEN=dev-token
mallard status
curl -s -XPOST -H "Authorization: Bearer $MALLARD_TOKEN" -H 'content-type: application/json' \
  $MALLARD_URL/sentry/v1/projects -d '{"name":"web"}'   # -> {"dsn": "http://<key>@localhost:8080/sentry/1", ...}
docker compose -f deploy/compose/docker-compose.yml exec mallard mallard --token dev-token status
```

The stack: RustFS (S3-compatible store), a one-shot bucket creation step,
PostgreSQL as the catalog, and the server with data under `s3://mallard/lake/` and
sourcemaps under `s3://mallard/artifacts`. Its config is
`deploy/compose/mallard.toml`.

Simpler mode (DuckDB-file catalog on the `mallard-data` volume, data still on S3;
one server process only):

```sh
MALLARD_CATALOG=/data/catalog.ducklake docker compose -f deploy/compose/docker-compose.yml up -d
```

Override `MALLARD_PORT`, `MALLARD_OTLP_HTTP_PORT`, `MALLARD_OTLP_GRPC_PORT`,
`MALLARD_API_TOKEN`, `S3_ACCESS_KEY`, `S3_SECRET_KEY`, `POSTGRES_PASSWORD` and
`MALLARD_IMAGE` in the shell or a `.env` file next to the compose file, and change
every secret before you expose it anywhere. `docker compose ... down -v` deletes all
data.

## Running the image in production

The image runs under any container platform. This one uses a Postgres catalog and object
storage, the set-up that also allows several replicas. Build the image and push it to
your own registry first:

```sh
docker run -d --name mallard -p 8080:8080 -p 4318:4318 -p 4317:4317 \
  --read-only --tmpfs /tmp -v mallard-data:/data --stop-timeout 90 \
  -e MALLARD_SERVER__PUBLIC_URL=https://mallard.example.com \
  -e MALLARD_SERVER__API_TOKENS="$MALLARD_API_TOKEN" \
  -e MALLARD_SERVER__INGEST_TOKENS="$MALLARD_INGEST_TOKEN" \
  -e MALLARD_STORAGE__CATALOG='postgres:host=pg port=5432 dbname=ducklake user=mallard sslmode=require' \
  -e PGPASSWORD="$PGPASSWORD" \
  -e MALLARD_CONFIG=/etc/mallard/mallard.toml -v "$PWD/mallard.toml:/etc/mallard/mallard.toml:ro" \
  <your registry>/mallard:<tag>
```

`mallard.toml` holds `[storage]` with `data_path`, `artifacts_path` and the
`[[storage.secrets]]` entry; see [Storage](#storage). The compose file shows a complete
example, and `mallard check -c <file> --connect` tests one. A local storage path does not
work with several replicas.

What holds on any platform, including Kubernetes:

- **Config**: everything can be set with `MALLARD_<SECTION>__<KEY>` variables, and `${NAME}`
  inside a config file reads a variable, so secrets stay out of files. The required
  settings are `server.api_tokens` and, so that DSNs are right, `server.public_url`.
- **Health**: `/healthz` for liveness and for start-up, and `/readyz` for readiness. Attaching the
  catalog can take a while, so allow up to 5 minutes at start-up.
- **Stopping**: on `SIGTERM` the server waits up to 20 s for requests in flight, then flushes both
  sides. Give it at least 60 s; 90 s is safer (`terminationGracePeriodSeconds: 90` in Kubernetes).
- **Restart policy**: required (`restart: unless-stopped` in compose, the default in Kubernetes). With a
  DuckDB-file catalog, running out of memory during a catalog checkpoint can invalidate the catalog for the
  whole process. Nothing can be stored until it is attached again, so the server logs that at `error`, stops
  and exits with code 1. Without a restart policy it stays down. What was buffered and not yet flushed is lost.
- **Security**: it runs as uid 10001 and needs no capabilities (drop them all), and the root
  filesystem can be read-only. It needs a writable `/data` and `/tmp`.
- **Volumes**: with a DuckDB-file catalog, `/data` must persist, and only one process may
  run. With Postgres and object storage, `/data` holds only spill files, so a scratch
  volume is enough (2 GiB or so). See [Catalog](#catalog-duckdb-file-or-postgresql).
- **Identity**: with `provider = "credential_chain"` the server uses the platform's
  credentials (EC2 and ECS roles, EKS IRSA and Pod Identity, Azure managed identity).

Behind a reverse proxy or ingress:

- Route the HTTP port: Sentry ingest, both APIs and OTLP/HTTP on `/v1/*`.
  OTLP/gRPC needs port 4317.
- Browser SDKs post cross-origin. The server sends the CORS headers itself; do not
  add CORS headers in the proxy (duplicate headers make browsers reject the response).
- Sourcemap uploads send up to 32 MiB per request and envelopes up to
  `sentry.ingest.max_body_bytes` (20 MiB). Raise the proxy's body limit to 64 MiB.
- `/metrics` is on the main port with no token. Set `server.metrics_addr` to serve it on a
  port of its own that the proxy does not route. `/sentry/docs`, `/otel/docs` and their
  `openapi.json` also need no token.
- If the proxy sets `X-Forwarded-For`, set `MALLARD_SENTRY__INGEST__TRUST_FORWARDED_FOR=true`
  so client IPs are recorded correctly. The server uses the last address in the header (the
  one the proxy adds), so this is right for exactly one proxy hop.

## Configuration

One TOML file plus `MALLARD_<SECTION>__<KEY>` variables.
[config/mallard.example.toml](../config/mallard.example.toml) lists the keys with
their defaults; check a config with `mallard check -c <file> [--connect]`.
The only required one is `server.api_tokens`. The Sentry settings are the
`[sentry.*]` sections. What the example does not say:

- `sentry.ingest.max_event_bytes` and `.max_body_bytes` are sizes after
  decompression, e.g. `2MiB` or a number of bytes. A request with its key only inside
  the envelope is limited to 2 MiB.
- `sentry.ingest.batch_window` is how long a quiet server collects before it commits;
  200 or more queued events commit at once.
- `sentry.query.enable_sql` turns on `POST /sentry/v1/sql` (ADR 0011). API tokens
  then become admin access.
- `duckdb.memory_limit` defaults to DuckDB's own, 80% of RAM. Accepts `1GB`, or `1GiB`
  for powers of 1024.
- Snapshot expiry is catalog-wide and telemetry's:
  `otel.maintenance.snapshot_retention` ([otel/configuration.md](otel/configuration.md)).

The tables live in the DuckLake schema `sentry` of the catalog attached as `lake`
(both fixed).

## Storage

Prefer a dedicated bucket (or prefix) per deployment. The server needs list,
read, write and delete on `<prefix>/*`. These examples show both locations, with
what sourcemap storage needs; [otel/storage-backends.md](otel/storage-backends.md)
has more on the data path. `artifacts_path` takes a local path, `s3://`, `gs://` or
`az://`, and must not be inside `data_path`. It does not take `r2://` or `abfss://`, which
work for `data_path` only; for R2, use `s3://` as shown below. `mallard check --connect`
fails with exit 2 on a location the server could not start with. It builds the store from
the config and does not contact it, so a wrong key or a missing bucket shows up at the
first upload.

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
instance roles, ECS task roles, EKS IRSA and EKS Pod Identity. Minimal IAM policy:
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

The one secret covers the lake and the sourcemap store, if its `scope` covers both
(or it has none). The store reads `AccountName`, `AccountKey` and `BlobEndpoint` from the
connection string and ignores the rest. So on a cloud other than the public one, set
`BlobEndpoint`, because `EndpointSuffix` has no effect there. A connection string with
only a SAS token (`SharedAccessSignature`) does not work for sourcemaps.
`AZURE_STORAGE_CONNECTION_STRING` alone, without a secret, is not read. Without a
connection string the store uses `AZURE_STORAGE_ACCOUNT_NAME` and
`AZURE_STORAGE_ACCOUNT_KEY`, and fails at start-up and in `check --connect` without an
account name. Tested against the Azurite emulator, not against real Azure.

With `provider = "credential_chain"` the lake uses the Azure credential chain, with
`AZURE_STORAGE_ACCOUNT_NAME` set (managed identity, AKS workload identity).

### Self-hosted S3 (RustFS, Garage, SeaweedFS, MinIO, Ceph RGW, ...)

```toml
[storage]
data_path = "s3://mallard/lake/"
artifacts_path = "s3://mallard/artifacts"
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
example the same volume as the DuckDB catalog). Not usable with several processes.

## Catalog: DuckDB file or PostgreSQL

SQLite is also possible; see [otel/storage-backends.md](otel/storage-backends.md).

| | DuckDB file (`MALLARD_STORAGE__CATALOG=/data/catalog.ducklake`) | PostgreSQL (`MALLARD_STORAGE__CATALOG='postgres:host=... dbname=...'`) |
|---|---|---|
| Moving parts | None extra. | A PostgreSQL database (managed: RDS, Cloud SQL, Azure DB, CloudNativePG, ...). |
| Processes | Exactly one. The file is locked by the process. | Several (see "Running more than one process"). |
| Upgrades | One container on one volume: stop the old one first, so a few seconds of downtime on every upgrade or move; SDKs retry or drop events during that time. | Containers keep no state; replace them one at a time, without downtime. |
| Backups | Stop the server (or snapshot the volume) and copy the file. | `pg_dump` or the provider's point-in-time recovery. |
| Good for | Small teams, single VM, compose. | Production with several replicas, HA databases you already run. |

The catalog stays small (metadata plus not yet flushed data: a few MB to a few
hundred MB). Give the volume 5-10 GiB; most of it is DuckDB spill space.

### Running more than one process

With the PostgreSQL catalog several processes can share one catalog
([ADR 0021](adr/0021-sentry-several-processes.md)). Ingest and API work on every
process. Sentry maintenance runs on one of them: the holder of a lease in the catalog
(valid for 3 flush intervals, so a replacement takes over within about 15 minutes by
default).

What to expect:

- A project created through one process works on all processes at once: an unknown project
  makes a process read the catalog (at most once a second).
- A status change (resolve, ignore) or a sourcemap bundle reaches the other processes
  within 30 s. In that time an event on another process does not reopen a just-resolved
  issue, and may be stored without symbolication.
- Both processes can write the first event of one new group at the same moment. Then
  `issues` has two rows for it; the API shows one issue. Raw SQL
  (`POST /sentry/v1/sql`) sees both rows.
- The limit on new issues (1000 per project and hour) and the dedupe of SDK retries
  are per process.
- Clocks must agree within 5 minutes.

Storage must be object storage, not a local path. Tested with two processes on one
Postgres catalog and on one SQLite file (`db::tests::two_processes_*`). OTel
maintenance uses the same kind of lease, with its own row in `otel.meta`
([operations](otel/operations.md#several-replicas)).

## Upgrades

- Read the release notes. The server records its schema version in the catalog and
  creates new tables and columns at start; there is no separate migration step.
- Back up the catalog first (see below). Telemetry details:
  [otel/operations.md](otel/operations.md#upgrades).
- DuckDB file catalog: stop the old container, then start the new one; expect a short gap.
- Postgres catalog: a rolling update starts the new container before stopping the
  old one; both write for a moment, as any two processes do.
- The duckdb crate (and so the DuckDB/DuckLake format) is pinned per
  release. Downgrading across a DuckLake catalog format change is not
  supported: restore the backup instead.

## Backups

What to back up: the **catalog** and the **bucket**, together.

- Catalog: PostgreSQL with point-in-time recovery or regular `pg_dump`;
  DuckDB file: volume snapshots or a copy taken while the server is stopped.
- Bucket: turn on object versioning (or soft delete) with a retention of a
  few days. Compaction rewrites Parquet files, and the server deletes files that
  are no longer used after `otel.maintenance.snapshot_retention` (default 1 hour).
  Raise it above your backup interval, or rely on versioning. A catalog backup older
  than that points at deleted files; versioning lets you restore them.
- Restoring: restore the catalog, restore missing objects from versions,
  start the server.

## Sizing

Measured numbers are in [sentry/capacity.md](sentry/capacity.md) and
[otel/capacity.md](otel/capacity.md). Set `duckdb.memory_limit` to about half the
container memory and keep container >= limit + `otel.ingest.max_buffer_bytes` + 0.9 GiB.
A 2 GiB container runs `1GB`, a `128MB` buffer and 4 threads, as compose does; 4 GiB runs
`2GB`, `256MB` and 8. Do not set a CPU limit below 1 core, or compaction throttles.

## Health and monitoring

- `GET /healthz`: liveness, answers `ok` while the process runs.
- `GET /readyz`: readiness of both sides: a query through the Sentry reader pool, the
  telemetry flush state and the catalog.
- `GET /metrics`: Prometheus metrics (no token). For this side watch
  `mallard_sentry_write_batch_seconds`, `mallard_sentry_ingest_rejected_total`,
  `mallard_sentry_write_errors_total`, `mallard_sentry_write_memory_retries_total` (a batch waited
  because memory was short), `mallard_sentry_maintenance_errors_total` and
  `mallard_sentry_issue_overflow_events_total`. Telemetry:
  [otel/operations.md](otel/operations.md).
- `mallard status`: both sides. `mallard status --json` (`sentry.info`) or `GET
  /sentry/v1/info`: queue depth.
- `POST /sentry/v1/maintenance` (token) or `mallard flush`: flush inlined
  data and compact now.
- `mallard maintenance [--full]` (no server running with a DuckDB-file or SQLite
  catalog): both sides' maintenance once, then exit. `--full` only changes the
  telemetry tables. Next to running processes on Postgres it can lose a conflict against
  their maintenance; it then exits with an error and is safe to run again.
