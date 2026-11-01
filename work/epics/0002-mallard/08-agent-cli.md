---
status: todo
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
Spec: [docs/specs/agent-cli.md](../../../docs/specs/agent-cli.md) (researched 2026-11-01). Implement P0 after story 07 (split), on the flattened command tree (spec item 15).

## Progress log

## Follow-ups
