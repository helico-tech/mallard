---
status: done
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

Fixed 2026-11-01. Two causes, both in `utoipa-gen` 6.0.1:
- Its serde parser only matches a plain literal token. A `$s:literal` macro
  fragment arrives wrapped in an invisible group, so `#[serde(rename = $s)]` was
  skipped (serde itself handles the group). `$s:tt` got past this.
- It treats an empty rename as no rename, so `Temporality`'s `""` still came out
  as `Unspecified`; no attribute can express it.

So `string_enum!` now writes the schema from its own list instead of deriving it.
Each `string_enum!` also makes a test named after the type: the committed document
lists exactly what serde sends. A new type cannot be left out.

That test also found `AttributeSignal` and `LogOrder` missing from the document:
query parameters pointed at `$ref`s without a schema, which a generator rejects.
`ApiDoc` registers them now.

`docs/sentry/openapi.json` does not have the problem: its only enum,
`IssueStatus`, uses `rename_all`, which utoipa reads, and it has no dangling `$ref`.
