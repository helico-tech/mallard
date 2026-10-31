---
status: in-progress
created: 2026-10-31
---
# Epic 0001 — Combined observability platform

## Goal
One server, one CLI, one TUI and one set of Docker/Helm files that run
sentry-ducklake and otel-ducklake together on one DuckLake catalog.

## Scope
- Server embedding both libraries, shared storage, correlation API.
- CLI and TUI over both.
- Dockerfile, docker-compose (with MinIO + Postgres), Helm chart, matching
  what the two sibling repos ship.

Out of scope: new ingest features (belong in the libraries), a web UI.

## Done when
- `docker compose up` accepts a Sentry event and an OTLP trace with the same
  trace id, and `obs trace <id>` shows both.
- Helm chart installs and passes `helm lint` / `helm template`.
- `cargo test --workspace` passes, including an end-to-end test.

## Dependencies
Waiting on library targets from both sibling repos (requested 2026-10-31).

## Stories
1. [01-library-contract](01-library-contract.md)
2. [02-server](02-server.md)
3. [03-correlation](03-correlation.md)
4. [04-cli](04-cli.md)
5. [05-tui](05-tui.md)
6. [06-deploy](06-deploy.md)
7. [07-e2e](07-e2e.md)
