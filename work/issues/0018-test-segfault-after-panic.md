---
status: todo
type: debt
created: 2026-11-01
---
# Test binary segfaults after a failing test leaves a server running

## Problem
When a test in `crates/mallard-server/tests/agent_cli.rs` panics before
`srv.stop()`, the test process ends with SIGSEGV instead of a plain test
failure (seen 2026-11-01 with `tags_cursors_releases_and_error_sql`). Most
likely DuckDB is torn down at process exit while server threads still use
the connection.

## How to reproduce
Make any assertion before `srv.stop()` in `agent_cli.rs` fail and run
`cargo test -p mallard-server --test agent_cli`.

## Notes
Only on the failure path; green runs are clean. Fix idea: stop the server
from a `Drop` guard in the test helper, or check `mallard-server` shutdown
when the runtime drops without `stop()`.
