---
status: todo
type: idea
created: 2026-11-03
---
# Deploy markers on the charts

## Problem
Releases and deploys are stored now ([ADR 0035](../../docs/adr/0035-stored-releases-and-deploys.md)), and `mallard releases list` shows them, but nothing
draws them. A line on the error and telemetry charts at each deploy would show what changed when, which is the reason to store them.

## Notes
Left out of issue 0060 on purpose. To decide: which charts, in the Web UI and the TUI, and how a deploy finds its chart: a deploy goes by version, and a
telemetry chart goes by service, so it needs the app concept (ADR 0027) to link the two.
