# Deployment: telemetry

How to run the server (Docker, compose, Helm, storage, catalog) is one page for both
sides: [deployment.md](../deployment.md). This page has what is specific
to telemetry.

> **Security:** the query API always needs a token (`server.api_tokens`). OTLP ingest
> is open unless you set `server.ingest_tokens`: anyone who can reach 4317, 4318 or
> `/v1/*` on 8080 can write into the lake. The server speaks plain HTTP and gRPC; see
> [TLS](#tls).

## Pointing your services at it

```sh
# any OpenTelemetry SDK
export OTEL_EXPORTER_OTLP_ENDPOINT=http://mallard:4318   # or :4317 with OTEL_EXPORTER_OTLP_PROTOCOL=grpc
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer <ingest-token>"   # if server.ingest_tokens is set
export OTEL_SERVICE_NAME=checkout
```

`mallard setup <name>` prints these for a project, with the HTTP port as endpoint
(OTLP/HTTP also answers on `/v1/*` there, so one ingress can carry everything).

## Helm values for telemetry

The chart has `retention.traces`, `.logs`, `.metrics`, `ingestToken` and
`duckdb.memoryLimit` as values. Everything else in `[otel.*]` goes through
`extraConfig`:

```yaml
extraConfig: |
  [otel.ingest]
  flush_interval = "10s"
  max_buffer_bytes = "256MB"
```

Mind the memory rule in [capacity.md](capacity.md#sizing-guide): the chart defaults
(1 GB DuckDB, the default 512 MB buffer, a 2 GiB limit) leave less than it asks for
([issue 0123](../../work/issues/0123-chart-memory-defaults-below-the-rule.md)).

Replicas: with `catalog.type=postgres` and object storage, set `replicaCount`. Each
pod ingests its own traffic; one holds the maintenance lease
([operations](operations.md#several-replicas)). CI checks a Postgres + S3 install on
kind (`deploy/helm/kind/test.sh`), with one pod.

## With an OpenTelemetry Collector in front (recommended for zero loss)

The server keeps acknowledged data in memory until the next flush
([ADR 0003](adr/0003-ingest-durability.md)). A hard crash (SIGKILL, OOM, power
loss) loses at most `otel.ingest.flush_interval` (5 s) of it. There is no built-in
write-ahead log, and none is planned ([ADR 0020](../adr/0020-no-write-ahead-log.md)).
If you can't lose those seconds, put a Collector with a persistent queue in
front. It keeps data on disk until the server has accepted it, also while the
server is down for an upgrade. You also get tail sampling, attribute scrubbing,
and so on.

Use the **contrib** distribution
(`otel/opentelemetry-collector-contrib`): `file_storage` is not in the core
one.

```yaml
extensions:
  file_storage:
    directory: /var/lib/otelcol/queue   # must exist; keep it on a volume
    fsync: true                         # also survive power loss; slower writes
receivers:
  otlp:
    protocols:
      grpc: {endpoint: 0.0.0.0:4317}
      http: {endpoint: 0.0.0.0:4318}
exporters:
  otlphttp:
    endpoint: http://mallard:4318
    compression: gzip
    # headers: {Authorization: "Bearer ${env:INGEST_TOKEN}"}   # if server.ingest_tokens is set
    sending_queue:
      enabled: true
      storage: file_storage             # the queue survives restarts of both sides
    retry_on_failure:
      enabled: true
      max_elapsed_time: 0               # retry forever
service:
  extensions: [file_storage]
  pipelines:
    traces:  {receivers: [otlp], exporters: [otlphttp]}
    logs:    {receivers: [otlp], exporters: [otlphttp]}
    metrics: {receivers: [otlp], exporters: [otlphttp]}
```

Good to know:

- **No `batch` processor in these pipelines.** It holds data in memory and
  answers the sender before the data reaches the queue, so a Collector crash
  would lose it. The server batches anyway; small requests are fine.
- The server answers 429/`UNAVAILABLE` when its buffer is full. The exporter
  retries those, and the queue absorbs the wait. A full queue
  (`sending_queue.queue_size`) makes the Collector refuse new data, and SDKs
  then retry or drop it. Size the volume for the longest outage you want to
  survive.
- Delivery is at-least-once: a crash right after a flush can send a request
  twice. Nothing removes such duplicates.
- Kubernetes: run the Collector with its own PVC (a StatefulSet per replica),
  so the queue survives a restart. The server itself needs no extra volume.

## TLS

The server speaks plain HTTP and gRPC. Terminate TLS in front of it: an
ingress, a load balancer, a service mesh, or the Collector above.
