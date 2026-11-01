# 0013 — Sentry span streaming goes through the bridge too

## Status
accepted (amends 0009 and 0010: `span` items are treated like transactions);
amended by 0021 (standalone spans of older SDKs are stored too)

## Context
Since SDK 11, `@sentry/node` streams spans by default
(`traceLifecycle: "stream"`). It sends envelope items of type `span`
(`content_type: application/vnd.sentry.items.span.v2+json`, up to 1000 spans
per item) instead of one `transaction` item per local root span. The SDK
also sends gen AI spans this way when it is in static mode: it takes them out
of the transaction. sdl-server dropped these items, so apps on SDK defaults
had errors but no traces (issue 0017).

The format, read from `@sentry/core` 11.10 and from a captured envelope:

```json
{"version": 2, "items": [{
  "trace_id": "…", "span_id": "…", "parent_span_id": "…",
  "name": "charge card", "status": "error", "is_segment": false,
  "start_timestamp": 1793538736.98, "end_timestamp": 1793538736.99,
  "attributes": {"sentry.op": {"type": "string", "value": "payment.charge"}},
  "links": [{"trace_id": "…", "span_id": "…", "sampled": true, "attributes": {}}]
}]}
```

Attribute types are `string`, `boolean`, `integer`, `double` and `array`.
Each attribute can also have a `unit`.

## Decision
Same model as ADR 0009 and 0010, one hook more:

- `sdl-server`: `Server::set_span_sink` (`span` items go to a `SpanSink`
  instead of being dropped). The sink gets the payload, the item's
  `content_type` and the envelope's headers, and parses nothing. Without a
  sink the item is dropped and counted as before.
- The bridge queues `span` items in the same queue as transactions: same
  `dedupe_window`, same limits, same workers. The OTLP copy of a span wins
  (ADR 0010).
- `[bridge] transactions` switches both. They are the same data (Sentry
  tracing as OTel spans), and a separate key would only let an app lose its
  traces when it changes `traceLifecycle`.

### Sentry v2 span → OTel span

| OTel | from |
|---|---|
| `trace_id`, `span_id`, `parent_span_id` | same names; a span without a valid trace or span id, or without any timestamp, is skipped |
| `name` | `name`, else `sentry.op`, else `span` |
| `kind` | attribute `sentry.kind`, else from `sentry.op` (table in ADR 0009) |
| start / end | `start_timestamp` / `end_timestamp` (rounded to µs); a missing one takes the other's value |
| status | `ok` → OK; missing → UNSET; any other → ERROR, message = attribute `sentry.status.message`, else the status |
| attributes | all typed attributes, types kept; arrays become OTel arrays (stored as JSON text); `unit` is dropped; `sentry.segment.name`, `sentry.segment.id`, `sentry.op`, `sentry.origin` and the like stay as they are |
| links | `links[]` with `sampled` as trace flags |

Resource and scope, per span (one `ResourceSpans` per different result):

| OTel | from |
|---|---|
| `service.name` | attribute `service.name`, else the project slug |
| `service.version` | attribute `sentry.release`, else `trace.release` of the envelope header |
| `deployment.environment.name` | attribute `sentry.environment`, else `trace.environment` of the envelope header |
| scope name / version | attributes `sentry.sdk.name` / `sentry.sdk.version`, else `sdk` of the envelope header |

`sentry.release`, `sentry.environment` and the two SDK attributes are not
kept as span attributes.

Envelope header values over 200 bytes are ignored: the headers come from the
client without a size limit, and each queued item keeps a copy of these four
values. The sink borrows the headers, so one envelope with many items costs no
copy per item.

A `span` item without the v2 content type (standalone spans of older SDKs,
one span per item) is not converted: `result=unsupported`, see issue 0040.

Metric: `mallard_bridge_span_items_total{result=stored|invalid|unsupported|duplicate|busy|otel_full|too_large|error}`
per item. `mallard_bridge_spans_total` and
`mallard_bridge_duplicate_spans_total` count spans of both kinds.

## Consequences
- Apps on SDK defaults get traces with no config change; `static` and
  `stream` give the same trace. Each span of an item that is partly
  invalid is handled on its own.
- Streamed spans wait `dedupe_window`, like transactions, and have the
  same dedupe limits (ADR 0010). A long request's child spans now arrive
  before its root span ends, so its trace fills up over time.
- v2 spans carry no `server_name` or `platform`, so `host.name` and
  `telemetry.sdk.language` are not set on these spans (they are on spans
  from transactions).
- `user.*` attributes stay as the SDK names them (`user.id`, `user.email`,
  `user.ip_address`, `user.name`). There is no mapping to `client.address`
  as for transactions: the HTTP instrumentation sets `client.address` itself.
