# syntax=docker/dockerfile:1.7-labs
#
# obs-ducklake container image.
#
# The server depends on the sentry-ducklake and otel-ducklake repos by path,
# so the build needs them as extra build contexts. From this repo's root:
#
#   docker build -f deploy/docker/Dockerfile \
#     --build-context sentry-ducklake=../sentry-ducklake \
#     --build-context otel-ducklake=../otel-ducklake \
#     -t obs-ducklake .
#
# (docker compose passes them for you, see deploy/compose.)
# Stage 1 compiles the server and the `obs` CLI (DuckDB's C++ is compiled
# from source, so the first build takes a few minutes; BuildKit cache mounts
# keep the cargo registry and target dir between builds). Stage 2 is a small
# glibc image with all DuckDB extensions pre-installed, so the server needs
# no internet access.

ARG RUST_VERSION=1.99
ARG DEBIAN_RELEASE=trixie

# ---------------------------------------------------------------- build stage
FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS build

RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential cmake pkg-config \
 && rm -rf /var/lib/apt/lists/*

# Same layout as on disk: path deps are ../sentry-ducklake and ../otel-ducklake.
WORKDIR /src/obs-ducklake
COPY --from=sentry-ducklake --exclude=target --exclude=.git . /src/sentry-ducklake
COPY --from=otel-ducklake --exclude=target --exclude=.git . /src/otel-ducklake
COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/obs-ducklake/target,sharing=locked \
    cargo build --release --locked -p obs-server -p obs-cli \
 && install -D -m 0755 target/release/obs-ducklake /out/obs-ducklake \
 && install -D -m 0755 target/release/obs /out/obs

# -------------------------------------------------------------- runtime stage
FROM debian:${DEBIAN_RELEASE}-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 obs \
 && useradd --system --uid 10001 --gid obs --home-dir /data --shell /usr/sbin/nologin obs \
 && install -d -o obs -g obs -m 0750 /data

COPY --from=build /out/obs-ducklake /out/obs /usr/local/bin/

# Every DuckDB extension any config may need (ducklake, httpfs, aws, azure,
# postgres, ...), installed for the runtime platform. Read-only for the server.
RUN obs-ducklake install-extensions --dir /opt/duckdb/extensions \
 && chmod -R a+rX /opt/duckdb/extensions

# /data is the working directory: with no config the DuckDB-file catalog and
# Parquet files go to /data/data/, and DuckDB spills to /data/.tmp. Mount a
# volume on it. Only image facts are set through env here (env overrides the
# config file); storage and logging come from the config file.
WORKDIR /data
ENV OBS_DUCKDB__EXTENSION_DIRECTORY=/opt/duckdb/extensions \
    OBS_DUCKDB__AUTOINSTALL_EXTENSIONS=false \
    OBS_DUCKDB__TEMP_DIRECTORY=/data/.tmp \
    HOME=/data

USER 10001:10001
# 8080: Sentry ingest + APIs + OTLP/HTTP, 4318: OTLP/HTTP, 4317: OTLP/gRPC
EXPOSE 8080 4318 4317

# No curl in the image: bash's /dev/tcp is enough for a liveness probe.
HEALTHCHECK --interval=15s --timeout=5s --start-period=30s --retries=3 \
  CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8080 && printf 'GET /healthz HTTP/1.0\\r\\nHost: localhost\\r\\n\\r\\n' >&3 && head -n1 <&3 | grep -q ' 200 '"]

ENTRYPOINT ["/usr/local/bin/obs-ducklake"]
CMD ["serve"]
