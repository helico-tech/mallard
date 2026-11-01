# mallard CLI for agents — research and spec

Status: P0 implemented in story `work/epics/0002-mallard/08-agent-cli.md` (ADR 0008); P1 is `work/issues/0004-agent-cli-p1.md`.
Date: 2026-11-01. Based on the `mallard` debug build in `target/debug/mallard`,
the code in `crates/`, and a throwaway server with seeded data
(161k spans, 46k logs, 500 Sentry events) used to measure real output sizes.

**Short version.** The CLI already covers most jobs on paper. What blocks
agents is the *contract*, not the command count: errors are human text with
exit code 1 for everything, `status` exits 0 when the server is down, JSON
shapes differ between the two sides, JSON is pretty-printed (about 2x the
tokens), there is no way to trim fields, the Sentry side has no `--from/--to`,
and there is no "new in this release", no idempotent project setup, no OTLP
env output, no schema/attribute discovery, and no agent guide. Fix the
contract first (P0), then add a few high-value commands (`setup`, `find`,
`schema`). An MCP server is P2.

---

## 1. Inventory today

### 1.1 Global flags (on every command)

| flag | env | default | notes |
|---|---|---|---|
| `--url` | `MALLARD_URL` | `http://localhost:8080` | |
| `--token` | `MALLARD_TOKEN` | – | bearer token |
| `--json` | – | off | "Print JSON instead of text" |
| `-p/--project` | `MALLARD_PROJECT` | only project if there is one | `errors …` and `tui` only |

No `--format`, `--fields`, `--quiet`, `--no-color`, config file or profiles.

### 1.2 Command tree (from `--help`, recursive)

| command | args / flags | output |
|---|---|---|
| `serve` | `-c/--config` (`MALLARD_CONFIG`) | server |
| `check` | `-c/--config` | validates config |
| `status` | – | text or JSON `{sentry:{ok,info|error}, otel:{ok,info|error}}` |
| `trace <TRACE_ID>` | `-s/--since` | spans + Sentry errors + logs (`{trace_id,spans,errors,logs,logs_truncated,warnings}`) |
| `tui` | `-p` | interactive |
| `errors info` | – | `ServerInfo` |
| `errors projects list` | – | `[Project]` |
| `errors projects create <NAME>` | `--slug`, `--platform` | `Project` (incl. DSN); 409 if slug taken |
| `errors projects show <PROJECT>` | – | `Project` |
| `errors issues list` | `--status unresolved\|resolved\|ignored\|all`, `-q/--query` (title+culprit), `--since` (server default 14d), `--sort last_seen\|first_seen\|count\|users`, `--env`, `--release`, `--limit` (50, max 500) | `{issues:[IssueSummary], window_start, window_end}` |
| `errors issues show <ID>` | `--since` | `IssueDetail` (summary, first/last release, latest_event_id, tag facets, activity) |
| `errors issues resolve\|ignore\|unresolve <IDS>...` | – | per-id result; exit 1 if any failed |
| `errors events list` | `--issue`, `--trace` (all projects), `--limit` | `{events:[EventSummary], next_cursor}` (cursor not exposed as a flag) |
| `errors events show <EVENT_ID>` | `--issue` (faster) | `EventDetail` incl. raw Sentry `payload` |
| `errors stats` | `--since` (24h), `--interval` | `ProjectStats` (buckets, totals, new_issues) |
| `errors sourcemaps list` | – | `[ArtifactBundle]` |
| `errors maintenance` | – | `{"ok":true}` |
| `errors send-test` | `--dsn` (`SENTRY_DSN`), `--message` | sends a test event |
| `errors tui` | – | interactive |
| `telemetry health` | – | `{status,version}`; exit 1 if down |
| `telemetry status` | – | `Stats` (ingest, storage, maintenance) |
| `telemetry services` | `-s/--since` (1h), `--from`, `--to`, `--all-spans` | `[ServiceSummary]` (RED + p50/p95/p99) |
| `telemetry ops` | window, `-S/--service`, `--all-spans` | `[OperationSummary]` |
| `telemetry traces` | window, `-S`, `-O/--operation`, `-e/--errors`, `--min-ms`, `--max-ms`, `-a/--attr k=v` (repeat), `-l/--limit` (30) | `[TraceSummary]` |
| `telemetry trace <ID>` | `-s` (7d), `-A/--attributes` | `{trace_id, spans}` (waterfall in text) |
| `telemetry logs` | window, `-S`, `-l/--level`, `-q/--query`, `-t/--trace`, `-a/--attr`, `-n/--limit` (100), `-f/--follow` | `[LogRecord]`; `-f --json` = NDJSON |
| `telemetry metrics` | window, `-S`, `-q` | `[MetricInfo]` |
| `telemetry metric <NAME>` | window, `-S`, `-g/--agg`, `-b/--by`, `-a/--attr`, `--step` | `MetricQueryResult` |
| `telemetry sql <QUERY>` | `-n/--limit` (100) | `{columns, rows, truncated, elapsed_ms}`; off by default on the server |
| `telemetry tui` | `-s` | interactive |

Aliases: `errors` = `sentry`, `telemetry` = `otel`.

### 1.3 HTTP APIs the CLI can use

**Sentry side** (`crates/mallard-sentry-server/src/api.rs`, mounted at `/sentry`, bearer token):

