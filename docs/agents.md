# mallard for AI agents

`mallard` reads errors (Sentry SDKs), traces, logs and metrics (OpenTelemetry)
from one mallard server. This guide is also built in: `mallard help agents`.

## Set up once

```sh
export MALLARD_URL=http://localhost:8080   # the server
export MALLARD_TOKEN=…                     # one of server.api_tokens
export MALLARD_PROJECT=web                 # Sentry project (optional with one project)
export MALLARD_FORMAT=json                 # or pass --json / --format ndjson
mallard status                             # exit 0: both sides up
```

## Output contract

- stdout is data only; errors and notes go to stderr. Nothing prompts or pages.
- `--format text|json|ndjson` (`--json` = `--format json`, env `MALLARD_FORMAT`).
  JSON is compact when piped. Text is plain (no boxes, no colour) when piped.
- Lists are one envelope:
  `{"items":[…],"count":20,"limit":20,"truncated":true,"next_cursor":null,"window":{"from":"…","to":"…"}}`.
  `truncated` means more exist: narrow the window or filters, or raise `--limit`.
  Where `next_cursor` is set (`issues list`, `events list`), pass it as `--cursor`
  with the same filters to get the next page.
  `ndjson` prints one item per line instead (the truncation note goes to stderr).
- Single objects (`show`, `status`, `setup`) are the object itself.
- `--fields a,b,c` keeps only those top-level keys (an unknown field is exit 2 and
  the error lists the valid ones).
- Concise by default; `--raw` adds raw payloads (the Sentry event `payload`,
  log `resource_attributes`).
- Times are RFC 3339 UTC. Windows: `--since 1h`, or `--from`/`--to` as RFC 3339 or
  a duration ago (`--from 2h --to 1h`). The JSON `window` is the resolved window.
- With `--format json|ndjson`, an error is one JSON line on stderr:
  `{"error":{"code":"not_found","message":"…","status":404,"retryable":false,"hint":null}}`.
- `--quiet` drops the `note:` lines on stderr (truncation, waiting, fallbacks);
  data, warnings and errors still print. (`-q` is `--query` on several commands.)

Exit codes:

| code | meaning | what to do |
|---:|---|---|
| 0 | ok (also an empty list) | |
| 1 | internal error | report it |
| 2 | usage: bad flag, id, window or field | fix the call (see `--help`) |
| 3 | auth: missing or wrong token | set `MALLARD_TOKEN` |
| 4 | not found (or `find` found nothing) | check the id, widen `--since` |
| 5 | conflict | |
| 6 | unavailable: server down, busy, timeout; `status` with a side down | retry later |
| 7 | turned off on the server (SQL) | ask for `otel.query.enable_sql = true` (`sql --errors`: `sentry.query.enable_sql`) |
| 8 | condition not met: `--wait` or `logs -f --until-match … --for …` saw nothing in time | wait longer, check the filters |

## Recipes

**Investigate a failure** (error → stack → trace → logs):
```sh
mallard issues list --since 1h --sort count -n 5 --fields id,title,count,last_seen
mallard issues show <issue-id> --json      # latest_event: exception, in-app frames, request, trace_id
mallard trace <trace-id> --json            # spans + Sentry errors + logs of the request
mallard logs --around <event timestamp> --window 2m -S <service>
```

**Wait for data after a test run** (ingest takes a few seconds). `--wait <dur>`
works on `events list`, `traces list`, `logs`, `trace` and `find`: it polls until
there is a result, else prints the empty result and exits 8.
```sh
mallard events list --trace <trace-id> --wait 30s --json
mallard find <trace-id> --wait 30s
mallard logs -S checkout -q "order 42" --since 5m --wait 20s
```

**What is this id?** (pasted from a log or a bug report)
```sh
mallard find <32-hex trace or event id | 16-hex issue id>   # each match has "next": the command to run
```

**Did my deploy add errors?**
```sh
mallard issues list --new --since 30m                 # first seen in the window
mallard issues list --first-release web@1.4.0 --status all
mallard releases list --since 7d                      # per release: events, issues, new_issues
```
Releases come from the `release` of error events; `sentry-cli releases new`
answers but stores nothing.

**Errors of one user, URL or tenant** (tags; repeat `--tag`, all must match):
```sh
mallard issues list --tag url=/checkout --env production --since 1d
mallard events list --since 1h --tag user=u-42 --fields event_id,title,timestamp
```

