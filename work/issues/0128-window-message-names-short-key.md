---
status: todo
type: bug
created: 2026-11-01
---
# The "time window too large" message names a key without its side

## Problem
`mallard_common::window` says `time window larger than query.max_window (...)`. In
mallard the keys are `otel.query.max_window` and `sentry.query.max_window`, so the
message names a key that does not exist. The text is shared by both sides, so the side
has to be passed in. The telemetry client and TUI tests also carry this text.

## Notes
Found during issue 0122, which covered the SQL message and the validation labels.
