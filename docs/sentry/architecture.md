# Architecture

```
   Sentry SDKs (browser, node, python, rust, ...)        sentry-cli / bundler plugins
            │ POST /api/{project}/envelope/                     │ /api/0/... (token)
            v                                                   v
 ┌──────────────────────────── mallard (one process) ──────────────────────────────┐
 │  ingest.rs            sourcemaps.rs                api.rs (/v1, token)          │
 │  auth, decompress,    chunk upload, assemble,      issues, events, stats,       │
 │  CORS, 429            bundle index, resolver ──┐   projects, OpenAPI, /docs     │
 │      │                                         │          │                      │
 │      v                                         │          v                      │
 │  mallard-sentry-protocol (pure): envelope -> normalize -> symbolicate -> group  │
 │      │                                         │          │                      │
 │      v  bounded queue                          │          │ reader pool          │
 │  db/writer.rs: ONE thread, group commit        │          │ (cloned conns)       │
 │      │                                         │          │                      │
 └──────┼─────────────────────────────────────────┼──────────┼──────────────────────┘
        v                                         v          v
   DuckDB 2.0 + DuckLake ──── catalog: DuckDB file | PostgreSQL | SQLite
        │                     data:    Parquet under <storage>/lake/
        └──────────── object storage (S3 default, GCS, Azure, local) ────────────┐
                      sourcemap bundles under <storage>/artifacts/ (object_store) │
                                                                                 ┘
   mallard CLI / TUI  ──(mallard-sentry-client, /v1)──>  api.rs
```

## Crates

| Crate | Role | Depends on |
|---|---|---|
| `mallard-sentry-protocol` | Sentry wire protocol, no I/O: DSN/auth parsing, envelopes, lenient event normalization, grouping, sourcemap symbolication | serde, sourcemap |
| `mallard-sentry-api` | `/v1` request/response types; `openapi` feature derives the schema | serde, utoipa |
| `mallard-sentry-server` | library (`Server`) | protocol, types, duckdb, axum, object_store |
| `mallard-sentry-client` | typed async client for `/v1` | types, reqwest |
| `mallard-sentry-cli` | CLI commands + TUI; library (`Command`, `run`, `Embedded`) | client, ratatui |
| `mallard-sentry-loadgen` | load generator and query benchmark | client |

## Request flow (ingest)

1. **HTTP**: `RequestDecompressionLayer` handles `Content-Encoding` (gzip,
   deflate, br, zstd). Body size is capped after decompression
   (`MALLARD_SENTRY__INGEST__MAX_BODY_BYTES`). CORS mirrors the origin, so browser SDKs work from
   any site (the DSN key is public by design).
2. **Auth**: the public key comes from `X-Sentry-Auth`, `Authorization:
   Sentry ...`, `?sentry_key=`, or the envelope header `dsn`, and must match
   the project in the URL.
3. **Envelope**: items are parsed; `event` items are processed, all other item
   types (transactions, sessions, replays, attachments, client reports, ...)
   are accepted and dropped (see [compatibility.md](compatibility.md)).
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
  write conflicts inside the process and makes batching natural.
- **One maintenance thread** (`ducklake-maintenance`, own connection) flushes
  inlined data, compacts and expires snapshots, next to the writer. Retention
  runs on the writer thread: its `DELETE` conflicts with concurrent inserts
  ([ADR 0014](../adr/0014-sentry-maintenance-thread.md)).
- **Reader pool**: `MALLARD_SENTRY__QUERY__READERS` connections cloned from the same DuckDB
  instance, used via `spawn_blocking`. Each query sees the latest committed
  snapshot.
- **Caches** (projects, issue status, known bundles) live in memory and are
  updated by the writer after a commit. Several processes can share a Postgres
  catalog: a cache miss reads the catalog, the writer reads what others wrote
  every 30 s, and one process runs maintenance
  ([ADR 0021](../adr/0021-sentry-several-processes.md)).

## Embedding

`mallard_sentry_server::Server` is a library: `Server::with_connection(config, host,
conn)` shares a connection that already has the catalog attached as `lake`
(`mallard_duck::open_connection`) and only creates the `sentry` schema.
`Config` is the library's own settings (mallard's `[sentry]` section);
`Host` is what the application decides (public URL, API tokens, sourcemap
storage, catalog-wide maintenance). Mount `router()` /
`ingest_router()` / `api_router()` anywhere (paths are relative; DSNs follow
`public_url`, which may include a prefix). The library installs no global
tracing subscriber or metrics recorder. `shutdown()` drains the queue and
flushes. mallard uses this to run Sentry ingest next to OTLP on one
catalog.

## Decisions

See [docs/adr/](adr/).
