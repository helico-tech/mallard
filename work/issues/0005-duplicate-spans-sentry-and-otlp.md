---
status: todo
type: idea
created: 2026-11-01
---
# Spans stored twice when an app sends Sentry tracing and OTLP

## Problem
Since ADR 0009, Sentry transactions are stored as OTel spans. An app whose
Sentry SDK is built on OpenTelemetry (e.g. @sentry/node 8+) and that also
exports OTLP sends the same spans (same trace and span ids) both ways, and
`otel.spans` gets each of them twice. The trace view then shows doubled
spans.

## Notes
Options: skip a Sentry span when a span with the same (trace_id, span_id)
was received over OTLP recently (in-memory set, like the exception
dedupe), or dedupe at query time in the trace view. Workaround today:
`[bridge] transactions = false` for such apps' servers.
