---
status: todo
type: debt
created: 2026-11-02
---
# The TUI components keep a mode without a host

## Problem
`errors tui` and `telemetry tui` are gone (issue 0183), so `mallard tui` is the only host. The errors tab still has
`App::hosted` with its branches (own `t`, `T` and `p` keys), and the telemetry tab has `Embedded::with_since`. Only
tests reach the other mode.

## Notes
Left out of 0183 because their tests also check the keys and screens in a host. Removing them means going through
those tests one by one.
