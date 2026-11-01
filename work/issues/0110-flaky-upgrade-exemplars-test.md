---
status: todo
type: bug
created: 2026-11-01
---
# 0110. `upgrade_adds_exemplars_to_an_old_catalog` fails about 1 run in 10

## Problem
`crates/odl-server/tests/e2e.rs` fails with `left: [0.0], right: [0.0, 1.0]` ("the
old row is still read"). `cargo test --workspace` can fail on it for no real reason.
It is not new: 4 of 40 runs failed before the lease change of issue 0051, 6 of 40
after it.

## How to reproduce
Run the test in a loop:
`cargo test -p odl-server --test e2e upgrade_adds_exemplars_to_an_old_catalog`.

## Notes
Likely cause, from reading the code (not proven by a fix yet): `GET
/api/v1/metrics/query` reads the points and then, in a second query, the exemplars
(`api/mod.rs`). When the flusher commits between the two, the answer has the new
exemplar but not the new point, and the test's wait loop stops at the first answer
with an exemplar. The loop should wait until both points are there too.

The API can therefore show an exemplar for a point it does not list yet. That is
harmless for a moment and gone at the next refresh, so only the test needs a change.
