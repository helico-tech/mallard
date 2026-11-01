---
status: done
created: 2026-11-01
---
# 10 — One `mallard` binary

## Goal
One thing to install: `mallard serve` runs the server, `mallard tui` and the
CLI talk to it over HTTP. The user chose this on 2026-11-01 (option A) over
two binaries (ADR 0007) and over `mallard serve` starting a second binary.

## Acceptance criteria
- `crates/mallard` builds the only product binary. Feature `server` is on by
  default and adds `serve`, `check [--connect]`, `install-extensions` and
  `maintenance [--full]`, from the `mallard-server` library crate.
- `--no-default-features` builds the same CLI and TUI with no DuckDB in
  `cargo tree`; CI checks this.
- `mallard-server` stays a library crate and has no binary.
- Docker, compose, Helm, CI, e2e, scripts and docs use `mallard serve`.
- A new ADR supersedes the binary split in ADR 0007. The crate split stays.

## Plan
1. Wait until the agents now running have merged; they change the same files.
   (Done: branched from main after the Sentry ops merge.)
2. Move `mallard-server`'s `main.rs` commands behind the feature. The clap
   definitions live in `crates/mallard/src/server.rs` and exist in both builds,
   so a slim `mallard serve` gets a usage error, not "unknown command". The
   logic goes in `mallard_server::commands`.
3. Move the tests that run the binary from `mallard-server/tests` to
   `crates/mallard/tests`, to avoid a dependency cycle. They use `mallard-server`
   as the optional normal dependency (`required-features = ["server"]`), NOT a
   dev-dependency: that would put DuckDB in the slim `cargo tree` and `cargo test`.
4. Update image, chart, CI, e2e, scripts, docs, ADR.

## Progress log
- 2026-11-01: Commands moved: clap definitions in `crates/mallard/src/server.rs`
  (both builds, hidden in the slim one), logic in `mallard_server::commands`.
  `main.rs` of `mallard-server` removed, and its `clap` dependency.
- Tests: `agent_cli`, `agent_cli_p1`, `maintenance` moved to `crates/mallard/tests`,
  use `CARGO_BIN_EXE_mallard` (no more looking for a possibly stale binary) and share
  `mallard-server/tests/common` through `#[path]`. Assertions unchanged. New:
  `serve.rs` (`serve` + `status`, SIGTERM, empty stdout), `check` and slim-build tests
  in `cli.rs`.
- Found on the way: `init_tracing` logged to stdout, which would mix logs into
  `mallard maintenance`'s data. All logs go to stderr now.
- Also added: `check`, `maintenance`, `install-extensions` answer JSON with
  `--json`, and a bad config is exit 2 (ADR 0008).
- Helm and compose needed no change: they set no binary name, and the image's
  entrypoint plus `args: ["serve"]` give `mallard serve`.
- Checked: fmt, clippy and tests of both builds; slim tree has no duckdb (also with
  dev-dependencies, `-e all`); e2e 28/28; helm lint and template on all three CI
  values; image built and smoke tested (`/readyz`, `mallard status` in the
  container); by hand `mallard serve` + `mallard status`. Sizes in ADR 0023.
- Not run: `deploy/helm/kind/test.sh` (CI only); the new CI step itself (its
  commands were run by hand).

## Follow-ups
None found.
