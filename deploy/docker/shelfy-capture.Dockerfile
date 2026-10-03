# syntax=docker/dockerfile:1
# Build from the repository root. Chromium and the adblock engine are installed
# at build time; the service cannot install browsers or filters at runtime.
ARG NODE_IMAGE=node:24-bookworm-slim@sha256:0e0ff40c39bc087845bfb27465a0df4ea419520094bc35842ff83dd8cbe6f9b6
FROM ${NODE_IMAGE} AS build
WORKDIR /src
COPY deploy/docker/shelfy-capture-runtime/package*.json ./
RUN npm ci --no-audit --no-fund
COPY capture capture
COPY electron electron
COPY src src
COPY shared/capture shared/capture
COPY build/prepare-adblock.ts build/prepare-adblock.ts
RUN node_modules/.bin/tsx build/prepare-adblock.ts \
    && node_modules/.bin/tsx capture/build.ts \
    && npm prune --omit=dev --ignore-scripts --no-audit --no-fund

FROM ${NODE_IMAGE}
LABEL org.opencontainers.image.title="shelfy-capture" \
      org.opencontainers.image.description="Shelfy sandboxed capture service" \
      org.opencontainers.image.source="https://github.com/niccolofanton/shelfy" \
      org.opencontainers.image.licenses="Apache-2.0"
WORKDIR /app
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/playwright \
    SHELFY_ADBLOCK_ENGINE=/app/resources/adblock/engine.bin \
    CAPTURE_WORK_BASE=/work \
    FFMPEG_BIN=/usr/bin/ffmpeg \
    HOME=/tmp \
    NODE_USE_ENV_PROXY=1
COPY --from=build /src/node_modules node_modules
COPY --from=build /src/capture/dist ./
COPY deploy/docker/shelfy-capture-runtime/package.json package.json
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates ffmpeg tini iproute2 fonts-noto-core fonts-noto-cjk fonts-noto-color-emoji fonts-liberation \
    && node node_modules/playwright-core/cli.js install --with-deps chromium-headless-shell \
    && rm -rf /var/lib/apt/lists/* /root/.npm \
    && groupadd --gid 10100 shelfy \
    && useradd --uid 10100 --gid shelfy --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin shelfy \
    && install --directory --owner=10100 --group=10100 --mode=0750 /work
COPY --chmod=0555 deploy/capture-isolation-check.sh deploy/fixtures/capture/isolation-check.mjs ./
COPY scripts/spikes/ssrf-probe.mjs ./ssrf-probe.mjs
USER 10100:10100
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD NODE_USE_ENV_PROXY=0 node -e "fetch('http://127.0.0.1:8080/health').then(r=>process.exit(r.ok?0:1),()=>process.exit(1))"
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["node", "/app/server.cjs"]
