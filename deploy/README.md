# deploy

Deployment assets for Shelfy Web, the self-hosted server. The desktop app uses nothing in
this directory.

The plan of record is [docs/web-port/IMPLEMENTATION-PLAN.md](../docs/web-port/IMPLEMENTATION-PLAN.md):
§2.4 fixes what lives here and §3 how it runs on the VPS. Each item lands with the task that
needs it, so for now the directory holds only this README.

| Item | Contents | Added in |
| --- | --- | --- |
| `compose.dev.yml` | Local stack, used until the osn PRs land (§3) | P0 (T7) |
| Dockerfile for `shelfy-api` | Multi-stage build (`cargo-chef`, pinned `rust:*-bookworm`) into `debian:bookworm-slim` with `ca-certificates`, `tini`, Debian `ffmpeg`, the pinned `yt-dlp_linux` (SHA-256 checked), the server binary and `web/dist` (§3.2) | P1 |
| nginx snippet | `shelfy.niccolofanton.dev` server block for the osn edge nginx (§3.3) | P1 |
| `osn/` | osn patch set, landed as two osn PRs (Appendix B). PR 1: `shelfy-api` service, edge route, DNS and Access, secrets, backups, scrape config, Grafana dashboard `03-shelfy.json` and alert rules (§3.5, §3.6). PR 2: `shelfy-capture` and `shelfy-egress` services, seccomp profile and Smokescreen settings, capture alerts and panels | PR 1 in P1, PR 2 in P4 |
| `compose.test.yml` | CI web e2e stack: mock AI provider, fixture CDN, Smokescreen that allows only the fixture subnet (§3.8) | With the first web e2e suite (no phase fixed in the plan) |
| Dockerfile for `shelfy-capture` | `node:24-bookworm-slim`, pinned `playwright-core` with `chromium-headless-shell`, Debian `ffmpeg`, Noto and Liberation fonts (§2.18) | P4 |
| Dockerfile for `shelfy-egress` | Smokescreen built from a pinned commit on `golang`, shipped on `distroless/static` (§3.2) | P4 |
| Smokescreen ACL | Egress policy: public destinations only, ports 80 and 443, extra deny ranges (§2.18) | P4 (SPIKE-4) |
| `osn/shelfy/chromium-seccomp.json` | Chromium seccomp profile mounted into `shelfy-capture` (§3.2) | P4 (SPIKE-4) |

The images will be built by `.github/workflows/release-server.yml` on `server-v*` tags and
pushed to GHCR (§3.8).
