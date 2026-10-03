# syntax=docker/dockerfile:1

# The shelfy-egress image (plan §3.2, SPIKE-4): Smokescreen (pinned in
# deploy/docker/shelfy-egress/go.mod to the commit fa5bb56) with the
# port-allowlist wrapper in main.go, built on a pinned golang image and shipped
# on distroless/static:nonroot as uid 10101. ~20 MB; nothing but the static
# binary and the CA bundle distroless carries.
#
# Build it from the repository root; the context is filtered to the Go sources
# by shelfy-egress.Dockerfile.dockerignore next to this file (BuildKit required):
#
#   docker build -f deploy/docker/shelfy-egress.Dockerfile -t shelfy-egress:local .
#
# A local build targets the host's architecture; the release workflow builds
# linux/amd64 (.github/workflows/release-server.yml). The runtime policy is in
# deploy/egress/ (config.yaml, acl.yaml) and the arguments in deploy/README.md.
#
# Pinned inputs, bumped by hand:
# - the base images, by digest (multi-arch indexes).
# - Smokescreen and every other module, by deploy/docker/shelfy-egress/go.sum.

FROM golang:1.27.1-bookworm@sha256:69a7b9788769bec032d238959b61854e9ae87f57be9029ec04e9885fabf99195 AS build
WORKDIR /src
COPY deploy/docker/shelfy-egress/go.mod deploy/docker/shelfy-egress/go.sum ./
RUN go mod download
COPY deploy/docker/shelfy-egress/main.go ./
ARG TARGETOS=linux TARGETARCH
RUN CGO_ENABLED=0 GOOS=$TARGETOS GOARCH=$TARGETARCH \
    go build -trimpath -ldflags '-s -w' -o /out/shelfy-egress .

FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
LABEL org.opencontainers.image.title="shelfy-egress" \
      org.opencontainers.image.description="Shelfy Web: the Smokescreen egress proxy (SSRF policy, ports 80/443)" \
      org.opencontainers.image.source="https://github.com/niccolofanton/shelfy" \
      org.opencontainers.image.licenses="Apache-2.0"
COPY --from=build /out/shelfy-egress /usr/local/bin/shelfy-egress
USER 10101:10101
EXPOSE 4750
ENTRYPOINT ["/usr/local/bin/shelfy-egress"]
CMD ["--config-file=/etc/shelfy-egress/config.yaml"]
