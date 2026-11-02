---
status: done
type: debt
created: 2026-11-02
---
# Stopping the server has no deadline

## Problem
`Platform::serve_on` waits for all receivers to stop, and a graceful stop waits for requests in flight. A client
that holds a request open (a slow upload) therefore holds up the exit, with no limit. With `SIGTERM` the platform
kills the process after its grace period. Since issue 0130 the server also stops by itself, when the catalog is
invalidated, and then nobody kills it: it stays down until the client lets go. It acknowledges nothing in that time.

From reading the code; not reproduced.

## Notes
A limit in `commands::serve` after the stop has begun, for example 60 s (`docs/deployment.md` asks for at least
that on `SIGTERM`). Exiting past it can crash on a thread inside DuckDB (issue 0018), which is still an exit.

Done: a stop waits at most 20 s for requests in flight (`RECEIVERS_GRACE` in `mallard-server`), then stops the
receivers and still does the final flush. So the limit is on the wait, not a hard exit, and no thread is left
inside DuckDB. Test: `a_request_held_open_does_not_hold_up_the_stop`, which times out with the limit set far away.
