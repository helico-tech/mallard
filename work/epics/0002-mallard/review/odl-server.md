# Review: `crates/odl-api`, `crates/odl-server`

Scope: all of `crates/odl-api/src/lib.rs`, `crates/odl-server/src/**`, `crates/odl-server/tests/*`.
Most important first. Paths are relative to the repo root.

## Bugs

1. **bug** — `crates/odl-server/src/query/mod.rs:194` (also `:196`, reached from `api/mod.rs:146` and `:305`).
   `to - TimeDelta::from_std(d)` uses chrono's `Sub`, which **panics** on overflow (`expect("DateTime - TimeDelta overflowed")`, checked in chrono 0.4.38–0.4.45). `since` is request input and humantime accepts years, so `GET /api/v1/traces?since=300000y` panics in the handler. The panic happens before the `max_window` check. There is no `CatchPanicLayer` on the odl side, so the connection is dropped and the panic is logged. The process survives only because `panic = "abort"` is not set.
   Fix: compare `d` against `max` *before* subtracting, and use `to.checked_sub_signed(delta).ok_or_else(|| bad("since is too large"))?`. Do the same on line 196. Add a unit test with `since=300000y`.

   **Status:** fixed in d4b1b02 (limit checked before the subtraction, `checked_sub_signed`; test `query::tests::huge_since_is_an_error_not_a_panic`).

2. **bug** — `crates/odl-server/src/lib.rs:133-135, 205-217`.
   If `run_background` fails early (`Flusher::start`, `Settings::from_config` or `Maintenance::start` returns `Err`), it returns at once. But `serve` only `join!`s it with the receivers and never cancels `shutdown`. The three listeners keep accepting OTLP data that is never flushed. The buffer fills up and every exporter then gets 429/UNAVAILABLE forever. The process never exits, and the error surfaces only after a signal.
   Fix: start the flusher and maintenance *before* spawning the listeners, or cancel `shutdown` when `background` resolves with `Err` (for example, wrap it as `async { let r = background.await; if r.is_err() { shutdown.cancel() }; r }`).

   **Status:** fixed in 19c4e46 (shutdown is cancelled when `run_background` fails; test `serve_stops_when_background_fails_to_start`). Note: `mallard-server`'s `Platform::serve` joins `run_background` the same way and was not changed here.

3. **bug** (triggered by config) — `crates/odl-server/src/store/maintenance.rs:202` and `:226`.
   `now - TimeDelta::from_std(retention).unwrap_or(TimeDelta::MAX)` panics for large retentions: `TimeDelta::MAX` always panics, and `"300000y"` overflows `DateTime`. The `epoch` closure has the same problem for `snapshot_retention` and `file_cleanup_delay`. Validation allows these values. The panic kills the `odl-maintenance` thread silently: `Maintenance::stop` ignores the `join` error, and `/stats` keeps showing the last good run. The `otel-ducklake maintenance` CLI crashes too.
   Fix: use `now.checked_sub_signed(..)` and treat `None` as "keep everything" (skip the delete). Also bound durations in `Config::validate` (for example ≤ 100 years).

   **Status:** fixed in 07d3afc (`checked_sub_signed`, `None` keeps everything; test `huge_retention_keeps_everything_instead_of_panicking`) and d4b1b02 (durations limited to 100 years in `validate`).

4. **bug** — `crates/odl-server/src/api/mod.rs:305` and `:146`; `crates/odl-server/src/config.rs:296-330`.
   The default windows (7 days for `GET /traces/{id}`, 1 hour elsewhere) are not clamped to `query.max_window`. With `max_window = "1d"`, every trace lookup without `from`/`since` returns 400 "time window larger than query.max_window". That includes `mallard-client`'s correlation call to `/otel/api/v1/traces/{id}`. Also, `max_window = "0"` passes validation: it is not in the "must be > 0" list, and with it every query fails.
   Fix: use `default.min(max)` in `Window::resolve`, and add `query.max_window` to the non-zero check.

   **Status:** fixed in d4b1b02 (`default.min(max)`, `query.max_window` must be > 0).

## High

