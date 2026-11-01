---
status: in-progress
created: 2026-11-01
---
# Epic 0001: v0.1 — collector, query API, CLI/TUI, deployment

## Goal
A production-usable OTLP collector for small/mid companies that stores data in
DuckLake on any storage DuckLake supports, with an OpenAPI query API, CLI and
TUI, reference Docker images and a Helm chart, documented capacity.

## Done when
- OTLP gRPC + HTTP (protobuf/JSON) ingest of traces, logs, metrics
- Data model designed and measured (docs/data-model.md)
- Query API with OpenAPI; CLI; TUI
- Docker image, compose stacks (local, Postgres+MinIO), Helm chart, CI
- Capacity measured and documented
- Independent code reviews done, findings handled
- Pushed to GitHub (helico-tech), docs complete

## Stories
- [01 Server core](01-server-core.md) — done
- [02 Client and CLI](02-client-cli.md) — done
- [03 TUI](03-tui.md)
- [04 Deployment](04-deployment.md)
- [05 Capacity](05-capacity.md)
- [06 Reviews and release](06-reviews-release.md)
