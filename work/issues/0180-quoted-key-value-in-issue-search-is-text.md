---
status: done
type: bug
created: 2026-11-02
---
# A quoted `"a=b"` in the issue search should be plain text

## Problem
A search for `a=b` was always a tag filter, even in quotes, so there was no way to search issue titles
for text with an `=` in it. The owner decided: unquoted stays a tag filter, quoted is text.

## How to reproduce
In the TUI (`/`) or the web UI's issue search, type `"a=b"`. The request had `tag=a:b` and no `query`.

## Notes
The quotes were gone before anyone looked for the `=`: `tokenize` and `tokens` remove them. Both issue
searches now read the word as typed. The rule is the one `suggest.rs` already had: a word is `key=value`
only if there is no quote before the `=`. So `a="b c"` stays a tag, and `"a"=b` is text.

Where a query is read:
- TUI: `split_filter` in `mallard-sentry-cli`. Changed.
- Web UI: `parseIssueFilter` in `lib/query.ts`. Changed.
- Suggestions: `suggest.rs` already called a quoted word free text. `tokenAt` in `lib/suggest.ts` took
  the key `"a` and asked for its values, so it now says `free`, and `useSuggestions` then shows nothing.
  This also applies to the filter bars of traces and logs, like in the TUI.
- Server and `issues list -q`, `events list -q`: no splitting, the `query` parameter is one piece of
  text. Pinned by `the_query_parameter_is_plain_text`.
- `setToken` and `cycleLevel` in `lib/query.ts` only work on telemetry filters. Not changed.

Not done: the telemetry filters have the same problem, see issue 0181.
