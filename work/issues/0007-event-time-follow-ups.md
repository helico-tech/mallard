---
status: done
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

### What changed (done)
1. **Clock drift: done.** The envelope's `sent_at` differing 55 minutes or more
   from the receive time shifts the event `timestamp` and breadcrumb timestamps
   by that difference, before the range check (`sdl-protocol`:
   `MIN_CLOCK_DRIFT_SECS`, `NormalizeContext::sent_at`). Not spans: errors rarely
   have them, and transactions go to the bridge unchanged. Not `/store/`: it has
   no `sent_at`.
2. **Old events: done.** A resolved issue reopens only from an event whose
   `timestamp` is not older than the resolve. The writer's cache holds the time
   of the last status change (`IssueState`). The activity row stays at write
   time. A client whose clock is off by under 55 minutes can miss a reopen for
   that long after a resolve; accepted.
3. **first_seen: done.** Issue list, detail, `sort=first_seen` and `new=true` read
   `least(first_seen, min(timestamp))`, with the minimum of the queried window:
   cheap, and an event outside the window does not move it. "New issues" in stats
   and `new_issues` per release still read the `issues` row.
4. **Event lists by event time: not changed, on purpose.** They stay ordered by
   `received_at`, with their cursor on it. A late event gets a new `received_at`,
   so it appears on top and never behind a cursor; by event time it would land
   between pages already read, and pages would skip or repeat. The sort order of
   the files also serves `received_at`. Each event still shows its `timestamp`.

Tests: `event::tests::clock_drift_shifts_event_and_breadcrumbs`,
`db::tests::only_events_after_the_resolve_reopen_an_issue`,
`older_events_lower_first_seen_when_read`, integration test
`envelope_sent_at_corrects_the_client_clock`.
