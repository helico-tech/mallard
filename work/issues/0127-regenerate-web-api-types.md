---
status: done
type: debt
created: 2026-11-01
---
# Regenerate the Web UI's API types after the OpenAPI descriptions changed

## Problem
Issue 0122 changed descriptions in `docs/otel/openapi.json` and
`docs/sentry/openapi.json`. `crates/mallard-web/ui/src/api/*.gen.ts` copy those
descriptions into comments, and that work had to leave `crates/mallard-web` alone. So
the CI step "API types are current" (`web` job) fails until they are regenerated.

## How to reproduce
In `crates/mallard-web/ui`: `npm run api-types`, then `git diff --exit-code src/api`.

## Notes
Only comments differ, no types: 3 lines in `sentry.gen.ts`, 4 in `otel.gen.ts`. The fix
is to run the command above and commit the result. Do this before merging the branch
that carries issue 0122.

Done on 2026-11-01 with `npm run api-types`, when the branch merged.