5. **high** — `crates/odl-server/src/otlp/http.rs:31-40, 99-107`; `crates/odl-server/src/otlp/grpc.rs:56, 71, 88, 100-122`.
   Authentication runs *after* the request body is fully read. On HTTP, the `Bytes` extractor buffers up to `max_request_bytes`, 32 MB by default. On gRPC, tonic decompresses and decodes the whole protobuf message before the service method calls `check_auth`. There is also no bound on requests in flight: back-pressure only applies at `WriteBuffer::push`. N concurrent unauthenticated 32 MB requests therefore cost N×32 MB (more after prost decoding) before any check runs.
   Fix:
   - HTTP: do the token check in a `middleware::from_fn_with_state` layer, as `api::auth` already does. It runs before body extractors.
   - gRPC: wrap each service with `XxxServiceServer::with_interceptor(svc, check)`. Tonic interceptors see the metadata before the body is decoded.
   - Add a global in-flight bound, either `tower::limit::ConcurrencyLimitLayer` or a byte-weighted `Semaphore` sized from `max_buffer_bytes`.

   **Status:** fixed in bd9ffd2 (HTTP middleware and tonic interceptor check the token before the body is read/decoded; in-flight budget of `max_buffer_bytes`, answered with 429/UNAVAILABLE when used up; tests `http_ingest_checks_the_token_before_reading_the_body`, `grpc_roundtrip_and_auth_before_decoding`).

## Medium

6. **medium** — `crates/odl-server/src/query/metrics.rs:422-479`.
   For histograms and summaries, the result caps (`MAX_SERIES`, `MAX_POINTS`) are only enforced in `Collector::put`, after the whole result set has been merged into `merged: HashMap<(Key, i64), Merged>`. Each `Merged` holds a `BTreeMap` of buckets, and each row's JSON bucket arrays are parsed into fresh allocations. A high-cardinality `group_by` over a long window grows memory without limit before the 400 "more than 500 series" error comes back.
   Fix: check `merged.len()` (or the number of distinct keys) against the caps inside the row loop. Better: push `group_by` key cardinality into SQL (`count(DISTINCT k)` first, or `LIMIT`) so DuckDB fails fast.

   **Status:** fixed in f24476d (caps checked as new (series, bucket) entries appear; the SQL-side cardinality check was not needed).

7. **medium** — `crates/odl-server/src/store/writer.rs:244-268` and `:141-153`.
   A failed flush puts *all* buffered batches back and retries forever (during normal running there is no attempt limit). One batch that can never be written blocks every later batch. For example: a value DuckLake rejects, a column type changed by another version, or the table dropped. The buffer then fills and ingest returns 429 permanently. Whether such a "poison" batch can actually occur is **uncertain**; the design has no way to tell transient errors from permanent ones.
   Fix: after N failures, retry per table (or bisect the batches) to isolate the bad one. Move it to a dead-letter location (a Parquet file in `data_path/_rejected/`, or drop it and count it in a `dropped_rows` stat), so that other tables and later data keep flowing.

   **Status:** fixed in a0a2651 (after 3 failures each batch is written alone; one that fails while others succeed, and is not a conflict, is dropped, logged and counted in the new `ingest.dropped_rows`; no dead-letter file; test `poison_batch_is_dropped_and_the_rest_written`).

8. **medium** — `crates/odl-server/src/store/writer.rs:191-193`, `crates/odl-server/src/store/maintenance.rs:82-84, 145-162`.
   The flusher and maintenance threads are not supervised. A panic, from bug 3 or from arrow/duckdb code, ends the thread silently. For the flusher, this means data is acknowledged but never written, and nothing alerts except one log line.
   Fix: wrap each task in `std::panic::catch_unwind` (record it via `stats.flush_err` or `maintenance`). Or expose thread liveness on `/readyz` and cancel the server's shutdown token when the flusher dies.

   **Status:** fixed in a0a2651 (flusher: a panic while writing is a failed flush) and 9e9f514 (maintenance: a panicking task is recorded as failed).

