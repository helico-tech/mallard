---
status: done
type: debt
created: 2026-11-01
---
# 0051. OTel maintenance and catalog-wide cleanup run on every replica

## Problem
The Sentry side runs maintenance in one process, by a lease in its `meta` table
(ADR 0021). The otel side still runs it on every replica, with jitter. It is safe
but wasteful: flushes and merges conflict and roll back, and two concurrent
`ducklake_cleanup_old_files` fail with "Failed to delete scheduled cleanup files"
(seen on Postgres, 2026-11-01). In mallard, otel owns catalog-wide expiry and
cleanup (ADR 0001), so every pod does it.

## Notes
- The same lease would work: a row replaced in one transaction, in `otel.*`.
- `try_lease` lives in `sdl-server/src/db/maintenance.rs`; move it to
  `mallard-duck` when the second user comes (ADR 0005).

Fixed 2026-11-01. `try_lease` moved to `mallard-duck` (it takes the `meta` table's
name); otel has its own `otel.meta` with the row, created at start-up. Before each
task the loop tries the lease, valid for 3 of the shortest interval. The jitter is
gone: it only spread runs that now do not overlap.

The one-shot `mallard-server maintenance` ignores the lease, as the Sentry side
does: it runs next to a holder, and a lost conflict is an error that is safe to
retry. A graceful stop does not release the lease, also as on the Sentry side, so
a restart can pause maintenance for up to the lease (30 minutes by default).
