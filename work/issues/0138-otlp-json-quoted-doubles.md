---
status: done
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

Done. Wider than filed: `Metric.data`, `NumberDataPoint.value` and `Exemplar.value`
are flattened `Option`s, and a value they cannot read becomes `None` with no error.
So a quoted `asDouble` loses the point's value (stored as NULL), a quoted exemplar
loses that exemplar's value, and a quoted `sum`, `min`, `max`, `explicitBounds`,
`zeroThreshold`, `quantile` or `value`, or a quoted `doubleValue` in an attribute of a
point, loses the whole metric ("1 metrics had no data"). All were reproduced over HTTP.

Fix: no search of the body. After the plain decoder, `maybe_lost_values` looks for a
metric without data, or a point or exemplar without a value, and only then decodes
again leniently. This replaces the search for `NaN"` and `Infinity"`, so `memchr` left
this crate. Spans and logs have no such `Option`: there a quoted double is a decode
error, which already took the lenient path.

Speed (release build, median of 201 runs, alternating; `decode` before and after, in
one binary): metrics 789 KiB 2575 -> 2546 us (-1.1%), metrics 36 KiB 124 -> 123 us
(-1.3%), traces 1444 KiB 2958 -> 2909 us (-1.7%), logs 1165 KiB 2464 -> 2399 us (-2.6%).
A metrics body with a `"NaN"` now pays for the plain decoder first: 5.1 -> 8.1 ms for
789 KiB. A log that says `NaN"` in its text is no longer slow: 5.9 -> 2.5 ms.

The unknown key was not real: it never failed on 0.33.1. `deserialize_from_value` does
skip the key without reading its value, but it runs behind `flatten`, whose map access
allows that. Checked over HTTP on the old decoder: before, after, only an unknown key,
nested in an array, in a metric attribute, and on the lenient path. The regression test
stays, because this depends on how the library is built.
