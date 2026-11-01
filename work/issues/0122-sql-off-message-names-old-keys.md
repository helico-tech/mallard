---
status: todo
type: bug
created: 2026-11-01
---
# The "SQL is off" message names keys that do not exist in mallard

## Problem
With SQL off, the telemetry side answers with config keys of the old binary:

    $ mallard sql "select 1"
    error: turned off on the server: ad-hoc SQL is disabled; set query.enable_sql = true (requires auth.api_tokens)

`POST /otel/api/v1/sql` returns the same text in its 403 body. In mallard the key is
`otel.query.enable_sql`, and `auth.api_tokens` is `server.api_tokens`, which is always
required. Copying the advice gives an unknown-key error. The Sentry side already says
it right (`sentry.query.enable_sql … in the mallard config`).

## How to reproduce
`MALLARD_SERVER__API_TOKENS=dev mallard serve`, then `MALLARD_TOKEN=dev mallard sql "select 1"`.

## Notes
Found during issue 0016. The same old names are in the comments of
`mallard_otel_server::config::QueryConfig` and in validation messages
(`query.enable_sql requires auth.api_tokens`). Those are part of issue 0121's
leftovers, but this message reaches users.
