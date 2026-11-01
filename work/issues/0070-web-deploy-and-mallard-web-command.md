---
status: done
type: idea
created: 2026-11-01
---
# 0070 — Web UI in compose

## Problem
`mallard web` ships in the Docker image, but the compose file does not start it.

## Notes
- The owner chose `mallard web` over a separate `mallard-web` binary on 2026-11-01.
- Helm and ingress are out of scope: the chart is being removed, we only offer
  Docker containers for now (owner's decision, passed on by the mallard-core
  session on 2026-11-01; ADR 0026).
- The `web` service runs in sign-in mode, because `mallard web` refuses to listen
  on 0.0.0.0 with a token. It is published on 127.0.0.1 only: off this machine it
  needs TLS in front, because the session cookie is `Secure`.
- The service has its own `build` block, the same as `mallard`: a service with only
  `image` would make compose try to pull `mallard:dev` before building it.
