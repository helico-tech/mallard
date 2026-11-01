---
status: done
created: 2026-11-01
---
# deployment

## Goal
Dockerfile with pre-installed extensions, compose (local; Postgres+MinIO), Helm chart (single/shared), GitHub Actions; verified by running.

## Acceptance criteria
- Built, tested (cargo test), documented.

## Plan
See epic and docs/architecture.md.

## Progress log
- 2026-11-01: created.
- 2026-11-01: Dockerfile (extensions baked in, non-root), compose (local; Postgres + S3 with 2 replicas), Helm (single/shared), CI and release workflows.
- 2026-11-01: Verified: compose stacks, SQLite catalog, offline extension loading, Helm on a local cluster (both modes).
- 2026-11-01: Review fixes: CI negative checks, auth/ingress guard, ingestAuth, localhost ports, pinned images, glibc 2.35 builds.

## Follow-ups
