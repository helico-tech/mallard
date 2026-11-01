---
status: todo
type: bug
created: 2026-11-01
---
# OTLP/JSON: "NaN"/"Infinity" in attribute doubleValue rejects the request

## Problem
opentelemetry-proto 0.33.1 serde rejects the protojson encodings "NaN"/"Infinity" inside AnyValue (attributes, log body). The whole request gets 400 (not retryable). Gauge asDouble "NaN" works. Fix upstream or add a custom deserializer.

## Notes
Found during the v0.1 build and review (epic 0001).
