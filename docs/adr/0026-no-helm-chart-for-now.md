# 0026 — No Helm chart for now

## Status
accepted

## Context
The repo had a Helm chart in `deploy/helm/mallard` and a test that installed it on a kind
cluster. Nothing published the image or the chart (issue 0124), so a plain `helm install`
could not pull its image. The owner decided to keep the chart out for now and offer Docker
containers only.

## Decision
- mallard ships as a Docker image (`deploy/docker/Dockerfile`) and a compose stack for local
  use (`deploy/compose/`). The compose defaults stay local-only.
- `deploy/helm` is removed. It is in git history: last at commit `0e5c8f2` on the branch
  that removed it, and unchanged from `75f3a09` on main. To bring it back, start from there.
- `docs/deployment.md` keeps the facts that hold on any platform: health endpoints, stop
  time, read-only root, config through variables, notes for proxies.
- `deploy/compose/test.sh` runs the compose stack in CI in place of the kind test.

## Consequences
Moved to `deploy/compose/test.sh`, with both the Postgres and the DuckDB-file catalog:

- data survives a restart: the container is replaced, and the trace and project are read back;
- the RustFS + Postgres stack, with an ingest token and a refused request without it;
- a read-only root filesystem;
- a Sentry event and an OTLP/HTTP span with one trace id in `mallard trace`, OTLP/gRPC
  with telemetrygen, `/metrics`, `mallard status` in the container.

Changed: the DuckDB-file scenario keeps its Parquet files on S3, because the compose config
fixes `data_path`; the kind test had them on the volume. A local path is still covered by
the image smoke test (no restart there) and the Rust integration tests.

Lost:

- **Two replicas.** The kind test ran two pods on one Postgres catalog. The compose stack
  has one `mallard` service and no equivalent without ports that clash, so nothing in CI now
  runs two containers against the same catalog. What remains is at the process level:
  `db::tests::two_processes_*` on a SQLite file in the normal test run; the Postgres
  variants, including the otel lease test, are ignored tests that need
  `MALLARD_TEST_POSTGRES`, and CI does not set it.
- `helm lint` and `helm template` on three values files, and the chart's refusal to render
  without `publicUrl` and a token, or with a local path and the Postgres catalog. The server
  still requires a token. It does not refuse a local path with Postgres.
- Probes, the Service and the separate metrics port on a real cluster. `server.metrics_addr`
  is still covered by the integration tests.
- Kubernetes users write their own manifests, with the facts in `docs/deployment.md`.

Issue 0124 now covers only the image and binaries. Issues 0070 and 0123 were written with
the chart in mind and are handled separately. Older ADRs and `work/` keep their mentions of
Helm as history.
