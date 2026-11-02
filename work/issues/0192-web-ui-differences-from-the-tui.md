---
status: todo
type: debt
created: 2026-11-02
---
# 0192 Web UI: the open "differs" rows of the parity table

## Problem
`docs/web/parity.md` has rows marked **differs**: both have the feature, it behaves differently,
and nobody decided. For each: make it the same, or mark it intended with a reason. They were read
from source on 2026-11-01, so check each one still holds.

## Notes
Worth doing first, because they can mislead:
- Cmd-K lands on one match at once even when a side could not be read, so the warning goes unseen
  (the TUI shows it first).
- A failed Sentry lookup on the traces list gives no message.
- Severity names fall back differently, and the parsers of the trace filter differ in details.

Small, seen in the README screenshots: the trace page says "1 Sentry errors".

## Deep lists re-read every page
`useInfiniteQuery` with `refetchInterval` and no `maxPages` (`TracesPage.tsx`, `IssuesPage.tsx`) reads
every loaded page again every 5 s, with no end to how many. Counted in a browser on 2026-11-02 on the
demo data, requests to `/traces` in 11 s: one page 2 reads, two pages 4 reads (2 with a cursor). The
cost on a large lake is not measured. The TUI stops its auto refresh past the first page for this
reason (issue 0191); the same rule, or a measurement, is the choice here.
