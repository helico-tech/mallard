---
status: todo
type: debt
created: 2026-11-01
---
# Install the Helm chart on a real cluster

## Problem
The chart passes `helm lint`/`helm template`, and its rendered config passes
`obs-ducklake check`, but it was never installed (no kind/k3d here). Probes,
PVC permissions with readOnlyRootFilesystem, and the gRPC service are untested.

## Notes
Use kind + the `ci/duckdb-values.yaml` and `ci/minio-values.yaml` value sets.
