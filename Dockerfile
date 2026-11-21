# syntax=docker/dockerfile:1
#
# The RustGIS web app, built from this checkout and served by nginx (see docs/installation.md ›
# Self-host the web app with Docker):
#
#   docker build -t rustgis .
#   docker run --rm -p 8080:8080 rustgis     # http://localhost:8080
#
# It builds as .github/workflows/web-deploy.yml does, with three differences: no analytics tag
# (only the official deploy adds one), relative addresses (as the docs preview's build) so it can
# also be served under a path, and --locked, so it uses exactly Cargo.lock's versions.
# Dependabot keeps the base images current.

# The app is WebAssembly, the same on every platform, so it is built once on the build machine's
# own platform, also when the image is for another (`--platform linux/amd64,linux/arm64`).
FROM --platform=$BUILDPLATFORM rust:1.100.0-slim@sha256:6f14b2b4b278ec0d50aa1d69541c304ce0f95e0f6268487aa9924af1c2e9aebe AS build

# clang compiles the Zstandard decoder in rustgis-data to WebAssembly; curl downloads Trunk, and
# crates/render/build.rs uses curl to fetch the Natural Earth sample data it embeds.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates clang curl \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add wasm32-unknown-unknown

# The Trunk release web-deploy.yml and docs-preview.yml pin (keep them in step), for the build
# machine, checksum-verified. The steps are chained with &&, so a wrong checksum stops the build
# in any shell (older Podman ignores SHELL).
ARG BUILDARCH
RUN case "$BUILDARCH" in \
      amd64) arch=x86_64; sum=f2b4680cd239693a646a2795e4633c625328d7b2a044fbe749fa3a2fe9e7036b ;; \
      arm64) arch=aarch64; sum=b1d8e60e454f7fc182d9a4d95d1506ffbae947d8ba90f8f6f02da93b60f980f9 ;; \
      *) echo "no Trunk release for BUILDARCH='$BUILDARCH' (BuildKit sets it)" >&2; exit 1 ;; \
    esac \
    && curl -sSfL -o /tmp/trunk.tar.gz "https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-${arch}-unknown-linux-gnu.tar.gz" \
    && echo "${sum}  /tmp/trunk.tar.gz" > /tmp/trunk.sha256 \
    && sha256sum -c /tmp/trunk.sha256 \
    && tar xzf /tmp/trunk.tar.gz -C /usr/local/bin trunk \
    && rm /tmp/trunk.tar.gz /tmp/trunk.sha256

WORKDIR /src
COPY . .

# The cargo registry, the target directory and the tools Trunk downloads are cache mounts, so a
# rebuild after a change redoes only what changed. The output, dist/web, is outside them.
WORKDIR /src/apps/rustgis-web
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    --mount=type=cache,target=/root/.cache/trunk \
    trunk build --release --locked --public-url ./
WORKDIR /src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo xtask web-pwa dist/web

# The slim variant: nginx without the optional modules (and their libraries) this image doesn't use.
FROM nginxinc/nginx-unprivileged:1.31.7-alpine-slim@sha256:a8cb0f33d51fa107c96fd2bd3a71aaf564ec131e9d2bdf8700944f0ed8b170d7

# The base image's labels describe nginx: the ones that apply are replaced, the rest cleared.
LABEL org.opencontainers.image.title="RustGIS web app" \
      org.opencontainers.image.description="The RustGIS web app (WebAssembly), served by nginx" \
      org.opencontainers.image.url="https://rustgis.app" \
      org.opencontainers.image.source="https://github.com/opengeos/RustGIS" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="" \
      org.opencontainers.image.revision="" \
      org.opencontainers.image.created="" \
      maintainer=""

COPY packaging/docker/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=build /src/dist/web /usr/share/nginx/html

EXPOSE 8080
HEALTHCHECK --timeout=5s CMD wget -q --spider http://127.0.0.1:8080/healthz || exit 1