9. **medium** — `crates/odl-server/src/config.rs:62, 82-88, 95-97, 107-117, 130, 134, 150, 296-330, 358-372`; `crates/odl-server/src/store/maintenance.rs:57-73`; `crates/odl-server/src/store/mod.rs:186, 345`.
   Durations and byte sizes are stored as `String` and parsed again later in four places: `validate`, the `expect("validated")` accessors, `Settings::from_config`, and `attach`/`configure_duckdb`. The field lists are written out twice in `validate`. The accessors panic if a caller changes the `pub` fields after validation; `mallard-server` builds this struct by hand. `log_format` is a free string that `main.rs:54` silently treats as "text" for any unknown value.
   Fix: type these fields at deserialize time, with `#[serde(with = "humantime_serde")] Duration` (already a workspace dependency, used by `sdl-server`) and a small `ByteSize(u64)` newtype with `Deserialize`/`Serialize` and `FromStr`. Also add `enum LogFormat { Text, Json }`. Keep the special case that `"0"` means zero in the `ByteSize`/duration deserializer. Then `validate` only checks ranges, and the accessors and `Settings::from_config` go away.

   **Status:** fixed in fff1c8a (`Duration` and `ByteSize` parsed at deserialize time, `"0"` = zero, `LogFormat` enum; accessors and `maintenance::Settings` removed; TOML forms unchanged).

10. **medium** — `crates/odl-server/src/lib.rs:20-26` (and everything below it).
    Every module is `pub`, with 169 `pub` items. Outside users only need `Server`, `config::*`, `store::{open_connection, CATALOG}` and `api::openapi`; `otlp::convert::convert_traces` is used only by `tests/e2e.rs`. `auth`, `stats`, `query`, `otlp::{grpc,http}`, `store::{writer,maintenance,schema}`, the `pub` fields on `Lake`, `Ingest`, `ApiState` and `Reader::max_window`, `estimate_expanded_bytes`, `sql_str`, `redact` and `json_col` are all internals that are now semver surface. Nothing can change them without breaking downstream crates.
    Fix: make the modules `pub(crate)`. Re-export the few public items at the crate root. For the e2e test, add one `#[doc(hidden)] pub` test hook, or move that check into a unit test.

    **Status:** fixed in f981ccd (modules private; public: `Server`, `config`, `open_connection`, `install_extensions`, `CATALOG`, `openapi()`, `run_maintenance()`, plus the `#[doc(hidden)]` test hook `__convert_traces`).

11. **medium** — `crates/odl-api/src/lib.rs:194-195, 299-305, 355-356, 431-436, 458, 91, 110`.
    Closed sets are sent as `String`: span `kind`, `status_code`, `metric_type`, `temporality`, `SignalIngestStats::signal`, `StorageStats::catalog_type`, `LogQuery::order`, and `MetricQuery::agg`. The server then validates `order` by hand (`api/mod.rs:314-320`). Clients cannot `match` on these values, and typos are not caught.
    Fix: use `#[derive(Serialize, Deserialize, ToSchema)] #[serde(rename_all = "SCREAMING_SNAKE_CASE" / "lowercase")] enum`s. The wire format stays the same, and utoipa emits `enum` schemas. Trace and span ids (`String` everywhere) would also benefit from `TraceId([u8;16])`/`SpanId(u64)` newtypes with hex serde: `parse_trace_id` (`query/mod.rs:260`) and `span_hex` then live in one place.

    **Status:** fixed in 3f10fae for span kind, status code, metric type, temporality, signal, catalog type and log order (wire strings unchanged). Not a problem for `MetricQuery::agg`: not a closed set (any `pNN`, e.g. `p99.9`). Deferred: trace/span id newtypes (optional suggestion; changes every id field of the API).

12. **medium** — tests in `crates/odl-server/src/otlp/convert.rs:834-864`, `crates/odl-server/tests/e2e.rs:136-200`; stale doc in `crates/odl-server/src/store/schema.rs:4-5`.
    The only check that convert output matches `TableDef` compares the *column count* for spans. Nothing compares names and Arrow↔SQL types for logs and the four metric tables. Exponential histograms, summaries and delta temporality are never ingested or queried end to end. `decode_body` (gzip/zstd/deflate, size cap) has no test. The gRPC receiver is never called in tests. `Window::resolve` edge cases (overflow, `from >= to`) are not covered. Also, `schema.rs` says "the appender maps columns by position" and points to `tests/lake.rs`, which does not exist. `writer.rs:166-170` now appends by column name.
    Fix: add one test that converts a request for each data kind and asserts, for every batch, that `schema().fields()` names equal `TableId::def().columns` names. Add an e2e test for exp-histogram, summary and delta-sum ingest+query, a `decode_body` unit test (oversized gzip bomb → error), and a tonic client round-trip. Update the `schema.rs` doc comment.

    **Status:** fixed in d8aba6e (column names and types for every table, e2e for exponential histograms, summaries and delta sums, `decode_body` tests incl. a gzip bomb), bd9ffd2 (tonic client round trip), d4b1b02 (`Window::resolve` overflow and `from >= to`) and f01f0d8 (`schema.rs` doc).