| route | params |
|---|---|
| `GET /v1/info` | – |
| `GET/POST /v1/projects` | POST `{name, slug?, platform?}` → 201 / 409 |
| `GET /v1/projects/{p}` | id or slug |
| `GET /v1/projects/{p}/issues` | `status, query, since, sort, environment, release, limit` |
| `GET/PATCH /v1/projects/{p}/issues/{id}` | GET `since, interval`; PATCH `{status}` (reason is hard-coded `"user"`) |
| `GET /v1/projects/{p}/events` | `issue, trace_id, limit, before` (cursor) |
| `GET /v1/projects/{p}/events/{event_id}` | `issue` |
| `GET /v1/events` | `trace_id, span_id, since (30d), limit` — across projects |
| `GET /v1/projects/{p}/stats` | `since, interval` |
| `GET /v1/projects/{p}/sourcemaps` | – |
| `POST /v1/maintenance` | – |
| `/api/0/...` | sentry-cli: chunk-upload, artifactbundle/assemble; releases and deploys are **stubs** (accepted, not stored) |
| `/openapi.json`, `/docs`, `/healthz`, `/readyz` | meta |

Errors: `{"error": "<message>"}` (no code field). Window: `since` only, no `from/to`.

**OTel side** (`crates/mallard-otel-server/src/api/mod.rs`, mounted at `/otel`):
`GET /api/v1/health` (no auth), `stats`, `services`, `operations`,
`spans/series`, `traces`, `traces/{id}`, `logs`, `logs/series`, `metrics`,
`metrics/query`, `POST /api/v1/sql`. Window: `since` or `from/to` (RFC 3339),
max `query.max_window` (31d). Errors: `{"error":"<code>","message":"…"}` with
codes `bad_request, unauthorized, disabled, not_found, internal, busy, timeout`.
Not used by the CLI: `spans/series`, `logs/series`.

### 1.4 Behaviour measured (no server, and against a seeded server)

| check | result |
|---|---|
| server down, `mallard --json status` | JSON with `ok:false` on both sides, **exit 0** |
| server down, any other command with `--json` | **human text on stderr** (`Error: … Caused by: 0: … 4: …`), exit 1 |
| bad token | text "invalid token (set MALLARD_TOKEN or --token)", exit 1 |
| not found (issue, trace) | text, exit 1 |
| bad trace id | text, exit 1 (client-side check) |
| clap usage error | exit 2 |
| colours | off when stdout is not a TTY or `NO_COLOR` set (good) |
| text tables when piped | still box-drawing tables + sparklines |
| SQL | only otel tables; `information_schema`, `DESCRIBE`, sentry tables refused → no schema discovery |

Output size (bytes; tokens ≈ bytes/4):

| command | text | `--json` (pretty) | compact JSON |
|---|---:|---:|---:|
| `errors issues list --limit 20` | 16 736 | 14 533 | 7 777 |
| `errors issues show <id>` | 4 601 | 5 349 | 2 560 |
| `errors events show <id>` | 3 561 | 9 269 | 5 630 |
| `errors events list --limit 20` | 5 568 | 11 168 | 8 716 |
| `telemetry traces -e -l 20` | 5 730 | 6 969 | 5 200 |
| `mallard trace <id>` (7 spans) | 698 | 8 085 | 5 990 |
| `telemetry logs -n 50` | 5 097 | 36 217 | 29 016 |
| `status` | 172 | 2 743 | 1 542 |

Lessons: pretty JSON costs ~1.5–2x compact; box tables cost more than
compact JSON; log JSON is 6–7x the text because every record repeats
`resource_attributes`; `events show` JSON carries the full raw payload.

---

## 2. Agent jobs

Realistic things an agent (Claude Code, Codex, Cursor, a CI bot) does:

| # | job | typical trigger |
|---|---|---|
| J1 | **Investigate a 500**: find the error, stack trace, the request's trace, logs around it | failing test, user report, alert |
| J2 | **Did my deploy/release add errors?** new issues in release X, error-rate change | after deploy, PR check |
| J3 | **Top errors** for service/project X in the last hour | triage |
| J4 | **Is the fix working?** has issue Y stopped since time T / release Z | after fix |
| J5 | **Resolve / ignore** an issue with a reason (PR link) | after fix |
| J6 | **Set up** a project: DSN, OTLP env vars, sentry-cli env for CI | new app, new repo |
| J7 | **Latency regression**: which endpoints got slower (p95) after T | perf complaint, deploy |
| J8 | **Tail logs** for service X while running a test | local dev |
| J9 | **Find everything for an id**: trace id, event id, issue id, span id, request id, user id | pasted from a log or a bug report |
| J10 | **Health check**: is mallard up, ingesting, not dropping data | before relying on it, CI smoke test |
| J11 | **Discover the data**: tables/columns, attribute keys and values, metric names and labels | before writing filters or SQL |
| J12 | **Ad-hoc SQL** for questions the commands don't cover | anything |
| J13 | **CI release tasks**: upload sourcemaps, create a release | build pipeline |
| J14 | **Wait for data**: after triggering a test, wait until the error/log/trace arrives (ingest is async, ~5 s flush) | e2e test, repro script |
| J15 | **Verify instrumentation**: did my new app send anything? which services/metrics exist? | after adding an SDK |
| J16 | **When did it start?** error / latency / log volume over time, to line up with a deploy | incident |
| J17 | **Metric check**: value of a metric (queue depth, p99 of a histogram) by label | perf / capacity |

