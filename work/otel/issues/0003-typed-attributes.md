---
status: wontfix
type: idea
created: 2026-11-01
---
# Typed or promoted attributes

## Problem
Attributes are MAP(VARCHAR, VARCHAR). Promoting frequent semantic-convention attributes (http.route, http.response.status_code, db.system.name) to typed columns, or using VARIANT with shredding, would speed up filters and keep types. See data-model.md section 5.

## Notes
Found during the v0.1 build and review (epic 0001).

Decision: keep the MAP. Measured in
[bench/otel/typed-attributes](../../../bench/otel/typed-attributes/README.md)
on 4.05 M loadgen spans, DuckDB 2.0.0 (two rounds on a busy machine, so read
the ratios).

| trace search filter, 1 h | ms |
|---|---|
| MAP, today | 224, 428 |
| column only (wrong answers: skips old rows and resource attributes) | 30, 76 |
| column with the same answers as today | 92, 323 |
| `variant_extract` on shredded VARIANT | 627, 1615 |

- **Promoted columns:** 6-11× when only the column is read, 1.3-2.4× with the
  answers the API gives today, because old rows and resource attributes still
  need the maps. Size +0.4%.
- **Little reaches the API:** the only span and log path that gains is search
  with an equality filter on exactly a promoted key. There is no numeric filter
  and no attribute group-by on spans; RED views don't read attributes. The
  larger gains (status >= 500, p95 by route) are ad-hoc SQL. Those users can
  live with `TRY_CAST`.
- **Cost:** columns are append-only and stay forever, the filter must route
  keys to columns, and "absent" needs a sentinel to tell it from an old row.
- **VARIANT:** about 2-8× slower than the MAP, no smaller, not decodable by the
  Rust `duckdb` crate. Out. (The old note that dotted keys collide with variant
  paths is not true for `variant_extract`; data-model.md is corrected.)

Reopen if a real install has attribute searches that time out; promote then
the keys that install filters on, from its numbers. The search time scales with
the rows in the window, so this is a matter for the top of the target load over
long windows. No ADR: otel ADR 0002 already says MAP, and this changes nothing.
