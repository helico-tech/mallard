---
status: wontfix
type: debt
created: 2026-11-01
---
# OTLP/JSON doubles: fix upstream, then drop the lenient decoder

## Problem
opentelemetry-proto 0.33.1 refuses protojson double strings (`"NaN"`,
`"Infinity"`, `"-Infinity"`, `"1.5"`) in `AnyValue.doubleValue` and drops
them silently in flattened oneofs (`NumberDataPoint`/`Exemplar` `asDouble`:
the value becomes `None`). `odl-server/src/otlp/json.rs` works around it
(otel issue 0004). Its fast path still drops a quoted finite `asDouble`
(`"1.5"`), because only bodies with `NaN"`/`Infinity"` or a decode error
take the lenient path.

Also seen in the same code: `deserialize_from_value` skips an unknown key in
an `AnyValue` without reading its value, so an unknown key there fails the
request.

## Notes
Report both upstream (opentelemetry-rust, `opentelemetry-proto/src/proto.rs`,
`serializers::deserialize_from_value` and the `asDouble` oneofs), with the
regression tests from `otlp/json.rs`. When a release fixes them, bump the
version and delete `otlp/json.rs`.

The owner decided on 2026-11-01 not to report this upstream. `otlp/json.rs` stays.
The cases it still misses are in [issue 0138](0138-otlp-json-quoted-doubles.md).
