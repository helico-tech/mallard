#!/bin/sh
# Container entrypoint for otel-ducklake.
# Uses /etc/otel-ducklake/config.toml (the image ships a default one with a
# DuckDB-file catalog and Parquet files under /data) unless ODL_CONFIG or
# -c/--config is given. ODL_<SECTION>__<KEY> environment variables override
# individual keys of whichever file is used.
set -e
has_config=
for a in "$@"; do
  case "$a" in
    -c|-c*|--config|--config=*) has_config=1 ;;
  esac
done
if [ -z "${ODL_CONFIG:-}" ] && [ -z "$has_config" ] && [ -f /etc/otel-ducklake/config.toml ]; then
  export ODL_CONFIG=/etc/otel-ducklake/config.toml
fi
exec /usr/local/bin/otel-ducklake "$@"
