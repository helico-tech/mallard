# 0035 — Releases and deploys from sentry-cli are stored

## Status
accepted (by the owner, 2026-11-03), with the three choices below as proposed. For 0.2.0, [issue 0060](../../work/issues/0060-store-sentry-cli-releases.md).

## Context
`POST`, `PUT` and `deploys` under `/api/0/.../releases/` are stubs: they answer as if something was stored, and nothing is.
`GET /sentry/v1/projects/{p}/releases` derives its list from the `release` of error events. So a release without errors is not seen,
and no deploy is. That is a success answer for work that did not happen, documented, but still misleading.

What `sentry-cli` 3.8.2 sends, captured with a logging server on 2026-11-03 (2.58.6 was not captured; check it when building this):

| Command | Request | Body |
|---|---|---|
| `releases -p web -p api new v3` | `POST /organizations/org/releases/` | `version`, `projects: ["web","api"]`, `dateStarted` |
| `releases new v2`, no `-p` | none: the CLI refuses | |
| `releases finalize v1` | `PUT /organizations/org/releases/v1/` | `dateReleased`, **no project** |
| `deploys -r v1 new -e production -n d1` | `POST /organizations/org/releases/v1/deploys/` | `environment`, `name`, `url`, `dateStarted`, `dateFinished`, **no project** |
| `releases list` | `GET /organizations/org/releases/` | no such route today |
| `deploys -r v1 list` | `GET /organizations/org/releases/v1/deploys/` | no such route today |

So a version belongs to the organization, and the projects are named only when it is created. Issue 0060 worried that the org-level
endpoint names no project: for `new` it does.

A new table needs no migration tool. `db::migrate` runs every `CREATE TABLE IF NOT EXISTS` at each start, and tries again on a conflict
between replicas. A 0.1.0 lake gets the tables on its first start. An older binary going back ignores them.

## Decision
**Three append-only tables,** in the pattern of `schema.rs`, where the latest row wins and two processes may each write a row:

- `releases`: `project_id, version, date_created`. One row per project named in `new`. Grouped when read.
- `release_activity`: `version, changed_at, date_released`. The latest row per version wins, as in `issue_activity`.
- `deploys`: `version, environment, name, url, date_started, date_finished, created_at`.

No `project_id` on the last two, because the requests have none: a deploy of a version counts for each project of that version.

Writes go through two new `WriteCmd`s on the writer thread, under the commit lock (ADR 0030). The three tables join `COMPACTED_TABLES`.
Retention leaves them, as it leaves `issues`: they are small, and a release outlives its events. `SCHEMA_VERSION` becomes 2.

**Endpoints:** `POST`, `PUT` and `deploys` store for real. `GET` on a release answers what is stored, 404 if nothing is known of it. New:
`GET` of the release list and the deploy list.

**The merged list.** `GET /sentry/v1/projects/{p}/releases` and `mallard releases list` show the union of stored releases and the
releases named by events:

- A release is in the window if it has an event, was created, was finalized, or was deployed in it.
- New fields: `date_created`, `date_released`, `deploys` (per environment, the last one).
- `first_seen` and `last_seen` still mean events. For a release with no events they are `null`, not the creation time.
- Newest first, by the latest of those dates.

**Three choices the owner made:**

1. A `new` naming an unknown project: **answer 400 and name it**, as a real error. Against: a build that passes today starts to log an error.
   Accepting it silently would hide a typo, so that is not offered. Check `@sentry/vite-plugin` with the e2e suite, since it creates releases too.
2. `first_seen` `null` for a release with no events: **yes**. Against: API clients of 0.1.0 must allow `null`, though only on rows they never saw
   before. Using the creation time would keep the type, and say "seen" of something no event has named.
3. A deploy of an unknown version: **store it.** A deploy may come before the first event, and checking events would mean a scan on each
   deploy. Against: a typo leaves a row that no list shows.

**Not in this change:** `set-commits`, `ref` and `url` of a release, `releases delete` and `archive`, `propose-version`, release health, and
markers on the Web UI's charts and the TUI's. Markers are the visible value, and come next, once the data is there.

## Consequences
- `sentry-cli releases list` and `deploys list` work, and a release with no errors can be seen.
- The stub text in `docs/sentry/compatibility.md`, `docs/agents.md` and `docs/cli.md` goes, and the OpenAPI types change.
- The release list reads three more tables, joined with `events` in the window. Measure it: it is a per-window scan today, with a limit of 500.
- `GET` on an unknown release turns from a made-up answer into a 404, and whatever relied on it will show. The e2e suite covers
  `sentry-cli` 3.x and 2.x and the Vite plugin.
- Bundler plugins that send a release on each build add rows, a few per build. Small next to events, and compacted.
- Replicas: each writes through its own writer. A duplicate row is grouped away when read, so no lease is needed.