13. **medium** — `crates/odl-server/src/api/mod.rs:150-192`.
    The list of 12 routes is written twice: once in `router` and once in `openapi()`, in a different order. A route added to one list but not the other leaves the served API and the committed `openapi.json` out of sync (the `openapi.rs` test only checks `openapi()`).
    Fix: one `fn routes() -> OpenApiRouter<Arc<ApiState>>` that both call. `openapi()` then does `routes().split_for_parts().1`, and `router` adds the auth layer. To keep `health` outside the auth layer, build two route groups and merge them.

    **Status:** fixed in 50501d2 (one route list; test `every_documented_route_is_served`).

## Low

14. **low** — `crates/odl-server/src/store/mod.rs:106`, `crates/odl-server/src/store/writer.rs:247`, `crates/odl-server/src/query/sql.rs:201`.
    Errors are classified by matching message text, for example `format!("{e:#}").contains("conflict")`. In `writer.rs:247` the first test, `contains("Transaction conflict")`, is redundant with the second. This happens because the library APIs return `anyhow::Error`, which loses the `duckdb::Error` type (`Lake::open`, `open_connection`, `compact`, `retention`, `Server::new`, `Config::load`).
    Fix: return `thiserror` enums from the library entry points (`StoreError`, `ConfigError`), keep the `duckdb::Error` as `#[source]`, and put the "is this a conflict?" check in one helper. Keep `anyhow` in `main.rs`.

    **Status:** partly fixed in a0a2651 (one `is_conflict` helper, redundant check gone). Deferred: cross-side — typed `ConfigError`/`StoreError` for `Config::load` and the DuckDB setup belong with unifying the config loaders and DuckDB setup of both sides. DuckDB reports conflicts only in the message text, so the helper still matches text.

15. **low** — `crates/odl-server/src/query/metrics.rs:552-553`.
    `2f64.powi(-scale)` with `scale` taken from ingested OTLP data. `-i32::MIN` panics in debug builds (inside `spawn_blocking`, so the result is a 500) and wraps in release, which gives `base = 1` and wrong percentiles. OTLP only defines scale in [-10, 20].
    Fix: at ingest (`convert.rs:704`), reject points with an out-of-range `scale` and count them as `rejected`. Or at query time use `scale.clamp(-10, 20)`.

    **Status:** fixed in 6c7b95c (scale clamped to -10..=20 at query time; test `out_of_range_exp_histogram_scale_does_not_panic`).

16. **low** — `crates/odl-server/src/otlp/convert.rs:561-568, 578-591`.
    `hash_attrs` separates parts with the bytes `0` and `1`. Keys and values may contain those bytes, so two different attribute sets can produce the same `series_id` (only by deliberate crafting). That would merge counters and corrupt `rate`.
    Fix: hash the lengths (`h.update(&(k.len() as u64).to_le_bytes())`) before each key and value, and do the same for scope and name. This changes `series_id`, so it needs a note in docs/data-model.md.

    **Status:** fixed in 1630810 (length-prefixed hashing; note in docs/otel/data-model.md; test `series_id_has_no_separator_collisions`).

17. **low** (**uncertain** impact) — `crates/odl-server/src/query/sql.rs:32-61`.
    The memory guard is a denylist (`repeat`, `lpad`, `rpad`). It probably misses other functions that build large values, such as the scalar `range(0, 2000000000)` (only *table* functions are allowlisted), `list_resize` and `array_fill`. I did not test whether DuckDB allocates these outside its buffer manager.
    Fix: switch scalar functions to an allowlist too, or keep the module doc's statement "treat SQL access as admin access" and reflect it in the config doc for `query.enable_sql`.

    **Status:** fixed in 07cb814 (documented as admin access in the config docs; no allowlist).

