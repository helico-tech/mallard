---
status: done
created: 2026-11-01
---
# 08 — CLI for agents

## Goal
AI coding agents can do every common observability job with the CLI: find errors and their traces/logs, check releases, query metrics, tail logs, manage issues, set up projects. Based on a researched spec.

## Acceptance criteria
- Spec in docs (jobs → commands, output and exit-code contract).
- P0 commands implemented with stable JSON output and tests.
- Agent-facing help doc (how to use mallard as an agent).

## Plan
Spec: [docs/specs/agent-cli.md](../../../docs/specs/agent-cli.md) (researched 2026-11-01). Implement P0 (spec §9, items 1–15) on the flattened command tree (option B).

1. Server (additive, each side owns its API):
   - sentry: `events` list gets `since/from/to` (S1); `issues?new=true&first_release=` (S2);
     `PATCH issues/{id}` takes `reason`, answers `changed` (S3); `/v1/info` adds
     `public_url`, `retention_secs`, `max_window_secs` (S4); `GET /v1/events/{event_id}`
     across projects (S7). S6 (one error shape) was already done in ADR 0005.
   - otel: `/api/v1/stats` adds `capabilities` (SQL on/off, max window, retention,
     flush interval, ingest auth) (S5).
   - Regenerate both OpenAPI snapshots.
2. `mallard-term`: the contract, shared by both CLIs: `Format` (text/json/ndjson,
   compact JSON when piped), `Output` (fields filter, envelope, `--raw`), `List`
   envelope, plain tables when piped, error kinds → exit codes, JSON errors on stderr,
   shared window flags (`--since/--from/--to`, relative `--from 2h`).
3. `sdl-cli` / `odl-cli`: use `Output`; new flags (`--new`, `--first-release`,
   `--reason`, `--if-not-exists`, `--raw`, `logs --around`); concise `latest_event` /
   normalized `events show`; Args structs so `mallard` can mount single commands.
4. `mallard`: flat tree (`issues|events|projects|traces|logs|metrics|services|ops|trace|
   find|setup|status|tui|help`), hidden `errors …`/`telemetry …` aliases (same commands),
   `status` exit codes + capabilities, `setup`, `find`, `help agents` (= `docs/agents.md`),
   examples in every `--help`.
5. Tests: unit (contract pieces), parse tests (tree + aliases), integration test that runs
   the real `mallard` binary against an in-process server.
6. Docs: ADR 0008, `docs/agents.md`, `docs/cli.md`, README, AGENTS.md.

## Progress log
- 2026-11-01: plan written; started with the server endpoints.
- 2026-11-01: server (S1–S5, S7): sentry event-list window, `issues?new=&first_release=`,
  `PATCH issue {status, reason}` answers `changed` (same status twice writes nothing),
  `/v1/info` `public_url`/`retention_secs`/`max_window_secs`, `GET /v1/events/{id}`;
  otel `capabilities` in `/api/v1/stats`. Tests in `sdl-server/tests/integration.rs`
  (`agent_endpoints`) and `odl-server/tests/e2e.rs`. OpenAPI snapshots regenerated.
  S6 was already done (ADR 0005). `slugify` moved to `sdl-types` (CLI needs it).
- 2026-11-01: `mallard-term`: `error::Kind` (exit codes), `Described {message, hint, kind,
  status}`, JSON error line; `output` (`Format`, `Output`, `List` envelope, `--fields`);
  `window` (`--since/--from/--to`, relative times); `table` plain when piped.
- 2026-11-01: `sdl-cli`/`odl-cli` on `Output`; Args structs for mounting; concise event
  JSON (`sdl_cli::concise`); `--new`, `--first-release`, `--reason`, `--if-not-exists`,
  `logs --around/--window`, `--raw`. 503 is now "server busy" (unavailable), not
  "server error"; unreachable is exit 6 (tests updated to the new contract).
- 2026-11-01: `mallard`: flat tree, hidden `errors`/`telemetry` aliases, `status`
  (capabilities, exit 3/6), `setup`, `find`, `trace` (resources once per service),
  `help agents` (= `docs/agents.md`, test checks), examples on every command (test checks).
  Usage errors print JSON on stderr when JSON output was asked for.
- 2026-11-01: tests: unit (kinds/exit codes, envelope, fields, format pick, windows,
  plain tables, concise event/trace, setup env, find ids), parse tests for the tree and
  aliases, `crates/mallard/tests/cli.rs` (binary without a server), and
  `crates/mallard-server/tests/agent_cli.rs` (binary against an in-process server).
  ADR 0008, docs/cli.md, README, AGENTS.md, architecture.md updated.

## Follow-ups
- P1 list: [issue 0004](../../issues/0004-agent-cli-p1.md) (schema, `--wait`, `logs -f --for`,
  `ops --compare`, `series`, tag filters, span ids in `find`, cursors, releases,
  Sentry SQL, `setup --sentry-cli`, `--quiet`).
- `agent_cli.rs` uses `target/<profile>/mallard`; run alone after a change to the CLI,
  build it first (`cargo build -p mallard`): `cargo test --workspace` does this.
