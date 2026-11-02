---
status: done
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

Also for this issue: `keys::shared(hosted)` in `mallard-term` (help screens); it can lose its argument with the mode.

## Fixed (2026-11-02)
`App::hosted`, both `set_hosted`, `Embedded::with_since` and `keys::shared` are gone, and so is what only the other mode could reach: the
tabs' own `t`/`T`, the project picker overlay, "the first project", and the `No projects yet` text. `mallard tui` looks and works as before.
Tests of the removed mode went with it; tests that relied on it by default now choose an app, as the host does.

