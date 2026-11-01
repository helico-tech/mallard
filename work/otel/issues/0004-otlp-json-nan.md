---
status: done
type: bug
created: 2026-11-01
---
# OTLP/JSON: "NaN"/"Infinity" in attribute doubleValue rejects the request

## Problem
opentelemetry-proto 0.33.1 serde rejects the protojson encodings "NaN"/"Infinity" inside AnyValue (attributes, log body). The whole request gets 400 (not retryable). Gauge asDouble "NaN" works. Fix upstream or add a custom deserializer.

## Notes
Found during the v0.1 build and review (epic 0001).

Fixed (2026-11-01). opentelemetry-proto 0.33.1 is the latest release and
still has the bug, so no version bump. New `odl-server/src/otlp/json.rs`:
the plain decoder stays the fast path; a body that contains `NaN"` or
`Infinity"`, or that the plain decoder refuses, is parsed into a
`serde_json::Value` and decoded through a small deserializer that turns
protojson double strings into `f64` under OTLP double keys (`doubleValue`,
`asDouble`, `sum`, `min`, `max`, `explicitBounds`, ...). Found on the way:
gauge `"asDouble": "NaN"` did not "work" — the plain decoder accepted it but
dropped the value (stored NULL); now it is stored as NaN. Storage: metric
values as DOUBLE nan/±inf (API returns null); attributes and log bodies as
the strings `NaN`, `Infinity`, `-Infinity` (also inside JSON arrays/maps,
before they became `null`). Documented in docs/otel/data-model.md. Tests:
`otlp::json` unit tests (attributes, log body, nested arrays/kvlists, gauge,
histogram) and e2e `json_non_finite_doubles_are_accepted`.
