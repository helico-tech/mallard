---
status: done
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

2026-11-01, done (ADR 0010): ingest-time dedupe in both directions, in
the bridge, no library change. The OTLP `IngestHook` remembers the
(trace_id, span_id) of every OTLP span (two in-memory generations: 5 min
or 500 000 ids each). Sentry transactions now wait `dedupe_window` (same
key, default 10 s) in a queue; then the spans whose ids came over OTLP are
removed and the rest is stored. The OTLP copy wins (real service name and
attributes). Query-time dedupe was rejected: every span query (trace view,
search, RED, series) would need a per-id dedupe over large windows.
Regression tests: `spans_sent_both_ways_otlp_first` and
`spans_sent_both_ways_sentry_first` in `crates/mallard-server/tests/bridge.rs`.

Limits: per process and in memory, so a copy that arrived before a restart
(or at another replica) is not seen; an OTLP copy that comes more than
`dedupe_window` after the Sentry transaction is stored too; at very high
OTLP rates the 500 000-id cap shortens how long ids are kept, so very long
requests can get early child spans twice. Sentry-derived spans show up
`dedupe_window` later than before.
