# syntax=docker/dockerfile:1.7
#
# sentry-ducklake container image.
#
# Build from the repository root:
#   docker build -f deploy/docker/Dockerfile -t sentry-ducklake .
#
# Stage 1 compiles the server (DuckDB's C++ is compiled from source by the
# duckdb crate's `bundled` feature, so the first build takes a few minutes;
# BuildKit cache mounts keep the cargo registry and target dir between builds).
# Stage 2 is a small glibc image (DuckDB extensions are glibc builds) with all
# DuckDB extensions pre-installed, so the server needs no internet access.

ARG RUST_VERSION=1.99
ARG DEBIAN_RELEASE=trixie

# ---------------------------------------------------------------- build stage
FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS build

RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential cmake pkg-config \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# The target dir is a cache mount, so copy the binary out of it.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p sdl-server --bin sentry-ducklake \
 && install -D -m 0755 target/release/sentry-ducklake /out/sentry-ducklake

# -------------------------------------------------------------- runtime stage
FROM debian:${DEBIAN_RELEASE}-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 sdl \
 && useradd --system --uid 10001 --gid sdl --home-dir /data --shell /usr/sbin/nologin sdl \
 && install -d -o sdl -g sdl -m 0750 /data

COPY --from=build /out/sentry-ducklake /usr/local/bin/sentry-ducklake

# Download every DuckDB extension any configuration may need (ducklake,
# httpfs, aws, azure, postgres, ...). Done in this stage so the extensions
# match the runtime platform. Owned by root: the server only reads them.
# The target directory comes from SDL_EXTENSION_DIR.
ENV SDL_EXTENSION_DIR=/opt/duckdb/extensions
RUN sentry-ducklake install-extensions \
 && chmod -R a+rX "$SDL_EXTENSION_DIR"

# /data is the working directory: the default DuckDB-file catalog lives here
# and DuckDB spills temporary files to ./.tmp. Mount a volume on it.
WORKDIR /data
ENV SDL_LISTEN=0.0.0.0:8000 \
    SDL_CATALOG=/data/catalog.ducklake \
    SDL_LOG_JSON=true \
    HOME=/data

USER 10001:10001
EXPOSE 8000

# No curl in the image: bash's /dev/tcp is enough for a liveness probe.
HEALTHCHECK --interval=15s --timeout=5s --start-period=30s --retries=3 \
  CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/${SDL_LISTEN##*:} && printf 'GET /healthz HTTP/1.0\\r\\nHost: localhost\\r\\n\\r\\n' >&3 && head -n1 <&3 | grep -q ' 200 '"]

ENTRYPOINT ["/usr/local/bin/sentry-ducklake"]
CMD ["serve"]
