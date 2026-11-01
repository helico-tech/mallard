---
status: todo
type: bug
created: 2026-11-01
---
# Accepting a suggestion with control characters filters on the cleaned text

## Problem
`mallard_term::tui::suggest` cleans what it fetches on the way in, and Tab puts that cleaned text in
the filter bar. A service called `a<U+202E>b` is suggested as `a\u{202e}b`, and `service=a\u{202e}b`
matches nothing. So the list offers a value that gives an empty result, which looks like "no data".
Enter on the row in the Overview does send the real name (issue 0146). Both tabs share this list.

## How to reproduce
Send a span whose `service.name` holds U+202E. On F2, Traces, press `/`, type `service=`, press Tab
on the suggestion and Enter: no traces.

## Notes
On purpose today: `untrusted_text_is_cleaned` tests that the bar never gets raw text. The fix is an
`Item` that shows the clean label and inserts the raw value. The telemetry bar already draws its text
cleaned (0146); the errors tab's bar (`mallard-sentry-cli/src/tui/ui/mod.rs`) does not yet. Only names
with control or bidi characters are affected, so it is low priority.