---

## 3. Prior art

**Sentry MCP** (official, hosted at `mcp.sentry.dev`). Tools include
`search_issues`, `search_errors`/`search_events`, `search_issue_events`,
`get_issue_details`, `get_trace_details`, `find_releases`,
`analyze_issue_with_seer`, `get_sentry_resource`, plus logs/traces search.
Notes: can be **scoped to org/project by URL** (discovery tools then hidden);
search tools take **natural language** and need an LLM to translate to
Sentry query syntax; recommends serial calls because of rate limits.
Sources: [getsentry/sentry-mcp](https://github.com/getsentry/sentry-mcp),
[mcp.sentry.dev](https://mcp.sentry.dev/),
[tool folders](https://github.com/getsentry/sentry-mcp/tree/main/packages/mcp-core/src/tools).

**Grafana MCP** (`mcp-grafana`). Per backend a *discover → query* pair:
Loki `list_loki_label_names`, `list_loki_label_values`, `query_loki_logs`,
`query_loki_stats`, `query_loki_patterns`; Tempo `search_tempo_traces`,
`get_tempo_trace`, `list_tempo_attribute_names/values`, `diff_tempo_traces`,
`get_tempo_traceql_docs`; Prometheus `list_prometheus_metric_names`,
`list_prometheus_label_names/values`, `query_prometheus`,
`query_prometheus_histogram`. Context-saving tools (`get_dashboard_summary`,
JSONPath property getter), `--max-loki-log-limit` (default 100), tool
categories toggled with `--disable-<category>`, `--disable-query` keeps only
discovery tools. Source: [grafana/mcp-grafana](https://github.com/grafana/mcp-grafana).

**Datadog MCP**. Core toolset: `search_datadog_logs`, `analyze_datadog_logs`
(SQL), `search_datadog_spans`, `aggregate_spans`, `get_datadog_trace`,
`get_datadog_metric`, `get_datadog_metric_context` (tags/values),
`search_datadog_metrics`, `search_datadog_events` (deploys), monitors,
incidents. Toolsets via URL param; docs warn large traces are truncated.
Source: [docs.datadoghq.com/mcp_server/tools](https://docs.datadoghq.com/mcp_server/tools/).

**Honeycomb MCP**. Discovery first: `get_workspace_context` (incl. current
time), `get_dataset_columns` (with sample values), `find_columns` (by
intent), `search_semconv`; query: `run_query`, `run_bubbleup` (what
distinguishes a subset); traces: `get_trace` (can zoom to a subtree),
`list_spans`, `get_span_details` (top attribute values). Hard caps (100
datasets/columns per call). Source: [docs.honeycomb.io/integrations/mcp/tools](https://docs.honeycomb.io/integrations/mcp/tools).

**Axiom MCP**: `queryApl` + `getDatasetSchema` — query language plus
schema, nothing more. Source: [axiom.co/docs/console/intelligence/mcp-server](https://axiom.co/docs/console/intelligence/mcp-server), [axiomhq/mcp-server-axiom](https://github.com/axiomhq/mcp-server-axiom).

**logcli / tempo-cli**: `logcli query --since/--from/--to --limit (30)
-o raw|default|jsonl --tail`; one JSON object per line for streaming.
`tempo-cli query api trace-id <endpoint> <id>`. Sources:
[logcli docs](https://grafana.com/docs/loki/v2.2.x/getting-started/logcli),
[tempo-cli docs](https://grafana.com/docs/tempo/latest/operations/tempo_cli/).

**gh**: `--json field1,field2` selects fields, `--jq` (jq bundled, no
install), `--template`; JSON only when asked. Source: [gh help formatting](https://cli.github.com/manual/gh_help_formatting).
**kubectl**: `-o json|yaml|name|wide|jsonpath=…|custom-columns=…`. Source:
[kubectl get](https://v1-34.docs.kubernetes.io/docs/reference/kubectl/generated/kubectl_get/).

**General CLI guidance** ([clig.dev](https://clig.dev/)): data to stdout,
messages to stderr; `--json` and `--plain`; no colour when not a TTY /
`NO_COLOR` / `TERM=dumb`; never require a prompt, `--no-input`; pager only on
a TTY; config precedence flags > env > project file > user file; lead help
with examples.

**Tools for agents** ([Anthropic, Writing tools for agents](https://www.anthropic.com/engineering/writing-tools-for-agents)):
consolidate into fewer high-level tools (e.g. a `search_logs` that returns
only relevant lines with context); offer a concise vs detailed response
format; paginate/filter/truncate with sensible defaults and say how to
narrow when truncated; return meaningful names, not only opaque ids;
actionable error messages that show a correct call.

**Agent-friendly CLI write-ups** (community, not standards): typed exit
codes (usage / auth / not found / conflict / transient), structured errors
with a `retryable` flag and a hint, help as a contract (examples, fields,
exit codes), `--dry-run`/`--yes` for destructive actions. Sources:
[Building agent-friendly CLIs](https://codex.danielvaughan.com/2026/04/28/building-agent-friendly-clis-codex-cli-composable-tool-design/),
[agentic-cli-design](https://skills.cat/skills/tumf/skills/agentic-cli-design),
[nibzard: AI-native](https://nibzard.com/ai-native).

**CLI vs MCP for coding agents**: most 2026 write-ups find CLIs cheaper
(no tool-schema preload; `--help` is progressive disclosure, ~200 tokens per
command vs tens of thousands for a big MCP tool list); MCP wins on
discoverability without a shell, structured schemas and org governance.
Numbers are vendor-reported. Sources:
[deployhq comparison](https://www.deployhq.com/blog/clis-or-mcp-for-coding-agents-practical-comparison),
[apideck: MCP eating your context](https://radar.apideck.com/blog/mcp-server-eating-context-window-cli-alternative),
[manveerc: decision framework](https://manveerc.substack.com/p/mcp-vs-cli-ai-agents).

**What to copy**: discover → query pairs (attribute names → values →
filter), project scoping by env, one "get everything for this id" call,
hard default limits with a truncation flag, compact/concise output, typed
errors, examples in help. **What not to copy**: natural-language search that
needs an LLM in the server; 40+ tools.

---

## 4. Gap analysis

Legend: ✅ works today · 🟡 possible but clumsy · ❌ missing.

| job | today | gap | proposal (section 5) | prio |
|---|---|---|---|---|
| J1 investigate 500 | 🟡 `issues list` → `issues show` (latest_event_id) → `events show --issue` → `trace <trace_id>`; or `telemetry traces -e` → `trace` | 4 calls; `events show` JSON is the raw payload (9 KB pretty); no "logs around time T" without a trace id; errors are text | `issues show` includes a concise `latest_event` (exception, in-app frames, trace_id, request, release); `events show` JSON gets a normalized `exception` block, `payload` only with `--raw`; `logs --around <time> [--window 2m]` | P0 |
| J2 deploy added errors? | 🟡 `issues list --release X` = issues *with events* in X, not *new* in X; no absolute window on Sentry side | need "first seen in release X" / "first seen after T" | `issues list --new` (first_seen in window) and `--first-release X`; Sentry `--from/--to` | P0 |
| J3 top errors | ✅ `issues list --sort count --since 1h`; OTel: `traces -e -S X`, `ops -S X` | OTel exceptions are not grouped (story 0002/04) | none in CLI; depends on story 04 | – |
| J4 fix working? | 🟡 `issues show ID --since 1h` (count, last_seen) | no `--from`; no clear yes/no | `issues show` returns `count` and `last_seen` for the window (exists) + `--from`; optional `issues check ID --since T` with exit 8 if events still arrive | P0 (`--from`), P2 (`check`) |
| J5 resolve with reason | 🟡 `issues resolve ID` | no reason (server hard-codes `"user"`); no `changed` flag | `--reason "<text>"`; JSON `{id, status, changed}`; idempotent | P0 |
| J6 setup project | 🟡 `projects create web` prints DSN; 409 if it exists | not idempotent; no OTLP env; no sentry-cli env; server doesn't report OTLP URL or ingest-auth | `mallard setup <name> [--platform] [--format env|json]` (create-or-get, prints `SENTRY_DSN`, `OTEL_EXPORTER_OTLP_*`, `OTEL_SERVICE_NAME`, sentry-cli vars); `projects create --if-not-exists` | P0 |
| J7 latency regression | 🟡 run `ops --from/--to` twice and diff by hand | no compare; `spans/series` not in CLI | `ops --compare <offset>` (client-side join, adds `p95_ms_before`, `p95_change`); `series spans` | P1 |
| J8 tail logs | ✅ `logs -S X -f` (NDJSON with `--json`) | no `--for 60s` stop condition; Ctrl-C only | `logs -f --for 2m` / `--until-match <text>` | P1 |
| J9 find by id | 🟡 trace id: ✅ `mallard trace`; event id: needs project (+ issue for speed); span id: ❌; request/user id: 🟡 `traces -a k=v`, `logs -a k=v` only if you know the key; Sentry tags: ❌ | agent must know what kind of id it has | `mallard find <id>` (detects 32-hex/16-hex/other, tries trace + event + issue + span, returns typed matches with next commands); `--tag k=v` on issues/events | P0 (typed ids), P1 (tags, free value) |
| J10 health | 🟡 `status` (exit 0 even when down), `telemetry health` (otel only) | wrong exit code; no capabilities | `status` exits 6 when a side is down / not ready; JSON adds capabilities (sql on/off, max_window, retention, flush interval, versions) | P0 |
| J11 discover data | ❌ `metrics list` only; SQL refuses `DESCRIBE`/`information_schema` | no table/column list, no attribute keys/values, no metric label keys | `mallard schema` (tables/columns), `mallard schema attrs --signal spans|logs|metrics [-S svc] [--key k]` | P1 |
| J12 ad-hoc SQL | 🟡 `telemetry sql` (off by default, otel tables only) | Sentry tables not queryable; agent can't tell SQL is off until it fails | status shows `sql: false`; error code `disabled` → exit 7 + hint; Sentry tables (P1, needs ADR) | P0 (signal), P1 (sentry tables) |
| J13 CI sourcemaps/releases | 🟡 via `sentry-cli` against `/sentry`; releases are stubs | agent must work out the sentry-cli env vars | `setup --sentry-cli` prints `SENTRY_URL`, `SENTRY_AUTH_TOKEN`, `SENTRY_ORG`, `SENTRY_PROJECT`; doc it in the agent guide. No own upload command. | P1 |
| J14 wait for data | ❌ agent sleeps and polls | ingest lag (otel flush 5 s; sentry writer) not visible | `--wait <dur>` on `events list`, `traces list`, `logs`, `trace show`, `find`: poll until ≥1 result, exit 8 on timeout | P1 |
| J15 verify instrumentation | ✅ `services`, `metrics list`, `projects list`, `errors stats` | – | covered by `status` + `services` + `schema` | – |
| J16 when did it start | 🟡 `errors stats` (events per bucket); `spans/series`, `logs/series` exist in API, not in CLI | – | `mallard series spans|logs|errors [-S] [-O] [--step]` | P1 |
| J17 metric check | ✅ `telemetry metric NAME -g p99 -b route` | label keys unknown | `schema attrs --signal metrics --metric NAME` | P1 |
| all | – | output contract (below) | §6 | P0 |

Cross-cutting gaps (all jobs): one error shape and typed exit codes; JSON on
stderr for errors; compact JSON when piped; one list envelope with
`truncated` and `next_cursor`; `--fields`; same window flags everywhere;
plain text (no box drawing) when piped; examples in help; an agent guide.

---

## 5. Proposed command surface

### 5.1 Tree shape — options

- **A. Keep `errors …` / `telemetry …`, fix the contract and add flags.** No
  churn; but paths stay long (`mallard errors issues list`) and mix styles
  (`errors issues list` vs `telemetry traces`, `telemetry trace` vs `trace`).
- **B. Flat nouns with `list`/`show` verbs; old paths stay as hidden
  aliases for one release.** Shorter, one style, matches how agents think
  (issues, traces, logs), not the two internal sides. Cost: `mallard` owns
  its clap tree and calls the library functions; this changes ADR 0002
  ("commands show up for free") and needs a new ADR. Story 0002/06 retires
  the standalone `sdl`/`odl` binaries anyway, and 0002/07 splits the CLI
  crate, so the timing fits.

**Preference: B**, done together with 0002/07. The two-side split is an
implementation detail; agents pay for it on every call and in every help
loop. If you want less churn, A plus all of §5.2–§6 still gets ~80% of the value.

### 5.2 Command tree (option B)

```
mallard
  status                         health + ingest + capabilities         (status, errors info, telemetry status/health)
  setup <name>                   create-or-get project; print env vars  NEW
  find <id|value>                what is this id, and everything linked NEW
  projects list | show <p> | create <name> [--if-not-exists]
  issues   list | show <id> | resolve|ignore|unresolve <id>... [--reason]
  events   list | show <event_id>
  traces   list | show <trace_id>          show = spans + errors + logs (today's `mallard trace`)
  logs     [list] [-f]
  services                                  RED per service
  ops                                       RED per operation [--compare]
  series   spans | logs | errors            counts / errors / p95 per bucket  NEW (CLI only)
  metrics  list | show <name>               show = today's `telemetry metric`
  schema   [tables] | attrs                 NEW
  sql "<select>"
  sourcemaps list
  maintenance run
  send-test
  tui
  help agents                               NEW: agent guide (same text as docs/agents.md)
  (serve, check — moved out by story 07, back behind a feature since ADR 0023)
```

Kept aliases (hidden): `errors …`, `sentry …`, `telemetry …`, `otel …`,
`trace <id>` = `traces show <id>`.

### 5.3 Global flags

| flag | env | meaning | prio |
|---|---|---|---|
| `--url`, `--token` | `MALLARD_URL`, `MALLARD_TOKEN` | as today | – |
| `-p/--project` | `MALLARD_PROJECT` | Sentry project; global for all commands that need it | P0 |
| `--format text\|json\|ndjson` | `MALLARD_FORMAT` | `--json` stays as alias of `--format json`. `ndjson` = one item per line for lists and streams | P0 |
| `--fields a,b,c` | – | keep only these top-level keys of each item (list) or of the object (show); unknown field → exit 2 listing valid fields | P0 |
| `--since <dur>` | – | window ending at `--to` (or now) | P0 (Sentry side) |
| `--from`, `--to` | – | RFC 3339, or a duration meaning "ago" (`--from 2h --to 1h`) | P0 (Sentry side) |
| `--limit <n>` | – | max items; every list command has it; defaults stay small (20–100) | P0 |
| `--cursor <c>` | – | continue a list from `next_cursor` | P1 |
| `--raw` | – | include raw payloads (Sentry `payload`, log `resource_attributes`) | P0 (with the concise default, see §6.3) |
| `--wait <dur>` | – | poll until ≥1 result; exit 8 on timeout | P1 |
| `-q/--quiet` | – | no hints/notes on stderr (built as `--quiet` only: `-q` is `--query` on several commands) | P1 |

No config file or profiles: env vars cover agents and CI (one server per
shell). A `mallard config` command has no clear agent job — skip.

### 5.4 Commands with flags and examples

**`mallard status`** (P0)
```sh
mallard status --json
# {"ok":true,"url":"http://…","sentry":{"ok":true,"version":"…","queue_depth":0,…},
#  "otel":{"ok":true,"buffered_rows":0,"flush_errors":0,"dropped_rows":0,…},
#  "capabilities":{"sql":false,"max_window":"31d","retention":{"traces":"7d","logs":"7d","metrics":"30d","errors":"90d"},
#                  "ingest_lag":"5s","ingest_auth":false}}
```
Exit 0 all ok; 6 if a side is unreachable or not ready; 3 if the token is wrong.
Replaces `telemetry health` (kept as alias; health stays tokenless).

**`mallard setup <name>`** (P0)
```sh
mallard setup checkout --platform node            # env lines, ready for >> .env
SENTRY_DSN=http://KEY@mallard:8080/sentry/3
OTEL_EXPORTER_OTLP_ENDPOINT=http://mallard:8080
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
OTEL_SERVICE_NAME=checkout
# OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer <ingest token>   (only printed when the server needs one)
mallard setup checkout --sentry-cli                # adds SENTRY_URL, SENTRY_ORG, SENTRY_PROJECT, SENTRY_AUTH_TOKEN
mallard setup checkout --json                      # {"project":{…},"created":false,"env":{…}}
```
Create-or-get by slug; never fails because the project exists (`created:false`).
`projects create --if-not-exists` gives the same idempotency for scripts.

**`mallard find <x>`** (P0 for ids, P1 for free values)
```sh
mallard find 0af7651916cd43dd8448eb211c80319c --json
# {"query":"0af7…","matches":[
#   {"kind":"trace","trace_id":"0af7…","spans":12,"errors":1,"logs":30,"next":"mallard traces show 0af7…"},
#   {"kind":"event","event_id":"…","issue_id":"…","project":"web","next":"mallard events show …"}]}
mallard find 85fede14250ec6be        # 16 hex: issue id or span id
mallard find req-7f3a --since 1h     # P1: attribute/tag value search (bounded window)
```
32 hex → trace and Sentry event (same format, try both, in parallel).
16 hex → issue and span. Other → value search (P1). Empty → exit 4.

**`mallard issues …`**
```sh
mallard issues list --since 1h --sort count --limit 10 --fields id,title,count,last_seen
mallard issues list --new --release web@1.4.0                 # P0: first seen in the window / in that release
mallard issues list --first-release web@1.4.0                 # P0
mallard issues list --tag url=/checkout --env production      # P1
mallard issues show 85fede14250ec6be --since 1h               # incl. latest_event (concise) + trace_id
mallard issues resolve 85fede14250ec6be --reason "fixed in #123"
```
`show` JSON adds `latest_event: {event_id, timestamp, release, environment,
trace_id, span_id, exception:[{type, value, frames:[top 10 in-app, with
function, file, line, col, context_line]}], request:{method,url}, user}`
(built in the CLI from `latest_event_id`; no server change).
`resolve/ignore/unresolve`: JSON `{"results":[{"id","status","changed"}]}`;
already in that status → `changed:false`, exit 0.

**`mallard events …`**
```sh
mallard events list --issue 85fede14250ec6be --limit 5
mallard events list --trace 0af7… --wait 30s                  # P1: e2e test asserts the error arrived
mallard events list --since 1h --tag user=u-42                # P1 (server: window + tag filters)
mallard events show <event_id>                                # concise; --raw adds payload
```

**`mallard traces …`, `logs`, `services`, `ops`, `series`, `metrics`** (today's otel commands, renamed)
```sh
mallard traces list -S checkout --errors --since 15m --limit 10
mallard traces show 0af7…                       # spans + errors + logs; --fields spans,errors
mallard logs -S checkout -l warn --since 10m --format ndjson --fields time,severity_text,body,trace_id
mallard logs -S checkout -f --for 2m            # P1: stops by itself; NDJSON with --json
mallard logs --around 2026-11-01T10:15:03Z --window 2m -S checkout   # P0 (client-side from/to)
mallard ops -S checkout --since 1h --compare 1d # P1: adds p95_ms_before, p95_change_pct, error_rate_before
mallard series spans -S checkout -O "POST /pay" --since 6h --step 5m  # P1
mallard metrics list -q http
mallard metrics show http.server.request.duration -g p95 -b http.route --since 6h --step 5m
```

**`mallard schema`** (P1)
```sh
mallard schema                       # SQL tables and columns (otel; sentry when SQL covers it)
mallard schema attrs --signal spans -S checkout            # top keys with counts
mallard schema attrs --signal spans --key http.route       # top values
mallard schema attrs --signal metrics --metric http.server.request.duration
```

**`mallard sql`**: as today. With SQL off: exit 7 and the hint
`set otel.query.enable_sql = true`.

**`mallard help agents`** (P0): prints the agent guide: env vars, the
output and exit-code contract, 10 recipes (one per job above), data lag,
limits. Same file in the repo as `docs/agents.md`; a test checks they match.
Every command gets 1–3 examples in `--help` (clap `after_help`).

---

## 6. Output and exit-code contract

### 6.1 Streams and modes

- stdout = data only. stderr = errors, notes, hints.
- `--format text` (default): on a TTY as today; **when piped: plain text,
  no box drawing, no sparklines, no colour** (one row per line, columns
  separated by two spaces or a tab).
- `--format json`: one JSON document. **Pretty on a TTY, compact when
  piped** (measured 45% smaller).
- `--format ndjson`: lists print one item per line, without the envelope
  (truncation goes to the stderr hint). Streams (`-f`) are always NDJSON in
  json/ndjson mode.
- Never prompt, never page. No command needs a TTY except `tui`.
- Untrusted text stays escaped (the existing `json_safe`/`clean` logic).

### 6.2 JSON shapes

Single object (`show`, `status`, `setup`): the object itself.

Lists (all `list` commands, `find`, `services`, `ops`, `metrics list`,
`schema attrs`):
```json
{"items":[…], "count":20, "limit":20, "truncated":true,
 "next_cursor":null, "window":{"from":"2026-11-01T09:00:00Z","to":"2026-11-01T10:00:00Z"}}
```
- `truncated: true` when `count == limit` or the server says so; stderr
  hint: "20 of more; narrow with --since/-S or raise --limit".
- `window` always resolved to absolute UTC times (agents can reuse them).
- Times RFC 3339 UTC; durations `*_ms` floats; ids lowercase hex.
- This replaces today's mixed shapes (`{issues:[…]}`, `{events:[…]}`, bare
  arrays). It is a breaking change of `--json`; do it once, before 1.0, and
  record it in the agent guide.

### 6.3 Concise by default, `--raw` for everything

- `events show` / `issues show.latest_event`: normalized fields; the raw
  Sentry payload only with `--raw`.
- `logs`: `resource_attributes` only with `--raw` (it repeats per record;
  the service name is already a field). Estimate from the sample record:
  50 logs 29 KB → about 18 KB; with `--fields time,severity_text,body,trace_id`
  about 6 KB.
- `traces show`: span `attributes` included (they are the point); resource
  attributes once per service in a `resources` map instead of per span.
- `--fields` works on top of this.

### 6.4 Errors

With `--format json|ndjson`, errors go to **stderr as one JSON line**:
```json
{"error":{"code":"not_found","message":"issue 85fede14250ec6be not found in project web",
          "status":404,"retryable":false,"hint":"mallard issues list --status all --since 30d"}}
```
In text mode: one line `error: <message>` plus `hint: …` (no `Caused by`
chain unless `MALLARD_DEBUG=1`).

### 6.5 Exit codes

| code | class | from |
|---:|---|---|
| 0 | success (also empty results) | – |
| 1 | other / internal error | 500, decode errors |
| 2 | usage: bad flag, bad id, bad window, unknown field | clap, client checks, HTTP 400 |
| 3 | auth | 401, 403 `unauthorized` |
| 4 | not found | 404, `find` with no match |
| 5 | conflict | 409 (e.g. slug taken without `--if-not-exists`) |
| 6 | unavailable — **retryable** | connect error, timeout, 429, 503 `busy`, 504 `timeout`, `status` with a side down |
| 7 | feature disabled on the server | 403 `disabled` (SQL), 501 (sourcemaps not configured) |
| 8 | condition not met | `--wait` timed out, `issues check` still seeing events |

Broken pipe stays exit 0 (as today).

---

## 7. Missing server endpoints

The architecture rule "the server adds no API of its own; correlation lives
in `mallard-client`" is kept: everything below is on one side's API, or done
client-side.

| # | side | endpoint / change | for | prio |
|---|---|---|---|---|
| S1 | sentry | `from`/`to` on `issues`, `issues/{id}`, `stats`, `events` (today `since` only) | J2, J4, all windows | P0 |
| S2 | sentry | `issues?first_release=X` and `issues?new=true` (first_seen ≥ window start) | J2 | P0 |
| S3 | sentry | `PATCH issues/{id}` accepts `reason` (column `issue_activity.reason` exists, hard-coded `"user"` today); response says if it changed | J5 | P0 |
| S4 | sentry | `/v1/info` adds `public_url`, `retention_days` | J6, J10 | P0 |
| S5 | otel | `/api/v1/stats` (or new `/api/v1/info`) adds capabilities: `enable_sql`, `max_window`, retention per signal, `flush_interval`, `ingest_auth_required`, OTLP HTTP/gRPC URLs | J6, J10, J12 | P0 |
| S6 | sentry | one error shape `{"error":"<code>","message":…}` like the otel side (keep `detail` for sentry-cli routes) | contract | P0 |
| S7 | sentry | `GET /v1/events/{event_id}` across projects (like `/v1/events?trace_id=`) | J9 `find` | P0 |
| S8 | otel | span by id: `GET /api/v1/spans/{span_id}` (or `traces?span_id=`) | J9 `find` | P1 |
| S9 | sentry | `issues?tag=k:v` (repeat), `events?since/from/to&tag=&release=&environment=&q=` (events.tags is a MAP column) | J1, J9 | P1 |
| S10 | sentry | cursor on `issues` (today max 500, no paging) | lists | P1 |
| S11 | otel | cursor (`before`/`after` time + id tie-breaker) on `logs` and `traces` | lists | P1 |
| S12 | otel | `GET /api/v1/attributes?signal=spans|logs|metrics&service=&key=&metric=` → keys with counts, or top values for one key (sampled, bounded window) | J11, J17 | P1 |
| S13 | otel | `GET /api/v1/schema` → tables and columns (SQL refuses DESCRIBE / information_schema today) | J11, J12 | P1 |
| S14 | sentry | `GET /v1/projects/{p}/releases` derived from events: version, first/last seen, events, issues, new issues | J2 | P1 |
| S15 | sentry | read-only SQL over sentry tables (`events`, `issues`, `issue_activity`) on the sentry side's own endpoint (keeps "each side owns its schema"); needs an ADR next to otel ADR 0005 | J12 | P1 |
| S16 | otel | value search across attributes (`/api/v1/search?value=&since=` with a small max window) | J9 free values | P2 |

Client-side only (no endpoint): `setup`, `find` orchestration, `issues show`
latest-event enrichment, `logs --around`, `ops --compare`, `--wait`,
`--fields`, envelope, exit codes, `series` (uses existing `spans/series`,
`logs/series`, `stats`).

---

## 8. MCP recommendation

**P2. Not now.** The main users (Claude Code, Codex, Cursor agents, CI) have
a shell. A CLI with a stable JSON contract and short help costs fewer
tokens than loading an MCP tool list, and it works in CI where MCP does not.
All P0/P1 work is needed by an MCP server anyway (it would call the same
client functions).

Add `mallard mcp` (stdio, e.g. with the `rmcp` crate) when someone needs
mallard from a client without a shell (Claude Desktop / claude.ai, IDE
chat). Then keep it small and consolidated, read-only by default:

| tool | maps to |
|---|---|
| `mallard_status` | `status` (incl. capabilities, current server time) |
| `mallard_find` | `find` — any id or value |
| `mallard_search_issues` | `issues list` (since/from/to, new, release, tag, sort, limit) |
| `mallard_get_issue` | `issues show` with latest event |
| `mallard_get_trace` | `traces show` (spans + errors + logs), `response_format: concise|detailed` |
| `mallard_search_traces` | `traces list` |
| `mallard_search_logs` | `logs` (no follow) |
| `mallard_services` | `services` / `ops` (with `operation_level` flag and compare) |
| `mallard_metrics` | `metrics list` / `metrics show` |
| `mallard_schema` | `schema`, `schema attrs` |
| `mallard_sql` | `sql` — only listed when the server has SQL on |
| `mallard_update_issue` | resolve/ignore/unresolve with reason — only with `--allow-writes` |

Scope by env (`MALLARD_PROJECT`), like Sentry's project-scoped URL. No
natural-language query translation (needs an LLM in the server; the agent
already is one).

---

## 9. Priorities

### P0 — must have for agents

1. **Exit codes** per class (§6.5) and **`status` non-zero** when a side is down.
2. **JSON errors on stderr** with `code`, `message`, `retryable`, `hint`
   (§6.4); one error shape on the Sentry API (S6).
3. **Output modes**: `--format text|json|ndjson` + `MALLARD_FORMAT`;
   compact JSON when piped; plain text (no boxes/sparklines) when piped.
4. **One list envelope** with `items`, `truncated`, `next_cursor`, resolved
   `window` (§6.2).
5. **`--fields`** on every command; concise defaults with **`--raw`** for
   raw payloads / resource attributes (§6.3).
6. **Same window flags everywhere**: `--since/--from/--to` on the Sentry side
   (S1), relative `--from 2h`.
7. **Release/new filters**: `issues list --new`, `--first-release` (S2).
8. **`issues resolve|ignore|unresolve --reason`**, idempotent, `changed` flag (S3).
9. **`issues show` with concise `latest_event`** (exception, in-app frames,
   trace_id) and normalized `events show`.
10. **`mallard setup <name>`** (create-or-get, DSN + OTLP env) and
    `projects create --if-not-exists` (S4, S5).
11. **`mallard find <id>`** for trace/event/issue ids (S7).
12. **`logs --around <time>`**.
13. **Capabilities in `status`** (SQL on/off, max window, retention, ingest
    lag) (S5); SQL-disabled → exit 7 with a hint.
14. **Agent guide**: `mallard help agents` = `docs/agents.md`; examples in
    every `--help`.
15. **Command tree** (option B, flat nouns) — decide with story 0002/07;
    if not, keep A and do 1–14 on the current tree.

### P1

`schema` + `schema attrs` (S12, S13) · `--wait` · `logs -f --for` ·
`ops --compare` · `series spans|logs|errors` · `--tag` filters and event
windows (S9) · span lookup in `find` (S8) · cursors (S10, S11) ·
`releases list` (S14) · SQL over Sentry tables (S15, ADR) ·
`setup --sentry-cli` · `--quiet`.

### P2

`mallard mcp` (§8) · free-value `find` (S16) · `issues check` · `--jq`
(bundled jaq) · machine-readable command catalog (`mallard help --json`) ·
`/llms.txt` served by the server · resolve-in-next-release / ignore-for-duration.

### Not proposed (no clear agent job)

Config file/profiles (env vars suffice), interactive prompts, own sourcemap
upload (sentry-cli works), natural-language search, dashboards/alerts.

---

## 10. Side findings (for the backlog, not verified in depth)

- `mallard status` exits 0 when both sides are unreachable.
- Errors print an anyhow `Caused by:` chain of 4–5 lines for a refused
  connection; noisy for humans and agents.
- `mallard errors issues list --since` and other Sentry-side windows have no
  client-side `value_parser` (the otel side validates with `check_window`).
- `events show` text says "Breadcrumbs (newest first)" but showed ascending
  times with `mallard-sentry-loadgen` data. Checked with `@sentry/node` 11: SDKs send
  oldest first, so the label is right and the loadgen had the wrong order
  (fixed).
- `send-test` default message still says "hello from the sdl CLI".
- Sentry release endpoints are stubs: `sentry-cli releases new/deploys` look
  successful but store nothing; agents may assume releases exist.
