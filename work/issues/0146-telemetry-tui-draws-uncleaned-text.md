---
status: done
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

Fixed with one cleaning point: `tui/loader.rs`, `Data::cleaned`, on the background task of each
load. It has no wildcard, so a new kind of data must say how it is cleaned, and it uses the same
`sanitize` as the errors tab (moved to `mallard_term::text`), so a new field is covered too. Log
bodies and stack traces keep their line breaks. `short_error` cleans server error messages.

What had to stay as the server has it:
- Service, operation and metric names go back in queries, and the app's services are compared with
  them, so these rows are `Named<T>`: the row is cleaned, `raw` is what is sent. Filters and search
  text hold raw values and are cleaned where they are drawn (the bars and the prompt).
- `LogTail` compares what it polls, uncleaned, with the first page. Cleaning first would show each
  record with such text twice. The tail is now built and marked on the load's task, before cleaning.

Left out: suggestions keep inserting cleaned text, see issue 0148. Trace, span and event ids go back
too. The server stores trace and span ids as UUID and UBIGINT, so cleaning does not change them; event
and span ids of the error lookup are not cleaned in the data, and the view cleans them.

The CLI's text output was already clean; `agent_cli_app.rs` now checks it with a real server.
