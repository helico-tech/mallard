---
status: done
created: 2026-10-31
---
# 06 — Docker and Helm

## Goal
Same getting-started experience as the sibling repos, for the combined platform.

## Acceptance criteria
- Dockerfile (multi-stage, non-root).
- docker-compose with MinIO and Postgres catalog.
- Helm chart; helm lint and helm template pass.

## Plan

## Progress log
- 2026-11-01: Dockerfile (named build contexts for the sibling repos, offline extensions), compose (RustFS + Postgres), Helm chart (ConfigMap TOML + env secrets). helm lint ok for 3 CI value sets; rendered TOML passes `obs-ducklake check`. Compose verified: Postgres catalog + s3:// data, error/span/log joined by trace id. Bugs fixed on the way: `OBS_*` names without `__` broke config loading; image env overrode the config file's storage.

- 2026-11-01: Rebuilt on pinned libs (ADR 0003); `/readyz` forwards to sentry's writer check. Not verified on a real cluster (helm lint + template + config check only).

## Follow-ups
