# Architecture

```
   Sentry SDKs (browser, node, python, rust, ...)        sentry-cli / bundler plugins
            │ POST /sentry/api/{project}/envelope/              │ /api/0/... (token)
            v                                                   v
 ┌──────────────────────────── mallard (one process) ──────────────────────────────┐
 │  ingest.rs            sourcemaps.rs                api.rs (/sentry/v1, token)    │
 │  auth, decompress,    chunk upload, assemble,      issues, events, stats,       │
 │  CORS, 429            bundle index, resolver ──┐   projects, OpenAPI, docs      │
 │      │                                         │          │                      │
 │      v                                         │          v                      │
 │  mallard-sentry-protocol (pure): envelope -> normalize -> symbolicate -> group  │
 │      │                                         │          │                      │
 │      v  bounded queue                          │          │ reader pool          │
 │  db/writer.rs: ONE thread, group commit        │          │ (cloned conns)       │
 │      │                                         │          │                      │
 └──────┼─────────────────────────────────────────┼──────────┼──────────────────────┘
        v                                         v          v
   DuckDB 2.0 + DuckLake ──── catalog: DuckDB file | SQLite | PostgreSQL
        │                     data:    Parquet under storage.data_path, schema sentry
        └──────────── object storage (S3, GCS, Azure) or local disk ─────────────┐
                      sourcemap bundles under <artifacts_path>/artifacts/       │
                                                                                 ┘
   mallard CLI / TUI  ──(mallard-sentry-client, /sentry/v1)──>  api.rs
```

The paths in the code are relative. `mallard-server` mounts this router under
`/sentry`, and the `/api/0` routes for `sentry-cli` also on the root. The
[main architecture page](../architecture.md) shows how it sits next to the
telemetry side and the bridge.

## Crates

| Crate | Role | Depends on |
|---|---|---|
| `mallard-sentry-protocol` | Sentry wire protocol, no I/O: DSN/auth parsing, envelopes, lenient event normalization, grouping, sourcemap symbolication | serde, sourcemap |
| `mallard-sentry-api` | `/v1` request and response types; feature `openapi` derives the schema | serde, utoipa |
| `mallard-sentry-server` | library (`Server`): ingest, writer, maintenance, `/v1` API, sourcemap upload | protocol, api, `mallard-duck`, axum, object_store |
| `mallard-sentry-client` | typed async client for `/v1` | api, reqwest |
| `mallard-sentry-cli` | CLI commands + TUI; library (`Command`, `run`, `Embedded`) | client, ratatui |
| `mallard-sentry-loadgen` | binary: load generator and query benchmark | client |

## Request flow (ingest)

1. **HTTP**: `RequestDecompressionLayer` handles `Content-Encoding` (gzip,
   deflate, br, zstd). Body size is capped after decompression
   (`sentry.ingest.max_body_bytes`). CORS mirrors the origin, so browser SDKs work from
   any site (the DSN key is public by design).
2. **Auth**: the public key comes from `X-Sentry-Auth`, `Authorization:
   Sentry ...`, `?sentry_key=`, or the envelope header `dsn`, and must match
   the project in the URL.
3. **Envelope**: items are parsed; `event` items are processed. `transaction`
   and `span` items go to the bridge, which stores them as OTel spans; with
   `bridge.transactions = false` they are dropped. All other item types (sessions,
   replays, attachments, client reports, ...) are accepted and dropped (see
   [compatibility.md](compatibility.md)).
4. **Normalize** (`mallard-sentry-protocol::event`): one canonical shape regardless of SDK
   (exception list vs `{values}`, float vs RFC 3339 timestamps, tags as map or
   pairs, `message` vs `logentry`, `{{auto}}` IP, User-Agent -> browser/os).
5. **Symbolicate**: frames' minified files are resolved to sourcemaps (debug id
   first, then release + dist + url), frames are rewritten, originals kept in
   `raw_stacktrace`. Lookups and parsed maps are cached.
6. **Group** (`mallard-sentry-protocol::grouping`): fingerprint -> 64-bit hash (see
   below). Title and culprit are derived like Sentry's.
7. **Queue + commit**: the event goes to the writer queue with a reply
   channel; the HTTP response is sent after the batch commits. Duplicate
   event ids (SDK retries) within an hour are acknowledged but not stored
   twice.

## Grouping

1. A custom `fingerprint` wins; `{{ default }}` expands to the default hash.
2. Each exception in the chain contributes its type plus its in-app frames
   (all frames if none is in-app). A frame is `module|filename` + `function`,
   normalized: bundle hashes (`main.4f3a2b1c.js`), query strings, line numbers
   and anonymous function names do not split issues.
3. An exception without frames uses type + value with variable parts masked
   (numbers, UUIDs, hex, dates, IPs, emails).
4. Then a thread / top-level stack trace, then the message template.

Symbolication runs **before** grouping, so a JS issue survives rebuilds.

## Concurrency model

- **One writer thread** (`ducklake-writer`, a plain OS thread) owns the write
  connection. Row writes from anywhere else are bugs. This rules out DuckLake
  write conflicts inside this side and makes batching natural. Its commits take
  a `mallard_duck::CommitLock` shared with the OTLP flusher, so they do not starve it
  on a shared connection ([ADR 0030](../adr/0030-one-lake-commit-at-a-time.md)).
- **One maintenance thread** (`ducklake-maintenance`, own connection) flushes
  inlined data and compacts, next to the writer. Retention runs on the writer
  thread: its `DELETE` conflicts with concurrent inserts
  ([ADR 0014](../adr/0014-sentry-maintenance-thread.md)). Snapshot expiry and file
  cleanup are catalog-wide, and the telemetry side runs them (ADR 0001).
- **Reader pool**: `sentry.query.readers` connections cloned from the same DuckDB
  instance, used via `spawn_blocking`. Each query sees the latest committed
  snapshot.
- **Caches** (projects, issue status, known bundles) live in memory and are
  updated by the writer after a commit. Several processes can share a Postgres
  catalog: a cache miss reads the catalog, the writer reads what others wrote
  every 30 s, and one process runs maintenance
  ([ADR 0021](../adr/0021-sentry-several-processes.md)).

## How mallard uses the library

`mallard_sentry_server::Server::with_connection(config, host, conn)` takes a
connection that already has the catalog attached as `lake`
(`mallard_duck::open_connection`) and only creates the `sentry` schema. `Config`
is the `[sentry]` section of the config file. `Host` is what mallard decides:
public URL (`<public_url>/sentry`), API tokens, sourcemap storage, and that the
telemetry side owns catalog-wide maintenance. `mallard-server` mounts `router()`
and calls `shutdown()`, which drains the queue and flushes. The library installs
no global tracing subscriber or metrics recorder.

## Decisions

[adr/](adr/) holds this side's decisions from before the merge; newer ones are in
[docs/adr/](../adr/).
