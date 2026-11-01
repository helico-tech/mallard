# 0002 — Embed the libraries' CLI and TUI instead of writing new ones

## Status
accepted

## Context
sentry-ducklake ships `sdl` (CLI + TUI) and otel-ducklake ships `odl`
(CLI + TUI). A third CLI/TUI here would duplicate both and drift.

## Decision
Both `sdl-cli` and `odl-cli` expose a lib target with:
- a clap `Subcommand` enum plus `run(cmd, &Ctx)`. `obs` mounts them as
  `obs sentry …` and `obs otel …`;
- an `Embedded` TUI component (`draw`, `handle_event`, `tick`) with jump
  points: sentry → `Outcome::OpenTrace(id)`, otel → `Outcome::OpenErrors(id)`,
  and the reverse calls `open_trace` / `show_trace_errors`.

`obs` owns the terminal, the event loop and the tabs (F1 errors, F2
telemetry), plus the commands that span both: `obs status`,
`obs trace <id>` (errors + spans + logs), `obs tui`.

## Consequences
- Every new command or screen in a library shows up in `obs` for free.
- Both libraries must stay on the same ratatui/crossterm majors.
- F1/F2 are reserved; the components must return `Ignored` for keys they
  don't use.
