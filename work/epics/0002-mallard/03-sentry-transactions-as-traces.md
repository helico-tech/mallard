---
status: done
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
- 2026-11-01: sdl-server `TransactionSink` (+ `/store/` transactions);
  odl-server `Server::ingest_traces`; `mallard-server/src/bridge/
  transaction.rs` with unit tests on a @sentry/node 8 transaction;
  `tests/bridge.rs::sentry_transactions_become_traces` (spans via the otel
  API, the error under the db span) and the off switch.
- Choices: service = `service.name` tag, else project slug. Not
  `server_name` (Sentry SDKs set it to the host name; it becomes
  `host.name`) and not `contexts.otel.resource` (@sentry/node always says
  `node`). `data["otel.kind"]` wins over the `op` mapping. Float
  timestamps are rounded to µs.
- Back-pressure: at most 1024 conversions in flight; busy / OTel buffer
  full / invalid → dropped, `mallard_bridge_transactions_total{result}`.
- Docs: ADR 0009 (mapping table), architecture, AGENTS, README,
  compatibility, example config, Helm values/configmap.

## Follow-ups
- [issue 0005](../../issues/0005-duplicate-spans-sentry-and-otlp.md):
  spans stored twice when an app sends Sentry tracing and OTLP.
- [issue 0006](../../issues/0006-e2e-sentry-node-tracing.md): a real
  @sentry/node tracing test in the JS e2e suite (needs a mallard harness).
