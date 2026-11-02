---
status: done
type: idea
created: 2026-11-02
---
# 0191 The TUI catches up with the Web UI

## Problem
The Web UI gained things the TUI did not have. Agreed with mallard-core on 2026-11-02:
- Build: find an id (`ctrl-k`), more rows than 200 on the issue and trace lists, what the
  server allows on Status.
- Skip, recorded as intended in `docs/web/parity.md`: the app home page (a page across both
  sides costs more than it gives, errors are one key away) and pausing all refresh (a concern
  of a browser left open).

## Notes
- Find lives in the host (`crates/mallard/src/tui/find.rs`), above both tabs. The tabs on their
  own do not have it.
- A warning from `/api/v1/find` stops the landing and is shown; a failed read is never "nothing
  found" (issue 0143).
- Choice: the lists grow their `limit` instead of following a cursor, about 15 lines per tab. The
  cost is an end at what the server gives in one answer (500 issues, 1000 traces), which the TUI
  says out loud. The web UI has no such end.
- Choice, proposed by mallard-core: past the first page there is no auto refresh, until `r` or the
  list starts again, and the title says so. Reading 1000 traces and a Sentry lookup of all those ids
  every 5 s was never measured, and it runs on the one shared connection next to ingest. Rows also
  hold still while someone reads deep in a list. The cost: a deep list goes stale until `r`.
