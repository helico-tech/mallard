# CLI and TUI (`odl`)

`odl` talks to the query API. It does not need DuckDB.

```sh
export ODL_URL=http://localhost:8080     # default
export ODL_TOKEN=...                     # if auth.api_tokens is set
```

Global flags: `--url`, `--token`, `-o table|json`. Time windows:
`-s/--since 15m` (default `1h`) or `--from/--to` (RFC 3339).

## Commands

| command | what |
|---|---|
| `odl health` | is the server up (no token needed) |
| `odl status` | ingest counters, buffer, flushes, storage per table, last maintenance |
| `odl services [-s 1h] [--all-spans]` | rate, error %, p50/p95/p99 per service |
| `odl ops [-S svc]` | the same per operation |
| `odl traces [-S svc] [-O op] [-e] [--min-ms 500] [-a k=v] [-l 30]` | search traces |
| `odl trace <id> [-A]` | waterfall of one trace (`-A`: attributes and events) |
| `odl logs [-S svc] [-l warn] [-q text] [-t trace] [-a k=v] [-n 100] [-f]` | search logs; `-f` follows like `tail -f` |
| `odl metrics [-S svc] [-q name]` | metric catalog |
| `odl metric <name> [-g p95] [-b route,service.name] [--step 1m]` | a metric over time, with sparklines |
| `odl sql "<select>"` | read-only SQL (server must enable it) |
| `odl tui [-s 1h]` | interactive UI |

Examples:

```sh
odl services -s 15m
odl traces -S checkout -e -l 10
odl trace 0af7651916cd43dd8448eb211c80319c -A
odl logs -l error -f
odl metric http.server.request.duration -g p99 -b http.route -s 6h --step 5m
odl sql "SELECT name, count(*) FROM spans WHERE start_time > now()::TIMESTAMP - INTERVAL 1 HOUR GROUP BY 1 ORDER BY 2 DESC LIMIT 5"
odl traces -e -o json | jq -r '.[].trace_id'
```

## TUI

`odl tui` opens a full-screen UI with five views. It refreshes itself (every
5 s, the log tail every 2 s) and keeps your selection across refreshes.

| view | shows |
|---|---|
| **1 Overview** | KPI tiles with sparklines (requests/s, error rate, p95, logs/s, ingest buffer); throughput and latency charts; services table. Enter on a service → its operations → its traces. |
| **2 Traces** | filterable trace list with duration bars and error counts. Enter opens the waterfall with a span detail pane: attributes, events with exception stack traces, links, the span's logs, resource. |
| **3 Logs** | stacked severity histogram and a live tail; filter by service, level, trace, text; Enter shows the full record; jump to its trace. |
| **4 Metrics** | searchable metric list. Enter shows a chart with a selectable aggregation (avg, p50…p99, rate, …) and group-by. |
| **5 Status** | ingest counters, buffer gauge, flushes, per-table storage, last maintenance. |

Keys (press `?` in the TUI for the full list):

| key | action |
|---|---|
| `1`–`5`, Tab, Shift-Tab | switch view |
| `t` / `T` | next / previous time window (5m, 15m, 1h, 6h, 24h, 7d) |
| `r` | refresh now |
| `/` or `f` | edit filter (`service=api op="GET /" errors min=250ms key=value`) |
| ↑↓ `j` `k` PgUp PgDn `g` `G` | move |
| Enter / Esc | open / back |
| `E` | traces: errors only |
| `l` | trace/span: show its logs |
| `o` | logs: open the log's trace |
| `s`, space | logs: cycle severity, pause/follow |
| `a` / `b` | metric chart: cycle aggregation, set group-by |
| `q`, Ctrl-C | quit |

The TUI is also a library component (`odl_cli::tui::Embedded`) for embedding
in other terminal apps; see [embedding.md](embedding.md).
