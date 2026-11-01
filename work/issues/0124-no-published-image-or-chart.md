---
status: todo
type: debt
created: 2026-11-01
---
# Nothing publishes the image, the chart or release binaries

## Problem
`deploy/helm/mallard/values.yaml` defaults to `ghcr.io/helico-tech/mallard` with the
chart's `appVersion`, but `.github/workflows/` has only `ci.yml`, which builds the
image with `push: false`. The old `release.yml` of the telemetry repo did not come
along. So a plain `helm install` cannot pull its image, and there are no release
binaries. The docs now say to build and push the image and set `image.repository`.

## Notes
Decide when mallard gets its first release: a release workflow (multi-arch image,
binaries, packaged chart), or a chart default that makes the need to set the image
explicit. Found during issue 0016.
