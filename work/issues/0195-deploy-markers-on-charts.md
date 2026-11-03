---
status: done
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

## Done (2026-11-03)
The owner chose all charts, not only the error charts. The link was already there: an app is a project and a service with one slug (ADR 0027).

- API: `GET /sentry/v1/deploys`, with or without `project`.
- Web UI: Overview's two charts and the metric chart (Kumo's `markers`), the events bars and log volume, the issue's trend.
- TUI: Overview's two charts and the metric chart (a vertical line), log volume and the events chart (`┊` through the empty cells).
- Not marked: the tiny sparklines in the issue list's rows, where a line cannot be read.
- If the deploys cannot be read the chart stays and says so: a note in the Web UI, a flash or title text in the TUI.

**Checked:** Web UI by screenshot too, since the line charts draw on a canvas that a browser test cannot look into. The TUI was checked by render tests only, not by eye.

**Limits:** all environments are marked alike. With many deploys in a window the lines and labels crowd; the list has at most 200. A telemetry service whose slug matches no project has no markers.
