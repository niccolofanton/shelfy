# syntax=docker/dockerfile:1

# The shelfy-api image (plan §3.2): shelfy-server, the built web app, Debian's
# ffmpeg and a pinned yt-dlp on debian:bookworm-slim. It runs as uid 10100 and
# works on a read-only root filesystem: it writes only to /data/shelfy (the
# volume) and /tmp (a tmpfs in compose).
#
# Build it from the repository root, after the web app (the image takes
# web/dist as it is, so no Node toolchain runs inside the build):
#
#   pnpm run web:build
#   docker build -f deploy/docker/shelfy-api.Dockerfile -t shelfy-api:local .
#
# BuildKit is required (TARGETARCH, and the build context filtered by
# shelfy-api.Dockerfile.dockerignore next to this file). A local build targets
# the host's architecture; the release workflow builds linux/amd64
# (.github/workflows/release-server.yml). deploy/README.md lists the settings.
#
# Pinned inputs, bumped by hand:
# - the base images, by digest. RUST_IMAGE keeps the version of
#   rust-toolchain.toml; the toolchain file itself stays out of the build, so
#   rustup installs no extra component.
# - cargo-chef, by version.
# - yt-dlp, by release and by the SHA-256 of each architecture's build, taken
#   from the release's SHA2-256SUMS (electron/binaries.ts pins the same
#   release for the desktop).

ARG RUST_IMAGE=rust:1.99.0-bookworm@sha256:59037199c44290f2befcdd58dcc540164763fc296950255aaefeef096a1866b0
ARG RUNTIME_IMAGE=debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251

# --- Rust: dependencies first (cargo-chef), so a code change reuses them ---

FROM ${RUST_IMAGE} AS chef
ARG CARGO_CHEF_VERSION=0.1.78
RUN cargo install cargo-chef --locked --version "${CARGO_CHEF_VERSION}" \
    && rm -rf "${CARGO_HOME}/registry"
WORKDIR /src

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json \
    --package shelfy-server --bin shelfy-server
COPY Cargo.toml Cargo.lock ./
COPY crates crates
# The core embeds the AI prompts and schemas with include_str!.
COPY shared/ai shared/ai
RUN cargo build --release --locked --package shelfy-server --bin shelfy-server \
    && install -D -m 0755 target/release/shelfy-server /out/shelfy-server

# --- yt-dlp: the pinned release, checked against its SHA-256 ---
#
# The unpacked build (yt-dlp_linux.zip), not the one-file yt-dlp_linux: the
# one-file binary unpacks ~100 MB into /tmp on every run and loads libraries
# from there, which fails on the noexec tmpfs of compose and would count
# against the container's memory.

FROM ${RUST_IMAGE} AS yt-dlp
ARG YTDLP_VERSION=2026.08.19
ARG YTDLP_SHA256_AMD64=32e72032766bef9199d99d15beb69fd52e46df8f8b06f0d8745db59e04d339e9
ARG YTDLP_SHA256_ARM64=4e27ad43f3a34bacffd078694eb3edbb4e3b378e7da44edab2be02e98555516e
ARG TARGETARCH
RUN set -eu; \
    case "${TARGETARCH}" in \
      amd64) build=yt-dlp_linux; sha256="${YTDLP_SHA256_AMD64}" ;; \
      arm64) build=yt-dlp_linux_aarch64; sha256="${YTDLP_SHA256_ARM64}" ;; \
      *) echo "no pinned yt-dlp for '${TARGETARCH}'" >&2; exit 1 ;; \
    esac; \
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
      --output /tmp/yt-dlp.zip \
      "https://github.com/yt-dlp/yt-dlp/releases/download/${YTDLP_VERSION}/${build}.zip"; \
    echo "${sha256}  /tmp/yt-dlp.zip" | sha256sum --check --strict -; \
    unzip -q /tmp/yt-dlp.zip -d /opt/yt-dlp; \
    mv "/opt/yt-dlp/${build}" /opt/yt-dlp/yt-dlp; \
    chmod -R u=rwX,go=rX /opt/yt-dlp; \
    chmod 0755 /opt/yt-dlp/yt-dlp; \
    /opt/yt-dlp/yt-dlp --version

# --- The web app: web/dist plus brotli and gzip siblings of its text files ---

FROM ${RUNTIME_IMAGE} AS web
RUN apt-get update \
    && apt-get install --yes --no-install-recommends brotli \
    && rm -rf /var/lib/apt/lists/*
COPY web/dist /web
# The server sends a sibling when the client accepts its encoding
# (crates/server/src/static_files.rs). Files of 1 KiB or less gain nothing.
RUN set -eu; \
    test -f /web/index.html \
      || { echo 'web/dist has no index.html: run pnpm run web:build first' >&2; exit 1; }; \
    find /web -type f -size +1k \( -name '*.js' -o -name '*.mjs' -o -name '*.css' \
      -o -name '*.html' -o -name '*.svg' -o -name '*.json' -o -name '*.webmanifest' \
      -o -name '*.txt' -o -name '*.xml' -o -name '*.wasm' -o -name '*.map' \) \
      -exec brotli --best --keep --force {} + \
      -exec gzip --best --keep --force --no-name {} +

# --- The image ---

FROM ${RUNTIME_IMAGE}
LABEL org.opencontainers.image.title="shelfy-api" \
      org.opencontainers.image.description="Shelfy Web: the API server and the web app" \
      org.opencontainers.image.source="https://github.com/niccolofanton/shelfy" \
      org.opencontainers.image.licenses="Apache-2.0"
# tini is PID 1: it forwards SIGTERM to the server and reaps the ffmpeg and
# yt-dlp children.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates ffmpeg tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10100 shelfy \
    && useradd --uid 10100 --gid shelfy --no-create-home --home-dir /nonexistent \
      --shell /usr/sbin/nologin shelfy \
    && install --directory --owner=10100 --group=10100 --mode=0750 /data/shelfy /data/shelfy/work /data/shelfy/work/capture \
    && ln -s /opt/yt-dlp/yt-dlp /usr/local/bin/yt-dlp
COPY --from=yt-dlp /opt/yt-dlp /opt/yt-dlp
COPY --from=web /web /app/web
COPY --from=builder /out/shelfy-server /app/shelfy-server
ENV SHELFY_DATA_DIR=/data/shelfy \
    SHELFY_WEB_DIR=/app/web \
    SHELFY_LISTEN_ADDR=0.0.0.0:8080 \
    SHELFY_METRICS_ADDR=0.0.0.0:9464
USER 10100:10100
EXPOSE 8080 9464
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["/app/shelfy-server", "healthcheck"]
ENTRYPOINT ["/usr/bin/tini", "--", "/app/shelfy-server"]
CMD ["serve"]
