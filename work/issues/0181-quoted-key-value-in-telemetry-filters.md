---
status: done
type: question
created: 2026-11-02
---
# A quoted `"a=b"` in the telemetry filters is still a filter

## Problem
Issue 0180 made a quoted `"a=b"` plain text in the issue search, as the owner decided. The filter bars of
traces and logs read it the old way, in the TUI (`mallard-otel-cli/src/tui/query.rs`) and the web UI
(`parseTraceFilter`, `parseLogFilter`): it is an attribute filter. On logs, text with an `=` in it can only be
searched with `logs -q`. On traces, plain words are an error, so text would have no meaning there.

The suggestions already treat such a word as free text in both places, so the list and the filter disagree.

## How to reproduce
In the logs filter, type `"a=b"`. The request has the attribute filter `a=b` and no `q`.

## Notes
A choice for the owner: the same rule for logs, and for traces an error like other words. `setToken` and
`cycleLevel` also drop or ignore quotes, so they need the same look.

**The owner decided (2026-11-03): the same rule as the issue search.** A word with a quote before its `=` is a bare word. In logs
that is text, in traces an error like other words. `a="b c"` stays a filter.

Changed in `tui/query.rs` and `tui/views/logs.rs`, and in `lib/query.ts`. What rewrites a filter (`cycle_level`, `without_service`,
`cycleLevel`, `setToken`, `toggleErrors`) now keeps words as typed. It removed the quotes before, which would turn `"a=b"` into a
filter on the next read, and `cycleLevel` also saw a `level=` inside a quoted phrase.

Found on the way: `TraceFilter::to_query` wrote `a="b c"` as `a=b c`, so opening the prompt on it gave a filter that did not parse. Fixed too.
