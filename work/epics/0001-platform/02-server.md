---
status: todo
created: 2026-10-31
---
# 02 — obs-server binary

## Goal
One binary that runs both libraries on shared listeners and storage.

## Acceptance criteria
- One TOML config with shared [storage], [sentry], [otel].
- Sentry ingest, OTLP/HTTP and both APIs on :8080/:4318, OTLP/gRPC on :4317.
- /healthz, /readyz, /metrics; graceful shutdown.

## Plan

## Progress log

## Follow-ups
