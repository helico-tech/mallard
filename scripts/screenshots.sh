#!/usr/bin/env bash
# Regenerate the README screenshots (docs/screenshots/*.png).
#
#   scripts/screenshots.sh
#
# Starts a throwaway mallard server on localhost:8080 (data in a temp dir),
# fills it with scripts/demo-seed.py, renders the real TUI into SVG via the
# ignored `screenshots` test, and converts the SVGs to PNG with rsvg-convert
# (brew install librsvg). Ports 8080, 4317 and 4318 must be free.
set -euo pipefail
cd "$(dirname "$0")/.."

command -v rsvg-convert >/dev/null || { echo "needs rsvg-convert (brew install librsvg)" >&2; exit 1; }
cargo build -q -p mallard-server -p mallard

data=$(mktemp -d)
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; rm -rf "$data"' EXIT

export MALLARD_URL=http://localhost:8080 MALLARD_TOKEN=dev-token
bin=target/debug/mallard-server
(cd "$data" && MALLARD_SERVER__API_TOKENS='["dev-token"]' MALLARD_SERVER__PUBLIC_URL=$MALLARD_URL \
  MALLARD_OTEL__INGEST__FLUSH_INTERVAL=1s exec "$OLDPWD"/$bin > server.log 2>&1) &
pid=$!
for _ in $(seq 1 60); do curl -fsS "$MALLARD_URL/readyz" >/dev/null 2>&1 && break; sleep 1; done

scripts/demo-seed.py
sleep 3  # let the OTLP buffer flush

out=docs/screenshots
MALLARD_SCREENSHOT_DIR="$PWD/$out" cargo test -q -p mallard -- --ignored screenshots
for svg in "$out"/*.svg; do
  rsvg-convert --zoom 2 "$svg" -o "${svg%.svg}.png"
  rm "$svg"
done
ls -la "$out"
