---
status: done
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

## Done: the ones that could mislead (2026-11-02)
All made the same as the TUI, and "1 Sentry error" reads right: Cmd-K stays open on one match with a warning, severity names, the
parser details. The failed Sentry lookup already had a message (the list's title), so that row is now marked intended.
The other **differs** rows in `docs/web/parity.md` are still open; they are small and need a decision each.

## Done: deep lists hold still
`useInfiniteQuery` with `refetchInterval` read every loaded page again every 5 s, with no end to how
many. Counted in a browser on 2026-11-02 on the demo data, requests to `/traces` in 11 s: one page 2
reads, two pages 4 reads (2 with a cursor). The owner chose the TUI's rule (issue 0191): past the
first page no auto refresh, and `r` reads all pages. `firstPageOnly` in `components/list.tsx`.

## Done: the other rows (2026-11-03)
The owner chose to mark them intended, each with a reason in `docs/web/parity.md`; any can change later if someone trips on it. Each was
checked against the source first. Two no longer differed and are now **ok**: frame after `a` `w`, and back on after `F`. One had an old
description: the project picker's list also reloads every 30 s.
