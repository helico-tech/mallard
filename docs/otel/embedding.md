# Library use

The server, client, CLI and TUI of this side are libraries. The `mallard` binary and
`mallard-server` are the code that uses them; read those for a full example. The
libraries install no global state (no tracing subscriber, no metrics recorder).

## Server

`mallard_server::Platform::new` in `crates/mallard-server/src/lib.rs` does this:

```rust
let conn = mallard_duck::open_connection(&storage, &duckdb)?; // settings, extensions, secrets, ATTACH ... AS lake
let otel = mallard_otel_server::Server::with_connection(otel_cfg, conn.try_clone()?).await?;   // schema "otel"
let sentry = mallard_sentry_server::Server::with_connection(sentry_cfg, host, conn).await?;    // schema "sentry"

let app = axum::Router::new().nest("/otel", otel.api_router()).merge(otel.otlp_http_router());
let grpc = otel.grpc_routes();                    // tonic::service::Routes (3 OTLP services)
// serve `app` and `grpc` on your own listeners, then:
otel.run_background(async { shutdown_signal().await }).await?; // flusher + maintenance; drains on shutdown
```

- `with_connection` expects the catalog attached as `lake`. It only creates
  its own schema and tables, uses `try_clone()` of the connection, and never
  changes global DuckDB settings.
- Merges and retention are scoped to this schema. Snapshot expiry and file
  cleanup are catalog-wide and always run with `maintenance.enabled` (on the
  replica holding the maintenance lease, a row in `otel.meta`): the otel side owns
  them on a shared catalog, and the Sentry side leaves them alone
  (`Host::catalog_maintenance = None`).
- `set_ingest_hook` and `ingest_traces` are the bridge's way in (ADR 0009).
- `Server::new` opens its own connection and lake. Only the tests use it. The library
  binds no listeners: the host serves the routers.
- Use DuckDB only through `mallard_duck::duckdb` (one version for the whole
  workspace), and build `libduckdb-sys` without debug assertions (the profile in the
  root `Cargo.toml`).

## Client

```rust
let c = mallard_otel_client::Client::new("http://localhost:8080/otel", Some(token))?;
let svcs = c.services(&mallard_otel_client::api::ServiceQuery { since: Some("1h".into()), ..Default::default() }).await?;
let mut tail = mallard_otel_client::LogTail::new(c.clone(), Default::default(), chrono::Utc::now());
let new_records = tail.poll().await?;
```

The base URL includes `/otel`. `mallard-client` wraps this client and the Sentry one
behind one server URL.

## CLI and TUI

`crates/mallard/src/main.rs` mounts the commands and the component:

```rust
// Mount the telemetry commands under your own CLI:
#[derive(clap::Subcommand)]
enum Top { #[command(subcommand)] Otel(mallard_otel_cli::Command), /* ... */ }
let result = mallard_otel_cli::run(cmd, &mallard_otel_cli::Ctx::new(client).with_json(false).with_hints(my_hints)).await;
// `error: …` on stderr and an ExitCode, as every mallard CLI does:
let code = mallard_term::error::report(result, mallard_term::color::ColorChoice::Auto);

// Embed the TUI as a component (you own the terminal and the event loop):
let mut otel = mallard_otel_cli::Embedded::new(client);
// in your loop: otel.tick(); terminal.draw(|f| otel.draw(f, area)); match otel.handle_event(&ev) { .. }
```

Requirements and hooks:

- Call `tick()` and `handle_event()` from inside a tokio runtime (loads run
  as tokio tasks); without one the TUI shows "no tokio runtime". Don't block
  the runtime thread waiting for terminal events: use crossterm's
  `EventStream` or a separate thread.
- `Embedded::set_errors_enabled(true)` shows the `x errors` hint when your
  host handles `Outcome::Jump` (off by default).
- An error tracker: implement `mallard_otel_cli::ErrorLookup` (error events of many
  trace ids in one call) and pass it to `Embedded::set_error_lookup` and
  `Ctx::with_error_lookup`. Trace lists then show an error count per trace (`sentry`
  column, JSON `sentry_errors`), waterfalls mark spans with `⚠`, and `x` on such a
  span returns `Outcome::JumpEvent { trace_id, event_id }`. Without one nothing
  changes.
- Keys with Ctrl/Alt modifiers (except Ctrl-C) and F1/F2 return `Ignored`.

F1 and F2 are left to the host. `Outcome::Jump(trace_id)` (the shared
`mallard_term::tui::Outcome`) signals that the user asked for errors linked to a
trace; `Embedded::open_trace(id)` jumps to a trace.
