---
status: done
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

Done: a gauge or sum point with no value now counts in `rejectedDataPoints` and is not stored, unless its `no recorded
value` flag is set, which is how a sender marks a series as gone. An exemplar with no value is left out and named in
the message; its point stays, so it does not count as rejected. The `asDouble` key in a body and how it is decoded did
not change. Tested in `convert.rs`; the reply over HTTP was not checked by a separate test.
