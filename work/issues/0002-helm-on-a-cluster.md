---
status: done
type: debt
created: 2026-11-01
---
# Install the Helm chart on a real cluster

## Problem
The chart passes `helm lint`/`helm template`, and its rendered config passes
`obs-ducklake check`, but it was never installed (no kind/k3d here). Probes,
PVC permissions with readOnlyRootFilesystem, and the gRPC service are untested.

## Notes
Use kind + the `deploy/helm/mallard/ci/duckdb-values.yaml` and `ci/minio-values.yaml` value sets.

Done 2026-11-01: CI's docker job now installs the built image with the
chart on kind (`deploy/helm/kind/test.sh`), in two set-ups: DuckDB catalog
on the PVC (with a restart that must keep the data) and RustFS + Postgres
catalog with two replicas and an ingest token. It checks probes under
readOnlyRootFilesystem, a Sentry event and an OTLP/HTTP span in one
`mallard trace`, OTLP/gRPC through the Service (telemetrygen), `/metrics`
and `mallard status` in the pod. Green in run 41308794931. Not covered:
ingress, ServiceMonitor, IRSA (postgres-irsa values).
