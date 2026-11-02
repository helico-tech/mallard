---
status: todo
type: bug
created: 2026-11-01
---
# A metric point without a value is accepted and stored as NULL

## Problem
A `NumberDataPoint` with no value, or with one that cannot be read (`"asDouble": "abc"`),
gets `partialSuccess: null` and a row with value NULL. The metric then shows in
`GET /api/v1/metrics`, but a query for it returns no series. OTLP calls such a point
invalid, so the sender should hear about it, as it does for a metric with no data
(`rejectedDataPoints`, "1 metrics had no data"). The same goes for an exemplar without
a value.

## How to reproduce
Post OTLP/JSON to `/v1/metrics` with `{"name":"m","gauge":{"dataPoints":[{"timeUnixNano":"<now>","asDouble":"abc"}]}}`.
The reply has no rejection. Read it back with `/api/v1/metrics/query?name=m&since=15m&agg=max`:
the `series` list is empty.

## Notes
Found during issue 0138. `otlp/convert.rs` appends `number_value(p.value.as_ref())` as it is.
