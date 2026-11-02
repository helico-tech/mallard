---
status: todo
type: bug
created: 2026-11-02
---
# `a:b=c` in the issue search differs between the TUI and the web UI

## Problem
The TUI reads a word whose key has a `:` as text. The web UI makes it the tag filter `a:b:c`, which the
server splits at the first `:`, so it filters on the tag `a` with the value `b:c`. That is not what was typed.

## How to reproduce
Search issues for `a:b=c` in both. Seen in `split_filter` and `parseIssueFilter`; not run in a browser.

## Notes
Found while doing issue 0180. The likely fix is the TUI's rule in `parseIssueFilter`. A tag key with a `:`
cannot be asked for through `key:value` either way.
