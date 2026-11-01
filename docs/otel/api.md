# Query API

Base URL: `http://<host>:8080/otel`; the paths below are relative to it. The OpenAPI
3.1 document is served at `/otel/openapi.json` (also committed as
[openapi.json](openapi.json)). An interactive reference is at `/otel/docs` (its script
loads from a CDN, so the browser needs internet access; `/otel/openapi.json` works
offline).

All endpoints except `/api/v1/health`, `/openapi.json` and `/docs` need
`Authorization: Bearer <token>`, with a token from `server.api_tokens`.

The Sentry side has its own API under `/sentry/v1`
([docs/sentry/openapi.json](../sentry/openapi.json)). OTLP ingest is not part of this
API: it is `POST /v1/traces`, `/v1/logs` and `/v1/metrics` on the server root.

## Conventions

- **Time window** (every query endpoint): `since=15m` (relative to now or to
  `to`), or `from=` / `to=` in RFC 3339 (`2026-11-01T10:00:00Z`). The default
  is the last hour; for `GET /traces/{id}` it is the last 7 days. The window
  may not exceed `otel.query.max_window` (31 days).
- **Buckets**: `step=30s`/`5m`. Default: about 60 buckets on a round
  boundary. At most 5000 buckets; a step larger than the window is reduced
  to the window.
- **Attribute filters**: `attributes=key=value,key2=value2` matches span/log/
  point attributes **or** resource attributes. URL-encode `=` as `%3D` if
  your client doesn't.
- IDs are lowercase hex: trace ids 32 chars, span ids 16 chars.
- Durations: `*_ms` as float milliseconds; `duration_ns` as integer
  nanoseconds.
- Errors: non-2xx with `{"error": "<code>", "message": "..."}` (also for
  malformed parameters and bodies). Codes: `bad_request` (400),
  `unauthorized` (401), `disabled` (403), `not_found` (404), `internal`
  (500, details only in the server log), `busy` (503), `timeout` (504).
- Requests that are cancelled (client disconnect) or time out interrupt the
  running query.

## Endpoints

| method & path | returns |
|---|---|
| `GET /api/v1/health` | `{"status":"ok","version":"…"}` |
| `GET /api/v1/stats` | ingest counters, buffer, flushes, storage per table, last maintenance |
| `GET /api/v1/services` | RED stats per service |
| `GET /api/v1/operations?service=` | RED stats per (service, span name, kind) |
| `GET /api/v1/spans/series?service=&operation=&step=` | count, errors, p50/p95/p99 per bucket |
| `GET /api/v1/traces?service=&operation=&errors_only=&min_duration_ms=&max_duration_ms=&attributes=&limit=&cursor=` | trace summaries, newest first (paged) |
| `GET /api/v1/traces/{trace_id}` | all spans of a trace, by start time |
| `GET /api/v1/spans/{span_id}` | one span (default window: 7 days) |
| `GET /api/v1/logs?service=&min_severity=&q=&trace_id=&attributes=&limit=&order=&cursor=` | log records (paged) |
| `GET /api/v1/logs/series?service=&q=&step=` | log counts per bucket by severity band |
| `GET /api/v1/metrics?service=&q=` | metric catalog: name, type, unit, temporality (`delta`, `cumulative`, `mixed` when it has both, empty for gauges and summaries), series/point counts |
| `GET /api/v1/metrics/query?name=&agg=&group_by=&attributes=&service=&step=&exemplars=` | time series (and exemplars) |
| `GET /api/v1/attributes?signal=&service=&metric=&key=&limit=` | attribute keys with counts, or the top values of `key` (sampled) |
| `GET /api/v1/schema` | the tables and columns SQL can query |
| `POST /api/v1/sql` | `{"query": "...", "limit": 1000}` → columns + rows (off by default) |

### RED stats (`/services`, `/operations`, `/spans/series`)

RED = **R**ate, **E**rrors, **D**uration. By default only *entry spans* are
counted: spans of kind `SERVER` or `CONSUMER`, or root spans. Those are the
requests a service handles. A service that has no entry spans in the window
falls back to all its spans, so it still shows up. Set `entry_only=false` to
count every span. Percentiles are approximate (t-digest,
`approx_quantile`); errors are spans with status `ERROR`.

### Trace search (`/traces`)

All filters apply to the *same* span: `service=checkout&errors_only=true`
finds traces in which a `checkout` span failed. The summary is computed from
the trace's spans inside the window: start, duration (earliest start to
latest end), root service and name (when the root span was received), span
and error counts, and the services involved.

### Paging (`/traces`, `/logs`)