**Is the fix working?** `count` and `last_seen` cover the window:
```sh
mallard issues show <issue-id> --since 15m --fields id,status,count,last_seen
```

**Resolve with a reason** (safe to repeat: `changed` is false the second time):
```sh
mallard issues resolve <issue-id> --reason "fixed in #123" --json
# {"results":[{"id":"…","status":"resolved","changed":true}]}
```

**Set up a new app** (safe to repeat; never fails because the project exists):
```sh
mallard setup checkout --platform node >> .env
# SENTRY_DSN, OTEL_EXPORTER_OTLP_ENDPOINT, OTEL_EXPORTER_OTLP_PROTOCOL, OTEL_SERVICE_NAME
mallard setup checkout --sentry-cli >> .env           # adds SENTRY_URL, SENTRY_ORG, SENTRY_PROJECT,
                                                      # SENTRY_AUTH_TOKEN (the CLI's token) for sourcemaps
mallard send-test --dsn "$SENTRY_DSN"                 # then: mallard events list --since 5m --wait 30s
```

**Latency and errors per service / endpoint:**
```sh
mallard services --since 1h
mallard ops -S checkout --since 1h
mallard traces list -S checkout --errors --since 15m -n 10
```

**Did it get slower?** `ops --compare <dur>` adds the same window `<dur>` earlier:
`span_count_before`, `p95_ms_before`, `p95_change_pct`, `error_rate_before`,
`error_rate_change` (null when the operation had no spans then).
```sh
mallard ops -S checkout --since 1h --compare 1d --fields name,p95_ms,p95_change_pct,error_rate_change
```

**Over time** (one item per bucket, `time` = bucket start):
```sh
mallard series spans -S checkout -O "POST /pay" --since 6h --step 5m   # count, errors, p50/p95/p99_ms
mallard series logs -S checkout --since 1h                             # total, debug, info, warn, error
mallard series errors --since 1d --step 1h                             # Sentry events, errors, warnings, other
```
In `traces list`, `error_count` counts spans with error status and `sentry_errors`
counts Sentry error events of the trace (`null` when the Sentry side could not be
read; the reason goes to stderr). In `trace --json` every span has `sentry_errors`;
`errors[].span_id` says which span an error was captured in.

**Traces with Sentry errors:**
```sh
mallard traces list --since 1h --format ndjson --fields trace_id,root_name,sentry_errors
```

**Logs:**
```sh
mallard logs -S checkout -l warn --since 10m --format ndjson --fields time,severity_text,body,trace_id
mallard logs -t <trace-id>
mallard logs -S checkout -f --format ndjson            # follow until stopped
mallard logs -S checkout -f --for 2m --format ndjson   # follow for 2 minutes, then exit 0
mallard logs -S checkout -f --until-match "listening on" --for 1m   # exit 0 at the first match
                                                       # (body, case-insensitive); exit 8 if none in 1m
```

**Metrics:**
```sh
mallard metrics list -q http
mallard metrics show http.server.request.duration -g p95 -b http.route --since 6h --step 5m
```

**Health and limits:** `mallard status --json` has `capabilities`: `sql` on/off,
`max_window_secs`, `retention_secs`, `ingest_lag_secs` (how long new telemetry takes
to show up), `ingest_auth`.

**Ad-hoc SQL** (telemetry tables `spans`, `logs`, `metric_points`, …; with `--errors`
the error tables `events`, `issues`, `issue_activity`; often off):
```sh
mallard sql "SELECT service_name, count(*) FROM spans GROUP BY 1"   # exit 7 when off
mallard sql --errors "SELECT tags['url'] AS url, count(*) FROM events GROUP BY 1 ORDER BY 2 DESC"
```

## Good to know

- Data arrives a few seconds after it is sent (telemetry: `ingest_lag_secs`). Use
  `--wait 30s` on a lookup right after a test run.
- Default windows: issues 14d, events all (with `--trace` 30d), traces/logs/metrics 1h,
  `trace`/`find` 7d. The server caps windows (`max_window_secs`).
- Lists are capped by `--limit` (`-n`); keep it small and use `--fields`. An issue
  cursor keeps its window and sort; changing `--sort` with a cursor is exit 2.
- The old paths `mallard errors …` and `mallard telemetry …` still work.
