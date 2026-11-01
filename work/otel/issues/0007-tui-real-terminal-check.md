---
status: done
type: question
created: 2026-11-01
---
# TUI colors not checked in a real terminal

## Problem
Rendering was verified with ratatui TestBackend (text only). Colors and truecolor fallbacks (Terminal.app) were not checked visually.

## Notes
Found during the v0.1 build and review (epic 0001).

Resolved 2026-11-01: the worry was truecolor RGB degrading on terminals
without it (Terminal.app). Since the shared theme (ADR 0006,
`mallard-term/src/tui/theme.rs`) every TUI colour is a 256-colour index
(`Color::Indexed`); no `Color::Rgb` is left in the TUIs. 256 colours work
in Terminal.app, iTerm2 and every common terminal. The README screenshots
(`scripts/screenshots.sh`) render the real TUI buffer with the xterm-256
palette. Not done: a look at a physical Terminal.app window.
