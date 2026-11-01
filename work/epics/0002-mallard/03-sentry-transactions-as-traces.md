---
status: in-progress
created: 2026-11-01
---
# 03 — Sentry transactions and spans become traces

## Goal
Sentry SDK performance data (transaction events with spans) is stored as OTel spans in otel.spans, so apps that only use a Sentry SDK get traces next to their errors. Today sdl-server drops transactions.

## Acceptance criteria
- A transaction envelope from @sentry/node shows up in `mallard telemetry traces` and `mallard trace <id>` with its child spans, status and attributes.
- Errors from the same request appear under the right span.
- Mapping documented (op/description/status/tags/data → OTel fields).

## Plan
- sdl-server: optional `TransactionSink` hook. Envelope `transaction`
  items (and legacy `/store/` transactions) go to the sink as raw bytes
  with project id and slug; without a sink they are dropped as before.
- odl-server: `Server::ingest_traces(ExportTraceServiceRequest)`, an
  in-process entry into the same write buffer as OTLP (no HTTP).
- mallard-server `bridge` module: Sentry transaction JSON → one OTLP
  `ResourceSpans` (root span from `contexts.trace`, one span per `spans[]`
  item), pushed on a blocking thread. Buffer full → drop + counter; Sentry
  ingest never waits.
- Config `[bridge] transactions = true` (off switch), example config,
  Helm values, ADR 0009 with the mapping table.
- Tests: unit tests on a @sentry/node transaction; integration test in
  `crates/mallard-server/tests/bridge.rs`.

## Progress log

## Follow-ups
