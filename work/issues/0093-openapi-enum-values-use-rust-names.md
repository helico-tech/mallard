---
status: todo
type: bug
created: 2026-11-01
---
# OpenAPI lists Rust names for `string_enum` values

## Problem
`docs/otel/openapi.json` lists the enum values of `string_enum!` types by
their Rust names (`Unspecified`, `Delta`, `ExponentialHistogram`). The API sends
the serde names (``, `delta`, `exponential_histogram`). A client generated from
the document would reject real answers.

## How to reproduce
Compare `Temporality` or `MetricType` in `docs/otel/openapi.json` with the
`string_enum!` list in `crates/odl-api/src/lib.rs`.

## Notes
The macro in `odl-api` puts `#[serde(rename = …)]` on each variant, which the
`ToSchema` derive seems not to pick up (cause not checked). Found while adding `mixed` (issue 0031).
