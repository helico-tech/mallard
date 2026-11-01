---
status: done
created: 2026-11-01
---
# 09 — TUI screenshots in the README

## Goal
The GitHub README shows what the TUI looks like with realistic data.

## Acceptance criteria
- Screenshots of the errors tab, telemetry tab, a trace waterfall with an error, generated reproducibly from demo data.
- A script to regenerate them.

## Plan

## Progress log
- 2026-11-01: `scripts/demo-seed.py` (linked demo data: 5 services, 3 issues, traces/logs/metrics), ignored test `crates/mallard/src/tui_screenshots.rs` renders the real Host to SVG, `scripts/screenshots.sh` runs it all and converts to PNG (rsvg-convert). 5 screenshots in the README. Fixed on the way: TUI badges still said sentry-ducklake/otel-ducklake; issue detail showed the old `t` jump key.

## Follow-ups