18. **low** — `crates/odl-server/src/query/metrics.rs:32-37, 155-180, 256-266, 285-287, 349-384, 538-571`.
    Table dispatch matches on string literals (`"metric_points"`, `"metric_histograms"`, ...) that repeat `schema.rs`. `Agg::Quantile(_) => unreachable!()` (line 331) and `_ => unreachable!()` (line 476) exist only because of this. `query` is a 235-line function.
    Fix: add `enum MetricTable { Points, Histograms, ExpHistograms, Summaries }`, mapped to `TableId`, and dispatch to `query_points` and `query_histograms`.

    **Status:** fixed in d321bf6 (`MetricTable`, `HistKind`, `HistAgg`; `query_points`/`query_histograms`; no `unreachable!`).

19. **low** — dead code: `crates/odl-server/src/query/metrics.rs:510, 442` (`Merged::series` is only ever incremented), `crates/odl-server/src/otlp/mod.rs:101-102` (`IngestError::BadRequest` is never built), `crates/odl-server/src/stats.rs:22-24` (`pub` signal fields as well as the `signal()` accessor).
    Fix: remove them, and make the fields private.

    **Status:** fixed in f981ccd (`IngestError::BadRequest`, `Lake::table`), 6c7b95c (`Merged::series`) and 3f10fae (`Stats` fields private).

20. **low** — `crates/odl-server/src/store/maintenance.rs:53, 67-71, 210`.
    `retention: [(&'static str, Duration); 3]` is looked up by comparing strings with `t.signal.as_str()`.
    Fix: key it by `Signal`, either a `fn retention(&self, s: Signal) -> Duration` or three named fields.

    **Status:** fixed in fff1c8a (`RetentionConfig::of(Signal)`).

21. **low** — `crates/odl-server/src/store/schema.rs:193-195`.
    `TableId::def` is `TABLES[self as usize]`. The test checks that `ALL` matches the discriminants, but not that `TABLES[i]` is the matching table.
    Fix: use `match self { Spans => &SPANS, ... }`, and build `TABLES` from `TableId::ALL.map(TableId::def)`.

    **Status:** fixed in f01f0d8.

22. **low** — `crates/odl-server/src/otlp/convert.rs:225-235, 276-284, 318-336, 347-364`.
    `Option<AttrCol>`/`Option<ListOffsets>` exist only so that `#[derive(Default)]` compiles, which forces nine `.as_mut().unwrap()` calls.
    Fix: `impl Default for AttrCol` and `ListOffsets`, and drop the `Option`s.

    **Status:** fixed in 1630810.

23. **low** — needless copies:
    - `otlp/http.rs:72` copies the uncompressed body with `body.to_vec()`, up to 32 MB per request. Return `Bytes`/`Cow` instead; prost decodes from `Bytes`.
    - `convert.rs:691-692, 710, 712` clone each bucket vector into `Vec<Vec<_>>`. Append to `ListBuilder` directly.
    - `metrics.rs:542-551` calls `json_col(r.a.clone())` for every row. Take `Option<&str>`.
    - `sql.rs:191` uses `v.as_array().cloned()`. Use `if let J::Array(a) = v`.
    - `lib.rs:53-54`: `Arc<Config>`/`Arc<Lake>` are never cloned. Use plain fields.

    **Status:** fixed in 1630810 (HTTP body, bucket vectors, `Arc` fields) and 6c7b95c (`json_col` clones, `sql.rs` clone).

24. **low** — `crates/odl-server/src/otlp/http.rs:139`.
    A `JoinError` from the decode task (a panic message) goes to the client verbatim, and the request is not counted in stats. `Ingest::handle` (`otlp/mod.rs:132-135`) already hides these.
    Fix: log it and return a fixed "internal error" message, the same way.

    **Status:** fixed in 1630810.

25. **low** — `crates/odl-server/src/auth.rs:33-42`.
    `constant_time_eq` returns early on length mismatch, which leaks token length. A `fold` with `|` is also not guaranteed to stay constant-time after optimisation.
    Fix: compare SHA-256 digests (fixed length) with `subtle::ConstantTimeEq`, or at least use `subtle`. API nit: `Tokens::new(tokens: impl IntoIterator<Item = impl AsRef<str>>)`.

    **Status:** fixed in c936966 (SHA-256 digests compared with `subtle`; `Tokens::new(impl IntoIterator<Item = impl AsRef<str>>)`).

