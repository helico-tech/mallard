# Review: sdl-protocol, sdl-types, sdl-server

Scope: `crates/sdl-protocol`, `crates/sdl-types`, `crates/sdl-server` (all files read).
Ordered by importance. Paths are relative to the repo root.

## Bugs

1. **bug** — `crates/sdl-server/src/db/mod.rs:287-291` — A cancelled read can poison the reader pool for good.
   `_permit` is a local of the async fn `Db::read`, not moved into the `spawn_blocking` closure. If the caller's future is dropped (client disconnects, a timeout layer, or a cancelled moka `get_with` init in `sourcemaps::locate`), the permit goes back while the blocking task still holds its connection. A later reader then gets a permit but finds the `Vec` empty, and `readers.lock().expect("lock").pop().expect("permit guarantees a connection")` panics **while the `MutexGuard` temporary is still alive**. That poisons the `Mutex`, and from then on every `readers.lock().expect("lock")` panics: every API read and `/readyz` fails until restart.
   Fix: move the permit into the closure (`let permit = ...; spawn_blocking(move || { let _permit = permit; ... })`), and do not `expect` while holding the guard. Simpler and more idiomatic: replace `Mutex<Vec<Connection>> + Semaphore` with one pool primitive, for example a `crossbeam_channel::bounded(n)` of connections. `recv()` inside `spawn_blocking`, and send the connection back from a guard's `Drop`. Then permits and connections cannot get out of step.
   **Status:** fixed in ecf5dca (permit moved into the blocking task, no `expect` under the lock; test `db::tests::cancelled_read_keeps_the_pool_usable`).

2. **bug** — `crates/sdl-server/src/db/mod.rs:294-303` — A panic in the query closure leaves the pooled connection inside an open transaction.
   `ROLLBACK` runs inside the `catch_unwind` closure after `f`. When `f` panics, `ROLLBACK` is skipped and the connection goes back to the pool mid-transaction. Every later `BEGIN TRANSACTION` on it fails, so 1 in `readers` API calls fails for good. Finding 7 gives a way to cause such a panic from user input (debug builds).
   Fix: run `let _ = conn.execute_batch("ROLLBACK")` after `catch_unwind`, whatever the outcome. Or, on a panic, drop that connection and replace it with a fresh `try_clone()` of a template connection kept in `Db`.
   **Status:** fixed in ecf5dca (ROLLBACK after `catch_unwind`; test `db::tests::panicking_read_returns_a_clean_connection`).

## High

3. **high** — `crates/sdl-server/src/ingest.rs:184` and `:202` — Metric labels come from client input, so their number has no limit.
   `"type" => ty.to_string()` uses the item `type` header from the envelope. Any client with a project's public key can send random item types. Browser DSNs are public, so that is anyone. Each new value creates a new Prometheus series, which stays in the recorder's memory and in `/metrics` output.
   Fix: map `ty` to a closed set (`"event" | "error" | "transaction" | "session" | ... | _ => "other"`) and use `&'static str` labels.
   **Status:** fixed in 37068b5.

