# CLI and TUI: telemetry

[docs/cli.md](../cli.md) has the commands, flags, output and shared keys, and
[docs/agents.md](../agents.md) has recipes. This page adds what is only true for
this side.

## Commands

The flat commands (`services`, `ops`, `traces`, `trace`, `logs`, `metrics`, `sql`, `schema`) cover
this side, with `mallard health` for "is the server up" without a token. `mallard status --json` has
the details of this side under `otel.info`: ingest counters, buffer, flushes, storage per table, last
maintenance. `mallard trace --json` has each span's attributes and events.

```sh
mallard trace 0af7651916cd43dd8448eb211c80319c --json | jq '.spans[].attributes'
mallard traces list -e --json | jq -r '.items[].trace_id'
mallard sql "SELECT name, count(*) FROM spans WHERE start_time > now()::TIMESTAMP - INTERVAL 1 HOUR GROUP BY 1 ORDER BY 2 DESC LIMIT 5"
```

## TUI

`mallard tui` opens both sides as tabs, with one app picker (`p`) and one time window
([cli.md](../cli.md)); F2 is this one, with five views. It refreshes
itself (every 5 s, the log tail every 2 s) and keeps your selection across refreshes.

| view | shows |
|---|---|
| **1 Overview** | KPI tiles with sparklines (requests/s, error rate, p95, logs/s, ingest buffer); throughput and latency charts; services table. Enter on a service → its operations → its traces. |
| **2 Traces** | filterable trace list with duration bars and error counts. Enter opens the waterfall with a span detail pane: attributes, events with exception stack traces, links, the span's logs, resource. |
| **3 Logs** | stacked severity histogram and a live tail; filter by service, level, trace, text; Enter shows the full record; jump to its trace. |
| **4 Metrics** | searchable metric list. Enter shows a chart with a selectable aggregation (avg, p50…p99, rate, …) and group-by. |
| **5 Status** | ingest counters, buffer gauge, flushes, per-table storage, last maintenance. |

Keys beyond the shared ones (press `?` in the TUI for the full list):

| key | action |
|---|---|
| `/` or `f` | edit filter (traces: `service=api op="GET /" errors min=250ms key=value`) |
| `t` / `T` | time window: 5m, 15m, 1h, 6h, 24h, 7d |
| `E` | traces: errors only |
| `l` | trace or span: show its logs |
| `o` | logs: open the log's trace |
| `s`, space | logs: cycle severity, pause or follow |
| `a` / `b` | metric chart: cycle aggregation, set group-by |

The TUI is also a library component (`mallard_otel_cli::Embedded`); see
[embedding.md](embedding.md).
