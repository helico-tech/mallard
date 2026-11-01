---
status: todo
type: debt
created: 2026-11-01
---
# Nothing publishes the image or release binaries

## Problem
`.github/workflows/` has only `ci.yml`, which builds the image with `push: false`. The
old `release.yml` of the telemetry repo did not come along. So there is no image to pull
and there are no release binaries. The docs say to build the image yourself.

## Notes
Decide when mallard gets its first release: a release workflow with a multi-arch image
and binaries. The image is the only deployment artifact to publish (ADR 0026). Found
during issue 0016.
