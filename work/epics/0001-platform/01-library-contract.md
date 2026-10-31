---
status: in-progress
created: 2026-10-31
---
# 01 — Library contract with sentry-ducklake and otel-ducklake

## Goal
Agree on and receive a library API from both sibling repos that lets one process embed them.

## Acceptance criteria
- Both expose a lib target with a server struct, routers/services, background jobs and shutdown.
- Config buildable in code; no global side effects.
- Both can share one catalog using separate schemas.
- Typed client crates exist for the CLI/TUI.

## Plan
Ask both sessions for a lib target; agree on shared connection (ADR 0001); wait for compile + tests; review their public API.

## Progress log
- 2026-10-31: Requested lib API from both. otel agreed: `Server::with_connection`, `otlp_http_router`, `api_router`, `grpc_routes`, `run_background`, `Config::load_with_prefix`, crate `odl-client`; catalog alias `lake`, schema `otel`, `maintenance.catalog_wide`. trace_id UUID, span_id UBIGINT. Asked sentry to mirror; awaiting reply.
- 2026-10-31: sentry agreed: lib `Server::new|with_connection`, `router()` (relative paths incl. /v1 mgmt API → must nest under /sentry), `shutdown()`, config `lake_alias`/`schema`, `public_url` may carry a path, `sdl-client`. ETA lib 1–2h. Open: trace_id UUID vs VARCHAR, catalog-wide maintenance flag, background job shutdown.
- 2026-10-31: sentry resolved all open points: `ingest_router`/`api_router`/`router`, `run_background(shutdown)`, `Config::from_env_prefix`, `catalog_wide_maintenance`, trace_id UUID + span_id UBIGINT, `storage_url: Option` for sourcemaps. Risk: two writers (sentry + otel) on one catalog only tested separately → cover in story 07 e2e (concurrent ingest into both).

## Follow-ups
