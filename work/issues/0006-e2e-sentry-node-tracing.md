---
status: todo
type: debt
created: 2026-11-01
---
# JS e2e: @sentry/node tracing through mallard

## Problem
The bridge (ADR 0009) is tested with real-looking payloads in
`crates/mallard-server/tests/bridge.rs`, not with a real SDK. The JS e2e
suite (`e2e/sentry`) starts the standalone `sentry-ducklake`, which drops
transactions, so a tracing test there needs a harness for
`mallard-server` (config file, OTLP port, the otel API).

## Notes
Add `lib/mallard.mjs` (start `mallard-server` with a temp config), then a
test: @sentry/node with `tracesSampleRate: 1`, an Express route that
throws; expect the spans in `/otel/api/v1/traces/{id}` and the error under
the request's span.
