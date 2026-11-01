# Deployment

> **Retired binary.** The `otel-ducklake` binary, its `ODL_*` settings and
> its chart are gone (story 0002/06). Deploy mallard: `deploy/` and the
> [repo README](../../README.md).

Three reference setups, all tested:

| setup | catalog | data | replicas | use when |
|---|---|---|---|---|
| [Docker, single node](#docker-single-node) | DuckDB file | local volume | 1 | one VM, simplest |
| [Docker, shared storage](#docker-postgresql--s3) | PostgreSQL | S3-compatible | ≥1 | HA, or data in a bucket |
| [Kubernetes (Helm)](#kubernetes-helm) | DuckDB file or PostgreSQL | PVC or object storage | 1 or more | clusters |

Not containerized: download a release binary (Linux builds target glibc
2.35+, i.e. Ubuntu 22.04 / Debian 12 or newer) or `cargo build --release`,
and run `otel-ducklake serve` with a config file.

> **Security:** authentication is off by default. Anyone who can reach the
> ports can read all telemetry (8080) and write into the lake (4317/4318).
> The examples bind to `127.0.0.1`. Before exposing the ports, set
> `auth.api_tokens` (and `auth.ingest_tokens`, or keep ingest on a private
> network) and put TLS in front.

## Container image

`ghcr.io/helico-tech/otel-ducklake:<version>` (linux/amd64 and linux/arm64)
contains `otel-ducklake`, `odl` and `odl-loadgen`.

- DuckDB extensions (ducklake, httpfs, aws, azure, postgres, sqlite, icu) are
  baked in at `/opt/duckdb/extensions`, and auto-install is off: the image
  works without internet access. `json` and `parquet` are compiled in. Build
  a smaller image with `--build-arg DUCKDB_EXTENSIONS="ducklake httpfs postgres"`.
- Runs as uid 10001. State lives in `/data` (catalog, Parquet, spill files in
  `/data/tmp`); the root filesystem can be read-only.
- Default config `/etc/otel-ducklake/config.toml`: DuckDB catalog
  `/data/catalog.ducklake`, data `/data/lake/`. Mount your own file there (or
  pass `-c`), and/or override single keys with `ODL_<SECTION>__<KEY>`.
- Ports: 4317 (OTLP/gRPC), 4318 (OTLP/HTTP), 8080 (query API, `/docs`).
- Health: `GET /api/v1/health`, or `odl health` inside the container.
- Size: about 156 MB compressed (most of it DuckDB extensions).

```sh
docker run -d --name otel -p 127.0.0.1:4317:4317 -p 127.0.0.1:4318:4318 -p 127.0.0.1:8080:8080 \
  -v otel-data:/data -e ODL_DUCKDB__MEMORY_LIMIT=1GB \
  ghcr.io/helico-tech/otel-ducklake:latest
docker exec otel odl services
```

## Docker, single node

[`deploy/compose/docker-compose.yml`](../deploy/compose/docker-compose.yml):

```sh
docker compose -f deploy/compose/docker-compose.yml up -d --build
docker compose -f deploy/compose/docker-compose.yml --profile loadgen up -d   # demo traffic
docker compose -f deploy/compose/docker-compose.yml exec otel-ducklake odl services
odl tui     # from the host, if you installed odl
```

## Docker, PostgreSQL + S3

[`deploy/compose/docker-compose.postgres-s3.yml`](../deploy/compose/docker-compose.postgres-s3.yml)
runs PostgreSQL (catalog), SeaweedFS (S3-compatible object store), a job that
creates the bucket, and **two** collector replicas writing to the same lake.
The config is in [`config/shared.toml`](../deploy/compose/config/shared.toml).

```sh
docker compose -f deploy/compose/docker-compose.postgres-s3.yml up -d --build
docker compose -f deploy/compose/docker-compose.postgres-s3.yml --profile loadgen up -d
docker compose -f deploy/compose/docker-compose.postgres-s3.yml exec odl-1 odl status
```

Replica `odl-1` listens on 4317/4318/8080, `odl-2` on 14317/14318/18080, and
the S3 endpoint on 8333, all on `127.0.0.1` only; put any load balancer in
front. Both ingest, and each sees the other's data. Only
`odl-1` runs catalog-wide maintenance. Verified: Parquet files appear under
`s3://otel/lake/otel/spans/epoch_day=…/`, no flush errors on either replica.

Swap SeaweedFS for AWS S3, R2, MinIO, Ceph or Garage by changing only the
`[[storage.secrets]]` block. The demo credentials in the compose file are
examples; change them.

## Kubernetes (Helm)

Chart: [`charts/otel-ducklake`](../charts/otel-ducklake) (see its
[README](../charts/otel-ducklake/README.md) for all values).

```sh
# single: StatefulSet + PVC, DuckDB-file catalog
helm install otel charts/otel-ducklake -n observability --create-namespace
# (an Ingress for the API is only allowed with auth.enabled=true)

# shared: Deployment with N replicas, PostgreSQL catalog + object storage
helm install otel charts/otel-ducklake -n observability -f my-values.yaml
```

```yaml
# my-values.yaml
mode: shared
replicaCount: 2
storage:
  catalog: "postgres:host=pg.db dbname=ducklake user=odl"   # password: PGPASSWORD env (envFrom)
  dataPath: "s3://my-bucket/otel/"
  secrets:
    - type: s3
      provider: credential_chain   # IRSA / pod identity
      region: eu-west-1
envFrom:
  - secretRef: {name: odl-storage} # PGPASSWORD (and S3 keys without IRSA)
auth:
  enabled: true
  existingSecret: odl-tokens       # key: api-token (+ ingest-token when ingestAuth)
  ingestAuth: false                # true: OTLP needs a token too
resources:
  limits: {cpu: "2", memory: 2Gi}
duckdb:
  memoryLimit: 768MB               # memory limit ≥ this + ingest buffer + 1 GB
```

Tested on a local cluster: single mode keeps all data across pod deletion.
Shared mode with in-cluster PostgreSQL + S3 stored 19,333 of 19,333 spans
from 2 replicas with no flush errors. The pods run non-root with a read-only
root filesystem.

## Pointing your services at it

```sh
# any OpenTelemetry SDK
export OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-ducklake:4318   # or :4317 with OTEL_EXPORTER_OTLP_PROTOCOL=grpc
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer <ingest-token>"   # if auth.ingest_tokens is set
export OTEL_SERVICE_NAME=checkout
```

### With an OpenTelemetry Collector in front (recommended for zero loss)

otel-ducklake buffers in memory ([ADR 0003](adr/0003-ingest-durability.md)).
If you can't lose even a few seconds of data on a crash, put a Collector with
a persistent queue in front. It also gives you tail sampling, attribute
scrubbing, and so on.

```yaml
extensions:
  file_storage:
    directory: /var/lib/otelcol/queue
receivers:
  otlp:
    protocols: {grpc: {}, http: {}}
processors:
  batch: {}
exporters:
  otlphttp:
    endpoint: http://otel-ducklake:4318
    compression: gzip
    sending_queue:
      enabled: true
      storage: file_storage      # survives restarts of both sides
    retry_on_failure:
      enabled: true
      max_elapsed_time: 0        # retry forever
service:
  extensions: [file_storage]
  pipelines:
    traces:  {receivers: [otlp], processors: [batch], exporters: [otlphttp]}
    logs:    {receivers: [otlp], processors: [batch], exporters: [otlphttp]}
    metrics: {receivers: [otlp], processors: [batch], exporters: [otlphttp]}
```

## TLS

The server speaks plain HTTP and gRPC. Terminate TLS in front of it: an
ingress, a load balancer, a service mesh, or the Collector above.

## CI and releases

- `.github/workflows/ci.yml` (pushes and PRs): fmt, clippy `-D warnings`,
  tests, `helm lint` for both modes, Docker build.
- `.github/workflows/release.yml` (tags `v*`): multi-arch image to
  `ghcr.io/<owner>/otel-ducklake` (`latest` only for non-prerelease tags);
  binaries for linux amd64/arm64 (glibc ≥ 2.35) and macOS arm64; the
  packaged chart, attached to the GitHub release. After the first release,
  make the GHCR package public (or use `imagePullSecrets`).