4. **high** — `crates/sdl-protocol/src/symbolicate.rs:162-171` (called per frame from `:209`) — Source context costs O(frames × source size).
   `context()` runs `contents.lines().collect::<Vec<_>>()` over the **whole** original source for every rewritten frame. One event can carry about 16k frames (64 holders × `MAX_FRAMES`), all with the same `abs_path`, against a map with a 1 MB `sourcesContent`. That is gigabytes of scanning in one `spawn_blocking` task per event: CPU DoS by anyone with the public key, once a project has bundles.
   Fix: use `SourceMap::get_source_view(src_id)` (a `SourceView` keeps a line index; `get_line(n)` is O(1) after the first call). Or build the line index once per `(map, src_id)` inside `apply` and reuse it.
   **Status:** fixed in c8961fe (uses the map's `SourceView` line index).

5. **high** — `crates/sdl-protocol/src/symbolicate.rs:111-118` with `crates/sdl-server/src/sourcemaps.rs:393` — `collect_refs` is O(n²), runs on an async worker thread, and is capped only after the fact.
   It removes duplicates with `out.iter().any(|r| r.abs_path == abs)` over up to about 16k frames, so up to ~130M string compares if the paths are unique. `resolve()` only applies `.take(MAX_REFS_PER_EVENT)` afterwards. `resolve` is called directly in the async handler (`ingest.rs:262`), not in `spawn_blocking`, so this blocks a Tokio worker.
   Fix: deduplicate with a `HashSet<&str>`, take a `limit` parameter (or return an iterator) so collection stops at `MAX_REFS_PER_EVENT`, and call it in the existing blocking task.
   **Status:** fixed in bec20f5.

6. **high** — `crates/sdl-server/src/api.rs:218`, `:272`, `:427`; `crates/sdl-server/src/ingest.rs:116` — HTTP status codes are picked by matching error text.
   Examples: `e.to_string().contains("already exists")` gives 409, `contains("not found")` gives 404, `contains("not configured")` gives 501, and `msg.contains("length limit")` gives 413. This breaks silently when a message is reworded. It can also turn an unrelated DuckDB error that happens to contain "not found" or "already exists" into a 404/409 that leaks the internal message.
   Fix: add typed errors with `thiserror` (for example `enum WriteError { SlugTaken(String), IssueNotFound, Db(#[from] anyhow::Error) }` and `enum SourcemapError { NotConfigured, BadRequest(String), Internal(anyhow::Error) }`) and match on the variants. For the body limit, walk `std::error::Error::source()` and `downcast_ref::<http_body_util::LengthLimitError>()`.
   **Status:** fixed in 4c8bfbe (`WriteError`, `SourcemapError`, `LengthLimitError` in the source chain).

## Medium

7. **medium** — `crates/sdl-server/src/db/read.rs:535-540` — Integer overflow and a truncating cast on the user-supplied `interval`.
   `d.as_micros() as i64` truncates a `u128`. `w.span_us() + interval_us - 1` overflows when `interval` is close to `i64::MAX` µs (for example `?since=1000y&interval=291500y`). Debug builds panic, which triggers bug 2. Release builds (no `overflow-checks`) silently give a single bucket.
   Fix: `i64::try_from(d.as_micros())` with checked arithmetic (`checked_add`, `div_ceil`). Reject impossible values with 400 in `api::project_stats`.
   **Status:** fixed in 451c63c (test `read::tests::stats_buckets_huge_interval_does_not_overflow`).

8. **medium** — `crates/sdl-server/src/ingest.rs:263-283`, `crates/sdl-server/src/db/writer.rs:26-31` — Each queued event holds its payload twice.
   `EventWrite` carries `event: ProcessedEvent`, which still owns `payload: Value`, plus `payload_json: String`. The writer only uses `payload_json`. A `serde_json::Value` tree is several times larger than its text. The queue is bounded by count (`queue_capacity = 20000`), not bytes. Queued commands outlive cancelled requests, so the concurrency limit does not bound them.
   Fix: `let payload = std::mem::take(&mut processed.payload);`, serialize it, and drop the `Value` before sending. Better: do not keep `payload` in `ProcessedEvent`; return it separately. Consider a byte budget for the queue.
   **Status:** fixed in 55b1b90 (`ProcessedEvent` no longer holds the payload; no byte budget for the queue added).

9. **medium** — `crates/sdl-server/src/ingest.rs:262`, `:276-280`, `:295` — Duplicate events are detected only after all the expensive work, and the check races.
   `recent_events.contains_key` runs after normalization, the sourcemap fetch and symbolication. The insert happens only after the commit, so two concurrent SDK retries both pass the check and are both stored. If the client disconnects after enqueueing, the event is stored but never marked as seen.
   Fix: compute the event id early (payload `event_id`, else the envelope hint), and check and claim the key before the heavy work. Use `moka`'s `entry().or_insert_with` or `get_with` for an atomic claim. Release the claim on failure.
   **Status:** fixed in 55b1b90 (atomic claim with `or_insert_with`; test `concurrent_retries_are_stored_once`).

10. **medium** — `crates/sdl-server/src/sourcemaps.rs:319-333`, `:388` — Lookups by debug id give clients cheap DB reads, and a failed lookup is cached as "not found".
    For a project with any bundle, every ref with a random `debug_meta` debug id does a `db.read` (cache keys are unique, so nothing is cached). That is up to 50 reads per event against a pool of 4 readers, shared with the query API, from a public key. Separately, `get_with(..., unwrap_or_else(|_| None))` caches a transient DB error as "not found" for 60 s. That goes against the stated rule in `load_map` ("a transient storage error must not disable symbolication").
    Fix: keep the known `(project, debug_id)` set in `Caches`, like `bundle_releases`, and skip unknown ids without a query. Use `optionally_get_with` or `try_get_with` so errors are not cached.
    **Status:** fixed in cf52cf1.

11. **medium** — `crates/sdl-server/src/sourcemaps.rs:168-170` — The parsed-map cache weight leaves out most of the memory.
    The weigher counts only `token_count * 24 + 1024`. It leaves out `sourcesContent` (kept inside `SourceMap`) and the minified `SourceView` (up to `MAX_ENTRY_SIZE` = 64 MiB per entry). So `sourcemap_cache_mb` does not really bound memory.
    Fix: record the byte size when parsing (`map_bytes.len() + minified.len()`) in `ResolvedSourceMap` or a wrapper, and weigh by that.
    **Status:** fixed in 697c359.

12. **medium** — `crates/sdl-server/src/sourcemaps.rs:263-283` — Assembling a bundle keeps two full copies, and does CPU work on the async thread.
    Up to 512 MiB goes into `data`, then `Bytes::from(bundle_zip(&data).to_vec())` makes a second copy. SHA-1 over 512 MiB and `zip::ZipArchive::new` (in `bundle_zip`) run on a Tokio worker. Several uploads with different checksums are not limited.
    Fix: `let data = Bytes::from(data);` then `data.slice(offset..)` (no copy). Move hashing and `bundle_zip` into `spawn_blocking`. Add a semaphore for concurrent assemblies.
    **Status:** fixed in 697c359.

13. **medium** — `crates/sdl-server/src/db/mod.rs:40`, `crates/sdl-server/src/db/writer.rs:610-623` — The issue cache and the `issues` table only grow.
    Retention deletes `events` only. `Caches::issues` loads every issue ever seen at startup and keeps them in memory forever. With high-cardinality grouping (messages not masked well enough) this is unbounded memory and startup time.
    Fix: during retention, drop cache entries (and add `issue_activity` rows or a tombstone) for issues with no events left in the window, or load the cache lazily per project. This needs an ADR, because the append-only rule applies.
    **Status:** deferred: needs a design decision (ADR on the append-only rule; re-inserting an expired issue would duplicate `issues` rows and break the list join). Tracked in [sentry issue 0002](../../../sentry/issues/0002-issue-cardinality-limits.md).

14. **medium** — `crates/sdl-server/src/db/mod.rs:324-330`, `:192-193` — `redact` is weak, and the DuckDB error is not redacted at all.
    It only masks whitespace-separated `password=...`. It misses quoted values (`password='a b'`), URI forms and `passwd=`. Also, `.with_context(redact(catalog))` redacts only the context: the wrapped DuckDB error can echo the full connection string. `odl-server/src/store/mod.rs:27-28` notes that the postgres extension does this. So startup errors can log the password. (Echoing by DuckDB is taken from the odl comment; not reproduced here.)
    Fix: reuse odl's more thorough `redact` (share it, see 29) and apply it to the formatted error chain (`format!("{e:#}")`), not only the context.
    **Status:** partly fixed in 99fd963 (the whole attach error is redacted); the stronger shared `redact`: deferred: cross-side.

15. **medium** — `crates/sdl-server/src/ingest.rs:94-100` — The client IP from `X-Forwarded-For` is the spoofable leftmost entry, and it is not checked.
    With a proxy that appends to the header (most do), the first entry is whatever the client sent. It is stored as `user.ip_address` and used for `user_key` and the `user` tag, so users can be faked. It is also never parsed as an IP.
    Fix: parse with `IpAddr::from_str`. Take the rightmost entry (or the Nth from the right through a `trusted_proxy_hops` setting), not the first. Document this. (This is a design choice; it depends on the deployment.)
    **Status:** fixed in 49a14f0 (last entry, parsed as `IpAddr`; documented as one proxy hop, no `trusted_proxy_hops` setting).

16. **medium** — `crates/sdl-server/src/lib.rs:24-48`, `:102` — The public API is much larger than what is used.
    `pub mod api, db, ingest, sourcemaps, storage`, `pub struct AppState` with all fields `pub`, and `Server::state()` expose internals (writer commands, caches, the DuckDB pool). Outside users (`mallard-server`, integration tests) use only `Server`, `Config`, the routers and `openapi()`. Every internal refactor is then a semver break.
    Fix: make the modules and `AppState` `pub(crate)`, drop `Server::state()`, and re-export only the facade types. `api::openapi()` is already reachable through `Server::openapi()`.
    **Status:** fixed in fb5adc6.

17. **medium** — `crates/sdl-server/src/api.rs:434-438`, `crates/sdl-server/src/sourcemaps.rs:200-226` — Every assemble error, including storage I/O failures, becomes 400 with the raw error text.
    `storage.exists(...)?` and `db.caches` errors from `assemble` go to `sentry_err(BAD_REQUEST, e.to_string())`. Server faults are reported as client faults, and internal details are leaked.
    Fix: use the typed error from 6, with validation as 400 and internal errors as 500 plus a log line.
    **Status:** fixed in 4c8bfbe.

18. **medium** — `crates/sdl-types/src/lib.rs:235-255`, `crates/sdl-server/src/db/read.rs:76-82` — Query enums are typed as strings.
    `IssueQuery.status` and `IssueQuery.sort` are `Option<String>`. Status is checked by hand in `api.rs:234-239`. An unknown `sort` silently falls back to `last_seen`.
    Fix: add `#[derive(Deserialize, Serialize, ToSchema)] #[serde(rename_all = "snake_case")] enum IssueSort { LastSeen, FirstSeen, Count, Users }` and `enum StatusFilter { All, #[serde(untagged)] Status(IssueStatus) }` (or a flat enum). serde rejects bad values with 400, and the OpenAPI schema lists the allowed values.
    **Status:** partly fixed in fd9d95e (server parses into `StatusFilter`/`IssueSort`, unknown sort is 400); typed fields in `sdl-types`: deferred: changes sdl-client and sdl-cli, outside this step.

19. **medium** — tests (`crates/sdl-server/src/db/*.rs`, `ingest.rs`, `sourcemaps.rs`) — Important logic has no unit tests.
    Missing: `Db::read` behaviour under a panic or cancellation (bugs 1–2), `project_stats` bucket maths and limits, `parse_cursor` and pagination edge cases (ties on `received_at`), `redact`, `key_from_headers` precedence, `trust_forwarded_for`, `map_url_for`, the relative `sourcemap` resolution in `find_by_url`, and `Window::parse` limits. Integration tests cover only the happy paths.
    Fix: add focused unit tests (most of these are pure functions). Add a `#[tokio::test]` that drops a `Db::read` future mid-flight and then runs `readers + 1` reads.
    **Status:** fixed in ecf5dca, 451c63c, 49a14f0, 37ae965, 99b94b2 (Db::read cancel/panic, stats buckets, cursors, paging ties, redact, key_from_headers, forwarded IP, map_url_for, find_by_url map url, Window::parse).

## Low

20. **low** — `crates/sdl-server/src/db/read.rs:389`, `:442-447` — An invalid `before` cursor is ignored silently, and the first page comes back again. A client that pages until `next_cursor` is `None` can loop forever.
    Fix: return 400 for a cursor that does not parse (parse it in `api::list_events`).
    **Status:** fixed in 37ae965.

21. **low** — `crates/sdl-protocol/src/symbolicate.rs:179` — `line.saturating_sub(1) as u32` truncates an untrusted `u64`. A `lineno` of 2^32+1 looks up line 0 and symbolicates a frame wrongly.
    Fix: `let (Ok(line0), Ok(col0)) = (u32::try_from(line - 1), u32::try_from(col - 1)) else { return false };`, after checking `line >= 1`.
    **Status:** fixed in 03e5430.

22. **low** — `crates/sdl-protocol/src/symbolicate.rs:231` — `apply` clones each holder's whole `stacktrace` before it knows whether any frame changes.
    Fix: clone lazily on the first rewritten frame (or clone only when `changed > 0` by doing a quick first pass to see which frames match).
    **Status:** fixed in 03e5430.

23. **low** — `crates/sdl-server/src/db/mod.rs:310-321` — `Drop for Db` does a blocking `send` (it waits if the queue is full) and `join`s the writer, which runs a final flush to object storage. When the last `Server` is dropped on a Tokio worker, this blocks the runtime for seconds.
    Fix: in `Drop`, only `try_send` the shutdown and log a warning if `shutdown()` was not called. Or document that `shutdown().await` is required.
    **Status:** fixed in c999d13 (Drop only warns; dropping the sender stops the writer).

24. **low** — `crates/sdl-protocol/src/dsn.rs:42-52`, `crates/sdl-server/src/config.rs:226` — `Dsn::format` ignores the error from `set_username`. For a cannot-be-a-base URL such as `data:x` (which `validate()` accepts), it returns a DSN with no key. The name `format` also suggests `Display`.
    Fix: in `Config::validate`, require an `http`/`https` `public_url` with a host. Rename to `Dsn::build(...) -> Result<String, DsnError>` (or build a `Dsn` and `impl Display`).
    **Status:** fixed in 93acef6.

25. **low** — `crates/sdl-server/src/config.rs:15`, `:64` — `#[derive(Debug)]` on `Config` prints `api_token` in plain text. It is not logged today, but `?cfg` in any trace line would leak it.
    Fix: wrap it in a `Secret` newtype with a redacting `Debug` (or use `secrecy::SecretString`), or write `Debug` by hand.
    **Status:** fixed in ded3e52 (`Secret` newtype).

26. **low** — `crates/sdl-server/src/db/writer.rs:125-138`, `crates/sdl-server/src/db/schema.rs:8` — `max(value)` on a `VARCHAR` sorts as text, so `"10" < "9"`. From schema version 10 on, a row would be inserted at every start. `.ok().flatten()` also swallows query errors. The doc mentions a `migrate` function that does not exist.
    Fix: `max(value::INTEGER)` (or order by `updated_at`), propagate errors, and fix the doc comment.
    **Status:** fixed in 0d8243f.

27. **low** — Several places use strings or tuples where a type would be clearer:
    - `crates/sdl-types/src/lib.rs:22-38`: `IssueStatus::parse` should be `impl FromStr` (+ `Display`).
    - `crates/sdl-protocol/src/symbolicate.rs:36` (`resolved_with: &'static str`) and `crates/sdl-server/src/sourcemaps.rs:125` (`state: &'static str`): use small serde enums.
    - `crates/sdl-server/src/sourcemaps.rs:97` returns `(Vec<NewBundleFile>, Option<String>, Option<String>)`: use a named struct.
    - `crates/sdl-protocol/src/useragent.rs:9-12` (`Option<(String, String)>`): use a struct `{ name, version }`.
    - `crates/sdl-server/src/db/writer.rs:30`: `Result<(), String>` loses the error chain; use `Result<(), Arc<anyhow::Error>>`.
    - `crates/sdl-server/src/db/read.rs:29`: `Window::parse -> Result<_, String>`.
    **Status:** fixed in fd9d95e (IssueStatus), fa627b9 (useragent), 967b33f (rest).

28. **low** — `crates/sdl-server/src/db/read.rs:86-104`, `:162-163`, `:195-204` — `clone_param` rebuilds `Box<dyn ToSql>` values by calling `to_sql()` and matching on the output variant. Anything that is not text silently becomes `NULL`.
    Fix: keep the filter values as `Vec<String>` (or `Vec<&str>` borrowed from `q`) and build `Vec<&dyn ToSql>` for each statement.
    **Status:** fixed in 37ae965.

29. **low** — `crates/sdl-server/src/ingest.rs:164`, `crates/sdl-server/src/storage.rs:53`, `crates/sdl-server/src/db/mod.rs:324` — Some helpers are in odd places or copied.
    `constant_time_eq` lives in `ingest` but `api` uses it, and `sql_str` lives in `storage` but `db` uses it. Both, and `redact`, are duplicated in `odl-server` (`auth.rs:37`, `store/mod.rs:29,58`), and the sdl `redact` is the weaker copy.
    Fix: move them into a small shared module (an `auth` module and a `sql` module, or a tiny shared crate). Use `subtle::ConstantTimeEq` instead of the hand-rolled loop. Raise this as an architecture point, since it crosses the sdl/odl boundary.
    **Status:** deferred: cross-side.

30. **low** — `crates/sdl-server/src/api.rs` (480 lines) — One file mixes the `/v1` API, the OpenAPI document, auth middleware, meta routes and the sentry-cli `/api/0` compat shims.
    Fix: split it into `api/{mod,v1,compat,meta}.rs`. Similarly, `sourcemaps.rs` could separate `upload` (chunks and assemble) from `resolve`.
    **Status:** deferred: not small; do it with the cross-side step that moves auth and JSON errors out of api.rs.

31. **low** — Needless clones and allocations:
    - `writer.rs:388,496,526,542`: `let ns = &self.ns.clone()` exists only because the methods take `&mut self` without needing it. Take `&self`.
    - `writer.rs:393`: `HashMap<(u32,u64),()>` should be a `HashSet`.
    - `db/mod.rs:55-64`: `bundle_known` and `project_has_release` allocate a `String` per lookup, and `project_has_bundles` (called for every ingested event) scans all bundles. Key by project: `HashMap<u32, HashSet<String>>`.
    - `api.rs:202,216`: `req.slug.clone()` and `req.platform.clone()` on an owned `req`.
    - `grouping.rs:60-63`: `is_default_marker` allocates for every fingerprint part.
    - `event.rs:368-369`: `normalize_level` allocates with `to_ascii_lowercase`; use `eq_ignore_ascii_case`.
    **Status:** fixed in cf52cf1 (bundle caches), fa627b9 (grouping, event), 5727121 (writer, api).

32. **low** — `crates/sdl-types/src/lib.rs:237-287` — `#[serde(default)]` on `Option<T>` fields does nothing, because serde already treats a missing `Option` as `None`. `TraceQuery` derives `Default` although `trace_id` is required, so `TraceQuery::default()` is never valid.
    Fix: remove those attributes, and remove `Default` from `TraceQuery`.
    **Status:** partly fixed in a227ed0 (`#[serde(default)]` removed); removing `Default` from `TraceQuery`: deferred: sdl-client and sdl-cli build it with `..Default::default()`.

33. **low** — `crates/sdl-protocol/src/event.rs:314-316` — `derive_tags` deduplicates on the full key but stores the truncated key. Two keys that share their first 200 chars produce duplicate tag keys (and one of them is later dropped by the JSON map).
    Fix: truncate first, then insert into `seen`.
    **Status:** fixed in fa627b9.

34. **low** — `crates/sdl-protocol/src/event.rs:286-292`, `crates/sdl-server/src/db/read.rs:17-19` — `parse_span_id` and `parse_group_id` use `from_str_radix`, which accepts a leading `+`. So `"+00f067aa0ba902b"` is a valid 16-char id, unlike `parse_trace_id`, which checks for hex digits.
    Fix: add `s.bytes().all(|b| b.is_ascii_hexdigit())`.
    **Status:** fixed in 37ae965 (group id), fa627b9 (span id).

35. **low** — `crates/sdl-server/src/ingest.rs:260` — `|_| sentry_error(...)` drops the `JoinError`. A panic in normalization or symbolication returns 500 with no log line.
    Fix: `|e| { tracing::error!(error = %e, "event processing panicked"); sentry_error(...) }`.
    **Status:** fixed in 55b1b90.

36. **low** — `crates/sdl-server/src/main.rs:96` — The spawned metrics server's `Result` is thrown away, so a failing metrics listener is silent.
    Fix: `if let Err(e) = axum::serve(..).await { tracing::error!(...) }`.
    **Status:** fixed in 6cd1a63.

37. **low** — `crates/sdl-server/src/storage.rs:79-80`, `:150-158`, `:232` — If `GCS_HMAC_KEY_ID` or `GCS_HMAC_SECRET` is missing, DuckDB gets a secret with empty `KEY_ID`/`SECRET`, while `object_store` falls back to the AWS env credentials. The failure shows up later and is hard to trace.
    Fix: fail in `Storage::new` when either variable is missing for `gs://`.
    **Status:** fixed in 6cd1a63.

38. **low** — `crates/sdl-server/src/db/read.rs:523`, `crates/sdl-server/src/db/mod.rs:332-334` — A corrupt stored payload silently becomes `null`, and an out-of-range timestamp silently becomes 1970-01-01. Both hide data problems.
    Fix: log a warning (or return an error) instead of `unwrap_or(Null)` / `unwrap_or_default()`.
    **Status:** fixed in 6cd1a63.

39. **low** — `crates/sdl-server/src/lib.rs:62`, `crates/sdl-server/src/storage.rs:94-96` — `Server::new` (async) calls `Storage::new`, which does blocking `create_dir_all` and `canonicalize`. This is startup only, but it goes against the crate's own rule of keeping blocking work off async threads.
    Fix: build `Storage` inside the `spawn_blocking` that already opens the DB.
    **Status:** fixed in 6cd1a63.

40. **low** — `crates/sdl-server/src/lib.rs:126` — `run_background(impl Future + Send + 'static) -> ()` differs from `odl_server::Server::run_background(impl Future) -> Result<()>`. The same concept has two signatures in sibling crates that `mallard-server` drives together.
    Fix: match the odl signature (no `Send + 'static` bound is needed, because the future is only awaited).
    **Status:** fixed in 6cd1a63.
