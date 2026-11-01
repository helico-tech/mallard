---
status: todo
type: debt
created: 2026-11-01
---
# Standalone paths left in the side crates

## Problem
Story 0002/11 removed `Standalone` from the Sentry server. Some code from the old
binaries is still there because something still reaches it:

- `mallard_otel_server::Server` still binds its own listeners from
  `config.server` (`api_addr` and so on). Only tests use that; mallard uses the
  routers. `mallard_otel_server::config::{ServerConfig, AuthConfig}` exist for it.
- `tui::run_standalone` (telemetry) and `tui::run` (Sentry) run one side's TUI in
  its own terminal. They are reached only through the hidden aliases
  `mallard telemetry tui` and `mallard errors tui`; `mallard tui` does not use them.
- `ERRORS_HINTS` in `crates/mallard/src/main.rs` now equals `mallard_sentry_cli::HINTS`.

## Notes
None of it is wrong, it is only more code than the product needs. Decide when the
hidden aliases go.
