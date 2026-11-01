---
status: done
type: question
created: 2026-11-01
---
# Issue first/last seen and trends use receive time, not event time

## Problem
Events sent with a past `timestamp` (e.g. replayed, or `scripts/demo-seed.py`)
show "first seen 49s ago" while the event itself is 50 minutes old, and the
24h trend puts all of them in the last bucket. Sentry uses the event's
timestamp for these.

## Notes
Check `sdl-server` issue aggregation (`first_seen`, `last_seen`, trend
buckets): received_at vs timestamp. Receive time is safer against clients
with wrong clocks; Sentry clamps timestamps to a window instead.

### What changed (done)
- Clamp window: ingest (`sdl-protocol` `normalize_event`) replaces an event
  `timestamp` more than 60 s after or 30 days before the receive time with
  the receive time. Constants `MAX_SECS_IN_FUTURE` (60, Relay's
  `max_secs_in_future` default; was 5 min) and `MAX_SECS_IN_PAST`
  (30 days, unchanged; Relay uses the retention). No new config.
- Writer: a new issue's `first_seen` is the earliest event `timestamp` of
  that group in the batch (was the receive time).
- Read (`db/read.rs`): issue list, issue detail, trends, tag facets and
  project stats window, bucket and pick "latest" by `timestamp`. DuckLake's
  per-file min/max on `timestamp` still prunes, since `timestamp` is clamped
  to near `received_at`.
- Kept on receive time: partitioning and sort order, retention, event
  lists and their cursor, trace lookups, event lookup by id, and the
  `issue_activity` log (regressions at write time, because status is
  `arg_max(status, changed_at)`).
- Stored data: no migration. `events.timestamp` was always stored clamped,
  so old events are counted by event time at once. Old `issues.first_seen`
  rows keep their receive time; for live data that is seconds off.
- Docs: `docs/sentry/data-model.md` (rule 4, queries table),
  `docs/sentry/compatibility.md`.
- Tests: `db::tests::issues_and_stats_use_event_time` (50 min old events:
  first/last seen = event time, trend and stats buckets by event time; an
  event one day in the future: clamped to receive time, last bucket);
  `event::tests::timestamps_outside_the_window_get_the_receive_time`.
- Follow-ups (clock drift via `sent_at`, regressions from old events,
  lowering first_seen, event lists by event time): issue 0007.
