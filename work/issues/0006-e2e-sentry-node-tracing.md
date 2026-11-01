---
status: done
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

Done (2026-11-01):
- The whole suite now runs against `mallard-server` (story 0002/06), so no
  separate `lib/mallard.mjs`: `lib/server.mjs` starts it with a temp
  config; `server.otel(...)` calls `/otel/...`.
- `tests/node-tracing.test.mjs` + `fixtures/node-tracing-app.mjs`: plain
  `node:http` service (no Express dependency), route throws inside a child
  span, called by a traced fetch. Asserts the spans in
  `/otel/api/v1/traces/{id}` (parents, kind, status, `sentry.op`, service,
  release, environment, the caller's fetch span as the request span's
  parent) and the error's `trace_id`/`span_id`: the SDK ties the error to
  the span it was thrown in ("charge card"), whose parent is the request
  span.
- Found: @sentry/node 11 streams spans by default (`span` v2 items), which
  the bridge drops. The test uses `traceLifecycle: "static"`; the default
  is a `todo` test. [issue 0017](0017-bridge-sentry-span-streaming.md).
- Also: an unsampled caller (fetch outside a span) makes the server inherit
  "not sampled" in static mode, hence the "checkout client" span.

