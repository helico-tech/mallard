# Embedding otel-ducklake in your own Rust application

The server, client, CLI and TUI are libraries. The `otel-ducklake` and `odl`
binaries are thin wrappers around them. The libraries install no global state
(no tracing subscriber, no metrics recorder).

## Server

```rust
use odl_server::{Server, config::Config};

// Config: build in code, deserialize it as a section of your own TOML,
// or load file + env with your own prefix.
let cfg = Config::load_with_prefix(Some("app.toml".as_ref()), "MYAPP_OTEL_")?;

let server = Server::new(cfg).await?;            // opens DuckDB, attaches the lake
let api = server.api_router();                    // /api/v1/..., /openapi.json, /docs (relative)
let otlp_http = server.otlp_http_router();        // POST /v1/traces|logs|metrics
let grpc = server.grpc_routes();                  // tonic::service::Routes (3 OTLP services)

let app = axum::Router::new().nest("/otel", api).merge(otlp_http);
// serve `app` and `grpc` on your own listeners, then:
server.run_background(async { shutdown_signal().await }).await?; // flusher + maintenance; drains on shutdown
```

`Server::serve(cancel_token)` does all of this with the configured listen
addresses; that is what the binary uses.

### Sharing one DuckDB / DuckLake with other components

```rust
let conn = odl_server::store::open_connection(&cfg)?; // settings, extensions, secrets, ATTACH ... AS lake
let otel = Server::with_connection(otel_cfg, conn.try_clone()?).await?;   // schema "otel"
// another component uses conn.try_clone()? with schema "sentry", etc.
```

- `with_connection` expects the catalog attached as `lake`. It only creates
  its own schema and tables, uses `try_clone()` of the connection, and never
  changes global DuckDB settings.
- Merges and retention are scoped to this schema. Snapshot expiry and file
  cleanup are catalog-wide: when several applications share one catalog, set
  `maintenance.catalog_wide = true` in exactly one of them (replicas of the
  same application may all run it).
- Pin the same `duckdb` version (`=1.20000.1`) and features (`bundled`,
  `appender-arrow`, `chrono`) in the whole workspace, and build
  `libduckdb-sys` without debug assertions:

  ```toml
  [profile.dev.package.libduckdb-sys]
  opt-level = 3
  debug = false
  ```

## Client

```rust
let c = odl_client::Client::new("http://localhost:8080", Some(token))?;
let svcs = c.services(&odl_client::api::ServiceQuery { since: Some("1h".into()), ..Default::default() }).await?;
let mut tail = odl_client::LogTail::new(c.clone(), Default::default(), chrono::Utc::now());
let new_records = tail.poll().await?;
```

## CLI and TUI

```rust
// Mount the `odl` commands under your own CLI:
#[derive(clap::Subcommand)]
enum Top { #[command(subcommand)] Otel(odl_cli::Cmd), /* ... */ }
odl_cli::run(cmd, &odl_cli::Ctx { client, output: odl_cli::Output::Table }).await?;

// Embed the TUI as a component (you own the terminal and the event loop):
let mut otel = odl_cli::tui::Embedded::new(client);
// in your loop: otel.tick(); terminal.draw(|f| otel.draw(f, area)); match otel.handle_event(&ev) { .. }
```

F1 and F2 are left to the host. `Outcome::OpenErrors(trace_id)` signals that
the user asked for errors linked to a trace (for example in an error
tracker); `Embedded::open_trace(id)` jumps to a trace.
