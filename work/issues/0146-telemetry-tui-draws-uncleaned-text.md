---
status: todo
type: bug
created: 2026-11-01
---
# Telemetry TUI draws server text without cleaning it

## Problem
ADR 0006 says every text from a server goes through `mallard_term::text::clean`. The errors tab
does it for each response (`Resp::sanitized`). The telemetry tab does it in some places only
(`widgets::trunc`). Service, operation and root span names in the overview and the trace list go
straight into a `Span` or `Cell` (`views/overview.rs`, `views/traces.rs`). ratatui drops control
characters, but a bidi override such as U+202E reaches the terminal.

## How to reproduce
Send a span whose `service.name` holds U+202E. Open F2 and look at the Services table.

## Notes
Found while building story 0004/03. New text from that story is cleaned: suggestions, the picker,
the top bar, the empty states.