When more items match than `limit`, the response has a header
`x-next-cursor`. Send it back as `cursor=` with the same filters, window
and order to get the next page; the last page has no header. The body stays
a plain array. The cursor is opaque (today: the time of the last item and an
id that breaks ties: the trace id, or DuckLake's row id for logs). Use an
absolute window (`from`/`to`) for all pages: with `since` the window moves
between requests.

### Span by id (`/spans/{span_id}`)

The span with this id (16 hex chars), with its trace id, in the window
(default: the last 7 days). Span ids are unique in practice; if two spans
share an id, the newest is returned. 404 when there is none.

### Logs (`/logs`)

- `q` is a case-insensitive substring match on the body (no wildcards).
- `min_severity`: 1-4 TRACE, 5-8 DEBUG, 9-12 INFO, 13-16 WARN, 17-20 ERROR,
  21-24 FATAL.
- Live tail: poll with `from=<last seen time minus a margin>&order=asc` and
  drop records already seen. Data becomes visible after the flush interval
  (5 s), so look back a bit. `mallard_otel_client::LogTail` does this.

### Metric queries (`/metrics/query`)

`agg` defaults by metric type: `rate` for monotonic sums, `avg` for gauges
and other sums, `p95` for histograms and summaries.

| agg | gauges & non-monotonic sums | monotonic sums (counters) | histograms | summaries |
|---|---|---|---|---|
| `avg` `min` `max` | over all points in the bucket | same | `avg` = sum/count of the increase | `avg` = sum/count increase |
| `sum` | sum over series of each series' average | same | – | – |
| `count` | number of points | number of points | number of observations (increase) | same |
| `increase` | – (400) | per series: last value in bucket minus last value in previous bucket (a drop = counter reset); summed | observations | observations |
| `rate` | – | increase / step (per second) | observations / step | same |
| `p50`, `p95`, `p99.9`, … | – | – | percentile of the merged bucket increase within the time bucket (linear interpolation within a bucket) | average across series of the reported quantile closest to the requested one |

- Delta temporality is supported: the increase is the sum of the values.
  Temporality is taken per series, so one metric can mix delta and
  cumulative series.
- `group_by=route,service.name` splits series by attribute values
  (`service.name` maps to the service column). At most 5 keys, 500 series and
  200,000 points per response.
- Exponential histograms are converted to explicit buckets
  (`base = 2^(2^-scale)`); zero and negative buckets fold into one "≤ 0"
  bucket.
- `exemplars=true` also returns `exemplars`: for each series (`group_by`
  labels) and bucket, the stored exemplar with the highest value that has a
  trace id, with its `trace_id`, `span_id`, value, time and filtered
  attributes. The same filters apply. At most 1000, oldest first. Open the
  trace with `/traces/{trace_id}`: that is "from a latency spike to the
  request that caused it". Gauges, sums and histograms carry exemplars;
  summaries have none in OTLP.

### Attributes (`/attributes`)

What can I filter on? `signal=spans|logs|metrics` (required). Without
`key`: the attribute keys of the matching rows, each with the number of
rows that have it, most frequent first. With `key`: the most frequent
values of that key. Span/log/point attributes and resource attributes are
counted together, as the `attributes=` filters match either. `service`
narrows to one service, `metric` (signal `metrics` only) to one metric.
The counts come from a random sample of at most 10,000 matching rows:
`total_rows` says how many rows matched, `sampled_rows` how many were
counted. The window is the usual one (default 1 hour).

### Schema (`/schema`)

The tables the SQL endpoint can query, each with its time column (filter
on it: tables are partitioned by day on it) and its columns as stored in
the catalog. It needs no SQL access, so it works when SQL is off.

### SQL (`/sql`)

Off by default (`otel.query.enable_sql`). Accepts one
`SELECT`/`WITH` query over the lake tables (`spans`, `logs`, `metric_points`,
`metric_histograms`, `metric_exp_histograms`, `metric_summaries`, optionally
qualified as `lake.otel.`). Rows come back as JSON arrays; types without a
JSON equivalent (timestamps, UUIDs) as strings, maps as objects, NaN/Inf as
null. Errors in the query are 400 with DuckDB's message. Table and CTE names
must be plain identifiers; `SHOW`/`DESCRIBE`/`SUMMARIZE` and a few functions
(`repeat`, `lpad`, `rpad`, file/settings/env access) are refused. See
[ADR 0005](adr/0005-sql-endpoint.md). An answer holds at most `limit` rows
(default 1000, max 100000) and 32 MiB of row data; `truncated` is true when
either cut it short ([ADR 0015](../adr/0015-sql-resource-limits.md)).

## Examples

```sh
API=http://localhost:8080/otel/api/v1
AUTH="Authorization: Bearer $MALLARD_TOKEN"
curl -s -H "$AUTH" "$API/services?since=15m" | jq '.[] | {name, error_rate, p95_ms}'
curl -s -H "$AUTH" "$API/traces?since=1h&service=checkout&errors_only=true&limit=5" | jq
curl -s -H "$AUTH" "$API/traces/0af7651916cd43dd8448eb211c80319c" | jq '.spans | length'
curl -s -H "$AUTH" "$API/logs?since=10m&min_severity=17&q=timeout" | jq -r '.[].body'
curl -s -H "$AUTH" "$API/spans/00f067aa0ba902b7" | jq -r .trace_id
curl -s -H "$AUTH" "$API/attributes?signal=spans&service=checkout&key=http.route" | jq -r '.items[] | "\(.value) \(.count)"'
curl -s -H "$AUTH" "$API/metrics/query?name=http.server.request.duration&agg=p99&group_by=http.route&since=6h&step=5m" | jq
curl -s -H "$AUTH" "$API/metrics/query?name=http.server.request.duration&agg=p99&since=1h&exemplars=true" | jq '.exemplars[] | {time, value, trace_id}'
curl -s -X POST "$API/sql" -H "$AUTH" -H 'content-type: application/json' \
  -d '{"query":"SELECT service_name, count(*) FROM spans GROUP BY 1"}' | jq
```

Typed clients: Rust [`mallard-otel-client`](../../crates/mallard-otel-client). For other
languages, generate one from `openapi.json` (e.g. `openapi-generator`).
