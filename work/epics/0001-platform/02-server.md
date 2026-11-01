---
status: done
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
- 2026-11-01: obs-server done: figment config (TOML + OBS_ env, ${VAR}), shared connection via odl open_connection, routers nested (/sentry, /otel) + OTLP/HTTP on :8080 and :4318, gRPC :4317, /metrics (Prometheus recorder in main only), ordered shutdown (receivers → flush). Smoke run: Sentry event, OTLP span and log with one trace id all read back; SIGTERM flushes both.

## Follow-ups
