---
status: todo
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
