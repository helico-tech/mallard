---
status: todo
type: idea
created: 2026-11-01
---
# Event time: follow-ups after issue 0003

## Problem
Issue 0003 moved first/last seen, trends, tag facets and stats to the
event `timestamp`. Some related behaviour still differs from Sentry:

1. **Clock drift correction.** Relay compares the envelope header `sent_at`
   with the receive time and, when they differ by 55 minutes or more,
   shifts all timestamps of the event (event, breadcrumbs, spans) by that
   drift. We only replace an out-of-range event `timestamp` with the
   receive time, and leave breadcrumbs alone.
2. **Regressions from old events.** Any new event of a resolved issue
   reopens it, even when its `timestamp` is from before the resolve (a
   replay, an offline mobile client). Sentry does not reopen a group for an
   event older than its last seen. Needs the resolve time (or last seen) in
   the writer's issue cache. The activity row itself must stay at write
   time, since status is `arg_max(status, changed_at)`.
3. **first_seen is never lowered.** `issues.first_seen` is the earliest
   event time in the batch that created the issue. An older event that
   arrives in a later batch does not move it back (the table is
   append-only). The read side could use `least(first_seen, min(timestamp))`.
4. **Event lists by event time.** `GET .../events` and trace lookups still
   window and order by `received_at` (and the cursor is on `received_at`).
   Sentry orders by event time. Left alone to keep cursors stable.

## Notes
Relay sources: `relay-event-normalization/src/validation.rs`
(`normalize_timestamps`), `clock_drift.rs`; defaults in
`relay-config/src/config.rs` (`max_secs_in_future: 60`, past = retention).
