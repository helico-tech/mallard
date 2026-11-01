---
status: todo
type: bug
created: 2026-11-01
---
# OTLP/JSON: quoted finite doubles and unknown keys in our decoder

## Problem
`crates/mallard-otel-server/src/otlp/json.rs` only takes its lenient path for bodies
with `NaN"` or `Infinity"`, or after a decode error. Two cases still go wrong:

- A quoted finite `asDouble` (`"1.5"`) on a metric point or exemplar is valid
  protojson, decodes without an error, and loses its value.
- An unknown key inside an `AnyValue` fails the whole request.

## How to reproduce
Post OTLP/JSON metrics to `/v1/metrics` with `"asDouble": "1.5"`, then read the metric back.

## Notes
The library will stay as it is: issue 0012 is closed without an upstream report, so
the fix belongs in our decoder. Check the cost on the fast path; ingest speed matters here.
