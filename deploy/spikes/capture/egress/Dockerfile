# shelfy-egress (plan §3.2): Smokescreen, pinned in go.mod, with the port
# allowlist wrapper in main.go, on distroless/static. SPIKE-4 candidate for osn PR 2.
#
#   docker build -t shelfy-egress deploy/spikes/capture/egress
#   docker run --rm -v "$PWD/deploy/spikes/capture/smokescreen:/etc/shelfy-egress:ro" shelfy-egress
#
# Both base images are pinned by digest (multi-arch indexes).

FROM golang:1.27.1-bookworm@sha256:69a7b9788769bec032d238959b61854e9ae87f57be9029ec04e9885fabf99195 AS build
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY main.go ./
ARG TARGETOS=linux TARGETARCH
RUN CGO_ENABLED=0 GOOS=$TARGETOS GOARCH=$TARGETARCH \
    go build -trimpath -ldflags '-s -w' -o /out/shelfy-egress .

FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
COPY --from=build /out/shelfy-egress /usr/local/bin/shelfy-egress
USER 10101:10101
EXPOSE 4750
ENTRYPOINT ["/usr/local/bin/shelfy-egress"]
CMD ["--config-file=/etc/shelfy-egress/config.yaml"]
