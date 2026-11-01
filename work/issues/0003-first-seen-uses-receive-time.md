---
status: todo
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
