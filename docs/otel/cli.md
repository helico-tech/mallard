# CLI and TUI (the telemetry side)

> **Retired binary.** This page was written for the old standalone CLI, which
> is gone (story 0002/06). These commands are part of `mallard` now; see
> [docs/cli.md](../cli.md).

`mallard` talks to the query API. It does not need DuckDB.

```sh
export MALLARD_URL=http://localhost:8080     # default
export MALLARD_TOKEN=...                     # one of server.api_tokens
```

Global flags: `--url`, `--token`, `--json` (old: `-o json`), `--color
auto|always|never`. Time windows: `-s/--since 15m` (default `1h`) or
`--from/--to` (RFC 3339). Flags, output style, time zone (UTC) and TUI keys
are the same as on the errors side: [../cli.md](../cli.md).

## Commands

| command | what |
|---|---|
| `mallard telemetry health` | is the server up (no token needed) |
| `mallard telemetry status` | ingest counters, buffer, flushes, storage per table, last maintenance |
| `mallard services [-s 1h] [--all-spans]` | rate, error %, p50/p95/p99 per service |
| `mallard ops [-S svc]` | the same per operation |
| `mallard traces list [-S svc] [-O op] [-e] [--min-ms 500] [-a k=v] [-n 30] [--cursor c]` | search traces; `--cursor`: the next page |
| `mallard telemetry trace <id> [-A]` | waterfall of one trace (`-A`: attributes and events) |
| `mallard logs [-S svc] [-l warn] [-q text] [-t trace] [-a k=v] [-n 100] [-f] [--cursor c]` | search logs; `-f` follows like `tail -f`; `--cursor`: the next page |
| `mallard metrics list [-S svc] [-q name]` | metric catalog |
| `mallard metrics show <name> [-g p95] [-b route,service.name] [--step 1m] [--exemplars]` | a metric over time, with sparklines; `--exemplars` lists trace ids of the highest samples |
| `mallard sql "<select>"` | read-only SQL (server must enable it) |
| `mallard telemetry schema` | the tables and columns SQL can query |
| `mallard schema attrs [--signal spans\|logs\|metrics] [-S svc] [-k key] [--metric m] [-n 50]` | attribute keys with counts, or the top values of one key (sampled) |
| `mallard tui` (`mallard telemetry tui [-s 1h]`: this side only) | interactive UI |

Examples:

```sh
mallard services -s 15m
mallard traces list -S checkout -e -n 10
mallard telemetry trace 0af7651916cd43dd8448eb211c80319c -A
mallard logs -l error -f
mallard metrics show http.server.request.duration -g p99 -b http.route -s 6h --step 5m
mallard sql "SELECT name, count(*) FROM spans WHERE start_time > now()::TIMESTAMP - INTERVAL 1 HOUR GROUP BY 1 ORDER BY 2 DESC LIMIT 5"
mallard traces list -e --json | jq -r '.[].trace_id'
```

## TUI

`mallard tui` opens a full-screen UI with five views. It refreshes itself (every
5 s, the log tail every 2 s) and keeps your selection across refreshes.

| view | shows |
|---|---|
| **1 Overview** | KPI tiles with sparklines (requests/s, error rate, p95, logs/s, ingest buffer); throughput and latency charts; services table. Enter on a service → its operations → its traces. |
| **2 Traces** | filterable trace list with duration bars and error counts. Enter opens the waterfall with a span detail pane: attributes, events with exception stack traces, links, the span's logs, resource. |
| **3 Logs** | stacked severity histogram and a live tail; filter by service, level, trace, text; Enter shows the full record; jump to its trace. |
| **4 Metrics** | searchable metric list. Enter shows a chart with a selectable aggregation (avg, p50…p99, rate, …) and group-by. |
| **5 Status** | ingest counters, buffer gauge, flushes, per-table storage, last maintenance. |

Keys (press `?` in the TUI for the full list). The keys every tab shares
are in [../cli.md](../cli.md): move, Enter open, Esc/`q` back, `/` filter,
`t`/`T` time window (5m, 15m, 1h, 6h, 24h, 7d), `r` refresh, `x` errors of
this trace (in `mallard tui`), `?` help, Ctrl-C quit. All times are UTC.

| key | action |
|---|---|
| `1`–`5`, Tab, Shift-Tab | switch view |
| `/` or `f` | edit filter (`service=api op="GET /" errors min=250ms key=value`) |
| `E` | traces: errors only |
| `l` | trace/span: show its logs |
| `o` | logs: open the log's trace |
| `s`, space | logs: cycle severity, pause/follow |
| `a` / `b` | metric chart: cycle aggregation, set group-by |

The TUI is also a library component (`mallard_otel_cli::Embedded`) for embedding
in other terminal apps; see [embedding.md](embedding.md).
