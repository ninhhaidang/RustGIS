# syntax=docker/dockerfile:1
#
# The RustGIS command line (rustgis-cli), built from this checkout, on a slim Debian (see
# docs/installation.md › The command line in Docker). Build it from the repository root:
#
#   docker build -f packaging/docker/cli.Dockerfile -t rustgis-cli .
#   docker run --rm -v "$PWD:/data" rustgis-cli info data.geojson
#
# Unlike the web app's image, this is native code, so it is built on the platform the image is
# for. .github/workflows/docker.yml builds each platform on a runner of its own.
# Dependabot keeps the base images current.

FROM rust:1.100.0-slim@sha256:6f14b2b4b278ec0d50aa1d69541c304ce0f95e0f6268487aa9924af1c2e9aebe AS build

# crates/render/build.rs uses curl to fetch the Natural Earth sample data it embeds.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# The cargo registry and the target directory are cache mounts, so a rebuild after a change redoes
# only what changed. The binary is copied out of them.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p rustgis-cli \
    && cp target/release/rustgis-cli /usr/local/bin/rustgis-cli

# The Debian release the rust image is built on, so the binary finds the C library it was linked
# against.
FROM debian:13.7-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

# ca-certificates for HTTPS (web tiles, remote data, the AI Assistant's providers).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 1000 rustgis \
    && install -d -o rustgis -g rustgis /data

LABEL org.opencontainers.image.title="RustGIS command line" \
      org.opencontainers.image.description="The RustGIS command line: run commands and geoprocessing, render maps, inspect data" \
      org.opencontainers.image.url="https://rustgis.app" \
      org.opencontainers.image.source="https://github.com/opengeos/RustGIS" \
      org.opencontainers.image.licenses="Apache-2.0"

COPY --from=build /usr/local/bin/rustgis-cli /usr/local/bin/rustgis-cli
# The licence and the notices of the dependencies compiled into it, where the .deb puts them.
COPY LICENSE THIRD-PARTY-LICENSES.txt /usr/share/doc/rustgis/

# Non-root; its settings, plugins and caches go to /home/rustgis. Mount the data to work on at
# /data, the working directory, so relative paths resolve there.
USER rustgis
WORKDIR /data
ENTRYPOINT ["rustgis-cli"]
