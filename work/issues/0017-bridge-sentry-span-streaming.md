---
status: done
type: bug
created: 2026-11-01
---
# Sentry SDK 11 span streaming is not bridged: no traces from @sentry/node

## Problem
`@sentry/node` 11 streams spans by default (`traceLifecycle: "stream"`, the
`SpanStreaming` integration): it sends envelope items of type `span`
(`content_type: application/vnd.sentry.items.span.v2+json`, several spans
per item) instead of one `transaction` item per root span. The bridge
(ADR 0009) only converts `transaction` items, so an app on the SDK's
defaults gets its errors stored but no spans in `otel.spans`: the trace
views show nothing.

Found by the tracing e2e test (issue 0006).

## How to reproduce
`cd e2e/sentry && node --test tests/node-tracing.test.mjs`: the test
"span streaming (SDK 11 default)" is marked `todo` and times out waiting
for the trace. The same app with `traceLifecycle: "static"` passes.

## Notes
- sdl-server must hand `span` items to the host (like `TransactionSink`),
  and the bridge must convert the v2 span format (span JSON with
  `trace_id`, `span_id`, `parent_span_id`, `name`, `status`,
  `start_timestamp`/`end_timestamp`, typed `attributes`; segment info in
  attributes such as `sentry.segment.name`).
- In stream mode the SDK ties a captured error to the span it was thrown
  in, the same as in static mode once the trace is sampled.
- When fixed, drop `todo` from the test (and consider making `stream` the
  main case, since it is the SDK default).

Done (ADR 0013): `Server::set_span_sink` in sdl-server, `bridge/span.rs` in
mallard-server. `span` v2 items take the transactions path: same queue, same
`dedupe_window`, same OTLP span dedupe, same `[bridge] transactions` switch;
metric `mallard_bridge_span_items_total`. Standalone sdl-server without a
sink keeps dropping and counting them. Stream mode is now the main e2e case,
static stays covered. Older standalone (v1) `span` items are counted as
`unsupported`: issue 0040. v2 spans carry no `server_name` or `platform`, so
`host.name` and `telemetry.sdk.language` stay empty on these spans.
