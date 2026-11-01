# 0010 — Spans sent both as Sentry transactions and over OTLP are stored once

## Status
accepted (amends 0009: Sentry transactions now wait `dedupe_window`)

## Context
Since ADR 0009 the bridge stores Sentry transactions as OTel spans. A
Sentry SDK built on OpenTelemetry (e.g. @sentry/node 8+) uses the OTel
span ids, so an app that also exports OTLP sends each span twice with the
same `trace_id` and `span_id`: `otel.spans` got both rows and trace views,
span search and RED numbers counted them twice (issue 0005).

The two copies arrive in either order: OTLP exporters batch (5 s by
default in the JS SDK) and child spans are exported when they end, while
the Sentry transaction is sent when its root span ends. Tables are
append-only, so a row cannot be removed once written.

Options:
- Ingest time, one way (drop a Sentry span when OTLP already sent it):
  simple, but misses the case where the Sentry copy comes first.
- Ingest time, both ways (hold transactions for a window, then drop the
  spans OTLP sent): correct in both orders within the window; delays
  Sentry-derived spans by the window.
- Query time (keep both rows, pick one per id when reading): survives
  restarts and replicas, but every span query in odl-server (trace view,
  search, services, operations, series, raw SQL) would need a
  per-(trace, span) dedupe over large windows, which is slow, and the
  OTel library would need to know which rows came from Sentry.

## Decision
Ingest time, both ways, in `mallard-server`'s bridge with the existing
hooks (no library change):

- The `IngestHook` (`traces`) records the (trace_id, span_id) of every
  span received over OTLP in an in-memory set of two generations. A new
  generation starts every 5 minutes or after 500 000 ids; the one before
  is dropped. So an id is kept at least 5 minutes, or 500 000 OTLP spans,
  whichever comes first (at most 1 M ids, about 50 MB).
- The `TransactionSink` queues each Sentry transaction for
  `dedupe_window` (the same key as for exceptions, default 10 s; 50 000
  transactions or 256 MiB max, more are dropped as `busy`). Then it is
  converted, the spans whose ids are in the OTLP set are removed, and the
  rest is buffered with `ingest_traces`.
- The OTLP copy wins: it has the real `service.name`, resource and OTel
  attributes; the Sentry copy only has the project slug as service.
- Spans that only Sentry sent (e.g. Sentry-only instrumentation) are kept.
- On shutdown the queue is converted and stored without waiting, after
  the receivers stop and before otel's final flush.

Metrics: `mallard_bridge_transactions_total{result=duplicate}` when all
spans of a transaction were dropped, `mallard_bridge_duplicate_spans_total`
for each dropped span.

## Consequences
- In both arrival orders each span is stored once, as long as the OTLP
  copy arrives within `dedupe_window` after the Sentry transaction, or
  less than 5 minutes (or 500 000 OTLP spans) before it.
- Spans from Sentry transactions show up `dedupe_window` (10 s) later.
  `dedupe_window = "0s"` stores them at once but then only catches the
  OTLP-first order.
- Still in memory and per process: an OTLP copy that went to another
  replica, or arrived before a restart, is not seen, and both copies are
  stored. An OTLP copy that arrives later than `dedupe_window` after the
  Sentry transaction is stored too (append-only tables).
- At very high OTLP rates the 500 000-id cap shortens how long ids are
  kept (e.g. 50 s at 10 000 spans/s): a request that runs longer than
  that can get its early child spans twice.
