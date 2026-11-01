---
status: done
created: 2026-10-31
---
# 05 — obs TUI

## Goal
Terminal UI to browse issues, traces and logs and jump between them.

## Acceptance criteria
- Tabs: F1 Errors (sdl_cli::Embedded), F2 Telemetry (odl_cli::Embedded); jump by trace id both ways.
- Live refresh; works over the obs-client only.

## Plan

## Progress log
- 2026-11-01: Host done (`crates/obs-cli/src/tui.rs`): owns terminal + loop, `Pane` adapter per library, F-keys, jumps. Errors tab live and checked in a pty against compose. Waiting on odl_cli::Embedded for F2.

- 2026-11-01: F2 Telemetry = odl_cli::tui::Embedded (8a75d0a). Live in a pty on the compose stack: issue → event → `t` opened the otel waterfall of that trace (span + log), `e` jumped back. Host unit tests cover tab switching and jumps.

## Follow-ups
