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

## Follow-ups
