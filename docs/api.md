# Query API

Base URL: `http://<host>:8080`. The OpenAPI 3.1 document is served at
`/openapi.json` (also committed as [openapi.json](openapi.json)). An
interactive reference is at `/docs`.

All endpoints except `/api/v1/health`, `/openapi.json` and `/docs` need
`Authorization: Bearer <token>` when `auth.api_tokens` is set.

## Conventions

- **Time window** (every query endpoint): `since=15m` (relative to now or to
  `to`), or `from=` / `to=` in RFC 3339 (`2026-11-01T10:00:00Z`). The default
  is the last hour; for `GET /traces/{id}` it is the last 7 days. The window
  may not exceed `query.max_window` (31 days).
- **Buckets**: `step=30s`/`5m`. Default: about 60 buckets on a round
  boundary. At most 5000 buckets.
- **Attribute filters**: `attributes=key=value,key2=value2` matches span/log/
  point attributes **or** resource attributes. URL-encode `=` as `%3D` if
  your client doesn't.
- IDs are lowercase hex: trace ids 32 chars, span ids 16 chars.
- Durations: `*_ms` as float milliseconds; `duration_ns` as integer
  nanoseconds.
- Errors: non-2xx with `{"error": "<code>", "message": "..."}`. Codes:
  `bad_request` (400), `unauthorized` (401), `disabled` (403),
  `not_found` (404), `internal` (500), `busy` (503), `timeout` (504).

## Endpoints

| method & path | returns |
|---|---|
| `GET /api/v1/health` | `{"status":"ok","version":"…"}` |
| `GET /api/v1/stats` | ingest counters, buffer, flushes, storage per table, last maintenance |
| `GET /api/v1/services` | RED stats per service |
| `GET /api/v1/operations?service=` | RED stats per (service, span name, kind) |
| `GET /api/v1/spans/series?service=&operation=&step=` | count, errors, p50/p95/p99 per bucket |
| `GET /api/v1/traces?service=&operation=&errors_only=&min_duration_ms=&max_duration_ms=&attributes=&limit=` | trace summaries, newest first |
| `GET /api/v1/traces/{trace_id}` | all spans of a trace, by start time |
| `GET /api/v1/logs?service=&min_severity=&q=&trace_id=&attributes=&limit=&order=` | log records |
| `GET /api/v1/logs/series?service=&q=&step=` | log counts per bucket by severity band |
| `GET /api/v1/metrics?service=&q=` | metric catalog: name, type, unit, temporality, series/point counts |
| `GET /api/v1/metrics/query?name=&agg=&group_by=&attributes=&service=&step=` | time series |
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

### Logs (`/logs`)

- `q` is a case-insensitive substring match on the body (no wildcards).
- `min_severity`: 1-4 TRACE, 5-8 DEBUG, 9-12 INFO, 13-16 WARN, 17-20 ERROR,
  21-24 FATAL.
- Live tail: poll with `from=<last seen time minus a margin>&order=asc` and
  drop records already seen. Data becomes visible after the flush interval
  (5 s), so look back a bit. `odl_client::LogTail` does this.

### Metric queries (`/metrics/query`)

`agg` defaults by metric type: `rate` for monotonic sums, `avg` for gauges
and other sums, `p95` for histograms and summaries.

| agg | gauges & non-monotonic sums | monotonic sums (counters) | histograms | summaries |
|---|---|---|---|---|
| `avg` `min` `max` | over all points in the bucket | same | `avg` = sum/count of the increase | `avg` = sum/count increase |
| `sum` | sum over series of each series' average | same | – | – |
| `count` | number of points | number of points | number of observations (increase) | same |
| `increase` | – | per series: last value in bucket minus last value in previous bucket (a drop = counter reset); summed | observations | observations |
| `rate` | – | increase / step (per second) | observations / step | same |
| `p50`, `p95`, `p99.9`, … | – | – | percentile of the merged bucket increase within the time bucket (linear interpolation within a bucket) | average across series of the reported quantile closest to the requested one |

- Delta temporality is supported: the increase is the sum of the values.
- `group_by=route,service.name` splits series by attribute values
  (`service.name` maps to the service column). At most 5 keys and 500
  resulting series.
- Exponential histograms are converted to explicit buckets
  (`base = 2^(2^-scale)`); zero and negative buckets fold into one "≤ 0"
  bucket.

### SQL (`/sql`)

Off by default (`query.enable_sql`, requires API tokens). Accepts one
`SELECT`/`WITH` query over the lake tables (`spans`, `logs`, `metric_points`,
`metric_histograms`, `metric_exp_histograms`, `metric_summaries`, optionally
qualified as `lake.otel.`). Rows come back as JSON arrays; types without a
JSON equivalent (timestamps, UUIDs) as strings, maps as objects, NaN/Inf as
null. See [ADR 0005](adr/0005-sql-endpoint.md) for the safety checks.

## Examples

```sh
API=http://localhost:8080/api/v1
curl -s "$API/services?since=15m" | jq '.[] | {name, error_rate, p95_ms}'
curl -s "$API/traces?since=1h&service=checkout&errors_only=true&limit=5" | jq
curl -s "$API/traces/0af7651916cd43dd8448eb211c80319c" | jq '.spans | length'
curl -s "$API/logs?since=10m&min_severity=17&q=timeout" | jq -r '.[].body'
curl -s "$API/metrics/query?name=http.server.request.duration&agg=p99&group_by=http.route&since=6h&step=5m" | jq
curl -s -X POST "$API/sql" -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"query":"SELECT service_name, count(*) FROM spans GROUP BY 1"}' | jq
```

Typed clients: Rust [`odl-client`](../crates/odl-client). For other
languages, generate one from `openapi.json` (e.g. `openapi-generator`).