26. **low** — `crates/odl-server/src/main.rs:75-91`.
    The `maintenance` subcommand repeats the maintenance sequence (small compaction, full compaction, retention) that `store::maintenance` already owns. It also runs blocking DuckDB work directly on the `#[tokio::main]` runtime thread, and so does `install-extensions` (line 72).
    Fix: add `pub fn run_once(conn, schema, &Settings, full: bool) -> Result<Vec<String>>` in `maintenance.rs`. Call it via `spawn_blocking`, or make these subcommands synchronous and create the runtime only for `serve`/`check`.

    **Status:** fixed in f981ccd (`run_maintenance()`; both subcommands run on a blocking thread).

27. **low** — `crates/odl-server/src/config.rs:445-453`.
    An env override for a list field needs TOML array syntax. `ODL_AUTH__API_TOKENS=abc` becomes `String("abc")` and fails with "invalid type: string, expected a sequence". This is a common trap when a token comes from a Kubernetes secret.
    Fix: when the existing value is an array and the value does not parse as TOML, wrap it as `[v]` (or split on commas).

    **Status:** fixed in 07cb814 (comma split for list settings; test `env_list_override_without_toml_syntax`).

28. **low** — `crates/odl-api/src/lib.rs:548`, `:5`, derives.
    - `parse_attribute_filter` returns `Result<_, String>`. Use a small `#[derive(thiserror::Error)] AttributeFilterError`, or at least `impl Error`.
    - The crate doc says to regenerate `docs/openapi.json` via `otel-ducklake openapi`; the real path is `docs/otel/openapi.json` (see `tests/openapi.rs`).
    - Public wire types derive no `PartialEq`, which C-COMMON-TRAITS recommends; clients and tests cannot compare responses.
    - `HasWindow::window` clones `since` on every call. Return a borrowed view, or pass `&TimeWindow` built once.

    **Status:** fixed in 5138a8b (`AttributeFilterError`, doc path, `PartialEq` on wire types). Not a problem: `HasWindow::window` clones one small `Option<String>` per request; a borrowed view type adds API for no measurable gain.

29. **low** — `crates/odl-server/src/stats.rs:76-83`.
    `ingest()` writes out `"traces"`, `"logs"` and `"metrics"` by hand, repeating `Signal::as_str`.
    Fix: iterate `[Signal::Traces, Signal::Logs, Signal::Metrics]` and use `self.signal(s)`.

    **Status:** fixed in 3f10fae.

30. **low** — `crates/odl-server/src/query/mod.rs:255-257, 314-316`.
    `from_micros` maps bad values to 1970-01-01, and `json_col` maps any JSON parse error to `Default`. Both hide data problems.
    Fix: return `duckdb::Result` (`FromSqlError::Other`) so that corrupt rows show up as a 500 with a log entry, or at least `tracing::warn!` on the fallback.

    **Status:** fixed in 6c7b95c (both log a warning on the fallback).

31. **low** — `crates/odl-server/src/store/writer.rs:117-138, 198-213`; `crates/odl-server/src/store/maintenance.rs:88-103`.
    `take` returns an unnamed 4-tuple `(batches, rows, bytes, shutdown)`. `stop()` repeats the `Drop` logic in both types. `run` polls the stop flag every 500 ms with `sleep`.
    Fix: return a small `Taken` struct, and make `stop(self)` just `drop(self)`. Use a `Condvar`/`park_timeout` for maintenance so that stopping wakes it at once.

    **Status:** fixed in a0a2651 (`Taken` struct, `Flusher::stop` = drop) and 9e9f514 (`Maintenance::stop` = drop, `park_timeout` woken by `stop`).

32. **low** — module layout: `crates/odl-server/src/otlp/convert.rs` (878 lines) holds value helpers, Arrow builders and three signal converters. `crates/odl-server/src/query/metrics.rs` (725 lines) mixes listing, aggregation SQL and histogram maths.
    Fix: split into `convert/{mod,traces,logs,metrics}.rs` and `query/metrics/{mod,hist}.rs`. `Hist`/`Bound`/`quantile` are pure, already unit-tested, and easy to move.

    **Status:** deferred: a large file move with no behaviour change; not a small fix. Do it when these files next change.
