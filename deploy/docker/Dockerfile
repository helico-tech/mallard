# syntax=docker/dockerfile:1.7
#
# mallard container image.
#
# Build from the repository root:
#
#   docker build -f deploy/docker/Dockerfile -t mallard .
#
# Stage 0 builds the Web UI, which goes into the `mallard` binary. Stage 1
# compiles `mallard-server` and `mallard` (CLI, TUI, Web UI; DuckDB's C++ is compiled
# from source, so the first build takes a few minutes; BuildKit cache mounts
# keep the cargo registry and target dir between builds). Stage 2 is a small
# glibc image with all DuckDB extensions pre-installed, so the server needs
# no internet access.

ARG RUST_VERSION=1.99
ARG DEBIAN_RELEASE=trixie

# ------------------------------------------------------------- web UI stage
FROM node:24-slim AS web
WORKDIR /src/crates/mallard-web/ui
COPY crates/mallard-web/ui/package.json crates/mallard-web/ui/package-lock.json ./
RUN npm ci
COPY crates/mallard-web/ui/ ./
RUN npm run build

# ---------------------------------------------------------------- build stage
FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS build

RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential cmake pkg-config \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .
COPY --from=web /src/crates/mallard-web/ui/dist crates/mallard-web/ui/dist

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p mallard-server -p mallard \
 && install -D -m 0755 target/release/mallard-server /out/mallard-server \
 && install -D -m 0755 target/release/mallard /out/mallard

# -------------------------------------------------------------- runtime stage
FROM debian:${DEBIAN_RELEASE}-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 mallard \
 && useradd --system --uid 10001 --gid mallard --home-dir /data --shell /usr/sbin/nologin mallard \
 && install -d -o mallard -g mallard -m 0750 /data

# The CLI ships too, for `docker exec … mallard status` and debugging. The Web UI is
# in it: `--entrypoint mallard … web --addr 0.0.0.0:8088`, with MALLARD_URL.
COPY --from=build /out/mallard-server /out/mallard /usr/local/bin/

# Every DuckDB extension any config may need (ducklake, httpfs, aws, azure,
# postgres, ...), installed for the runtime platform. Read-only for the server.
RUN mallard-server install-extensions --dir /opt/duckdb/extensions \
 && chmod -R a+rX /opt/duckdb/extensions

# /data is the working directory: with no config the DuckDB-file catalog and
# Parquet files go to /data/data/, and DuckDB spills to /data/.tmp. Mount a
# volume on it. Only image facts are set through env here (env overrides the
# config file); storage and logging come from the config file.
WORKDIR /data
ENV MALLARD_DUCKDB__EXTENSION_DIRECTORY=/opt/duckdb/extensions \
    MALLARD_DUCKDB__AUTOINSTALL_EXTENSIONS=false \
    MALLARD_DUCKDB__TEMP_DIRECTORY=/data/.tmp \
    HOME=/data

USER 10001:10001
# 8080: Sentry ingest + APIs + OTLP/HTTP, 4318: OTLP/HTTP, 4317: OTLP/gRPC
EXPOSE 8080 4318 4317

# No curl in the image: bash's /dev/tcp is enough for a liveness probe.
HEALTHCHECK --interval=15s --timeout=5s --start-period=30s --retries=3 \
  CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8080 && printf 'GET /healthz HTTP/1.0\\r\\nHost: localhost\\r\\n\\r\\n' >&3 && head -n1 <&3 | grep -q ' 200 '"]

ENTRYPOINT ["/usr/local/bin/mallard-server"]
CMD ["serve"]
