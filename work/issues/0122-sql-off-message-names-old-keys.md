---
status: done
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

Done. `POST /otel/api/v1/sql` and `mallard sql` now say `set otel.query.enable_sql = true
in the mallard config`. Validation messages name the full key on both sides
(`otel.query.timeout`, `sentry.query.max_window`), so the `otel:` and `sentry:` context
that was in front of them is gone. `server.api_tokens` replaces `auth.api_tokens`. The
OpenAPI descriptions carry the side prefix too, and the docs are regenerated. Pinned by
`sql_off_names_the_mallard_key`, `agent_cli`, `validation_messages_name_mallard_keys`,
`sql_requires_tokens`. The Web UI's generated types were left out on purpose: see
[issue 0127](0127-regenerate-web-api-types.md).
