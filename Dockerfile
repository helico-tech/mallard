# syntax=docker/dockerfile:1.7
#
# otel-ducklake container image: the collector (`otel-ducklake`), the CLI
# (`odl`) and the load generator (`odl-loadgen`).
#
#   docker build -t otel-ducklake .
#   docker run -p 4317:4317 -p 4318:4318 -p 8080:8080 -v otel-data:/data otel-ducklake
#
# DuckDB extensions (ducklake, httpfs, aws, azure, postgres, sqlite, ...) are
# installed at build time for the target platform, so the image runs without
# internet access.

ARG RUST_VERSION=1.99
ARG DEBIAN=trixie

FROM rust:${RUST_VERSION}-${DEBIAN} AS build
WORKDIR /src
COPY . .
# Cache the registry and target dir between builds; copy the binaries out of
# the cache mount in the same step.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p odl-server -p odl-cli -p odl-loadgen \
 && mkdir -p /out \
 && cp target/release/otel-ducklake target/release/odl target/release/odl-loadgen /out/

FROM debian:${DEBIAN}-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --gid 10001 odl \
 && useradd --uid 10001 --gid 10001 --home-dir /data --no-create-home --shell /usr/sbin/nologin odl \
 && mkdir -p /data /etc/otel-ducklake /opt/duckdb/extensions \
 && chown 10001:10001 /data
COPY --from=build /out/ /usr/local/bin/
COPY deploy/docker/entrypoint.sh /usr/local/bin/entrypoint.sh
COPY deploy/docker/config.toml /etc/otel-ducklake/config.toml
ENV ODL_DUCKDB__EXTENSION_DIRECTORY=/opt/duckdb/extensions
# Extensions to bake in (json and parquet are compiled into the binary).
# For a smaller image, keep only what your storage needs, e.g.
#   --build-arg DUCKDB_EXTENSIONS="ducklake httpfs postgres"
ARG DUCKDB_EXTENSIONS="ducklake httpfs aws azure postgres sqlite icu"
# Runs on the target platform (natively or under emulation), so the
# downloaded extensions match the image architecture.
RUN otel-ducklake install-extensions ${DUCKDB_EXTENSIONS} \
 && chmod -R a+rX /opt/duckdb/extensions
ENV ODL_DUCKDB__AUTOINSTALL_EXTENSIONS=false \
    HOME=/data
USER 10001:10001
WORKDIR /data
VOLUME ["/data"]
EXPOSE 4317 4318 8080
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["serve"]
