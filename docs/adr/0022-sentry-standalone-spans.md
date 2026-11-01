# 0022 — Standalone spans of older Sentry SDKs go through the bridge too

## Status
accepted (amends 0013: a `span` item without a content type is converted)

## Context
ADR 0013 stored only v2 `span` items and counted the others as
`result=unsupported`, on the guess that they were an edge case (issue 0040).
They are not. Read from the npm packages:

- `@sentry/browser` 8.55, 9.47 and 10.78: `browserTracingIntegration` has
  `enableInp: true` by default, and each INP (slow interaction) is sent as a
  standalone span. CLS and LCP do the same behind experiment flags.
- `@sentry/core` of those versions writes these as `{"type": "span"}` with no
  content type, one span per item, in the format of a transaction's `spans[]`.
  The envelope has a `trace` header and no `sdk` header.
- SDK 11 (September 2026) sends standalone spans as v2.

So any browser app on SDK 8 to 10 with tracing on sends them by default.

## Decision
A `span` item without a content type goes through the same queue, limits,
`dedupe_window` and metric as a v2 item (ADR 0013). A `span` item with another
content type stays `result=unsupported`.

The span is converted like a `spans[]` item (table in ADR 0009), with these
differences:

| OTel | from |
|---|---|
| `parent_span_id` | `parent_span_id`, else none (there is no root to fall back to) |
| `service.name` | the project slug |
| `service.version` | `data.release`, else `trace.release` of the envelope header |
| `deployment.environment.name` | `data.environment`, else `trace.environment` of the envelope header |
| scope name / version | `sdk` of the envelope header, else `sentry` / empty |
| attribute `browser.web_vital.<name>.value` | `measurements.<name>.value`, the name SDK 11 uses for the same number |

`release` and `environment` are not kept as span attributes.

## Consequences
- Slow interactions of browser apps on SDK 8 to 10 show up in the trace views,
  and a query on `browser.web_vital.inp.value` works across SDK versions.
- The test payloads are written from the SDK's source, not captured from a
  browser: e2e installs only SDK 11.
- `measurements` of spans inside transactions are still dropped.
