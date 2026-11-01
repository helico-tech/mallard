---
status: todo
type: idea
created: 2026-11-01
---
# 0070 — Web UI in compose and Helm

## Problem
`mallard web` ships in the Docker image, but neither the compose file nor the Helm
chart starts it.

## Notes
- Off this machine it needs TLS in front, so a chart needs an ingress with TLS.
- The owner chose `mallard web` over a separate `mallard-web` binary on 2026-11-01.
