# deploy

Deployment assets for Shelfy Web, the self-hosted server. The desktop app uses nothing in
this directory.

The plan of record is [docs/web-port/IMPLEMENTATION-PLAN.md](../docs/web-port/IMPLEMENTATION-PLAN.md):
§2.4 fixes what lives here and §3 how it runs on the VPS. Each item lands with the task that
needs it; the last column says when.

| Item | Contents | Added in |
| --- | --- | --- |
| `compose.dev.yml` | Local stack, used until the osn PRs land (§3): `shelfy-api` built and run from the working tree in `rust:1.99.0-bookworm`, API on `127.0.0.1:8080`, metrics on the compose network only | P0 (T7) |
| `docker/shelfy-api.Dockerfile` (with `shelfy-api.Dockerfile.dockerignore`) | The `shelfy-api` image (§3.2): `cargo-chef` on `rust:1.99.0-bookworm` into `debian:bookworm-slim`, both pinned by digest, with `ca-certificates`, `tini`, Debian `ffmpeg`, yt-dlp 2026.08.19 (the unpacked `yt-dlp_linux.zip` build, SHA-256 checked per architecture), the server binary and `web/dist` with brotli and gzip siblings. Runs as uid 10100 on a read-only root. [Building and running the image](#building-and-running-the-image) | P1-09 |
| `nginx/refs.niccolofanton.dev.conf` | The edge route of `refs.niccolofanton.dev` (§3.3, E5): its `log_format` and `server` block, to `shelfy-api:8080`, with no gzip and no buffering (SSE, uploads), 3600 s timeouts, 32 MiB bodies and an access log without query strings. The live copy is in osn's `edge/nginx.conf`: keep the two identical | P1-16 |
| `osn/` | osn patch set, landed as two osn PRs (Appendix B). PR 1: `shelfy-api` service, edge route, DNS and Access, secrets, backups, scrape config, Grafana dashboard `03-shelfy.json` and alert rules (§3.5, §3.6). PR 2: `shelfy-capture` and `shelfy-egress` services, seccomp profile and Smokescreen settings, capture alerts and panels. osn vendors `osn/shelfy/` with `just shelfy-sync <Shelfy checkout>`, so edit these files here | PR 1 in P1, PR 2 in P4 |
| `osn/shelfy/grafana/03-shelfy.json` | Grafana dashboard (§3.6): HTTP rate and latency per route group, jobs, disk against the budgets, SQLite, `g480` sizes, container CPU and memory next to Hermes, host contention, backup ages. Datasource uid `vmds`, as in osn | P1-16 |
| `osn/shelfy/alerting/shelfy.yaml` | Grafana alert rules (§3.6): metrics scrape down, queue stuck, job failures, slow API, media breaker (from P2), libraries near the disk budget, container memory. "Shelfy DOWN" is osn's probe rule on `/health`; root disk and host contention are osn rules | P1-16 |
| `osn/shelfy/alerting/shelfy-backups.yaml` | Stale-backup rules (db > 3 h, media > 36 h, drill > 35 days, restic maintenance > 8 days) and a missing-metric rule; the osn role `shelfy` installs them only while `shelfy_backups_enabled` is true | P1-16; on in P1-23 |
| `osn/shelfy/backup/` | The four backup jobs of §3.5 as scripts and systemd units for the osn role `shelfy`: hourly database snapshot and restic backup, daily media backup, weekly retention, prune and check, monthly restore drill, each writing textfile metrics. See [Backups and restores](#backups-and-restores) | P1-12; installed by P1-16, enabled after O2 in P1-23 |
| `rehearse-backups.sh` | Local rehearsal of the backups and of both restore runbooks on synthetic data, with restic in a container and a local repository | P1-12 |
| `compose.test.yml` | Test stack (§3.8): `shelfy-api` from a local image with its own data (a named volume, or a host path), production's user, read-only root, limits and healthcheck, API on `127.0.0.1:8081`. Optional `mail` profile with mailpit (E4). The mock AI provider, the fixture CDN and Smokescreen allowing only the fixture subnet join in P2–P4; P1-21 runs the web e2e suite on it | P1-09 |
| `restart-check.sh` | Restarts `shelfy-api` in the test stack and fails unless `/health` answers 200 within 3 s (§6.2) | P1-09 |
| Dockerfile for `shelfy-capture` | `node:24-bookworm-slim`, pinned `playwright-core` with `chromium-headless-shell`, Debian `ffmpeg`, Noto and Liberation fonts (§2.18) | P4 |
| Dockerfile for `shelfy-egress` | Smokescreen built from a pinned commit on `golang`, shipped on `distroless/static` (§3.2) | P4 |
| Smokescreen ACL | Egress policy: public destinations only, ports 80 and 443, extra deny ranges (§2.18) | P4 (SPIKE-4) |
| `osn/shelfy/chromium-seccomp.json` | Chromium seccomp profile mounted into `shelfy-capture` (§3.2) | P4 (SPIKE-4) |

`.github/workflows/release-server.yml` builds the image on `server-v*` tags and pushes it to GHCR
(§3.8): see [Releases](#releases).

## Server configuration

`shelfy-server serve` reads its settings from the environment; each variable also has a
command-line flag, and `shelfy-server serve --help` lists both with their defaults. Values are
validated at start: a bad one stops the process with a message.

| Variable | Default | Meaning |
| --- | --- | --- |
| `SHELFY_DATA_DIR` | `/data/shelfy` | Data directory (§2.5). `serve` creates `control/` and `users/` (mode 0750) if missing |
| `SHELFY_LISTEN_ADDR` | `0.0.0.0:8080` | API listener; the edge nginx is its only client |
| `SHELFY_METRICS_ADDR` | `0.0.0.0:9464` | Prometheus listener (`GET /metrics`). Never proxied: publish it on the internal network only. Must not share the API port |
| `SHELFY_PUBLIC_URL` | `http://localhost:8080` | Public origin, without a path. Links the server hands out start with it (`/login/magic#<token>`), and every state-changing request without an `Authorization` header must send it as its `Origin` (CSRF check). It is also the passkey relying party: the RP ID is its host and the only accepted origin is the URL itself, so changing the host orphans every registered passkey. Use https unless the host is localhost: the session cookie is `Secure`, and browsers offer passkeys in secure contexts only (an IP address or plain http elsewhere turns passkeys off) |
| `SHELFY_TRUSTED_PROXIES` | none | Proxies whose `CF-Connecting-IP` header names the client, as CIDR blocks separated by commas: on the VPS, the Docker network of the edge nginx. Requests from any other peer are keyed on their TCP address (sign-in rate limits; IPv6 clients by /64). Empty: the header is ignored, and behind a proxy every client shares the proxy's budget |
| `SHELFY_LOG_FORMAT` | `json` | `json` (one object per line, for Docker's `json-file`) or `text` |
| `RUST_LOG` | `info` | Log filter (`tracing` env-filter syntax); an invalid filter stops the start |
| `SHELFY_OWNER_EMAIL` | none | Default `--email` of `shelfy-server admin create-owner` and `admin login-link` (E4) |
| `SHELFY_SMTP_HOST` | none | SMTP relay for sign-in emails, `host[:port]` (§3.2: `smtp.resend.com:587`). Setting it turns email sign-in on. Without it, and without the dev mailbox, email is off and `admin login-link` is the way in (E4) |
| `SHELFY_SMTP_TLS` | `starttls` | `starttls` (required, not opportunistic; port 587), `tls` (implicit TLS; port 465) or `none` (plain text for a local catcher such as mailpit: only to a loopback, private or single-label host, and refused together with credentials) |
| `SHELFY_SMTP_USER`, `SHELFY_SMTP_PASSWORD` | none | SMTP credentials, set together. The password is a secret (§3.4: `RESEND_API_KEY`) |
| `SHELFY_SMTP_FROM` | none | Sender, `address` or `Name <address>`; required with `SHELFY_SMTP_HOST` |
| `SHELFY_DEV_MAILBOX` | `false` | Write emails as `.eml` files to `<data>/dev-mailbox/` instead of sending them. Local runs and tests only: refused together with `SHELFY_SMTP_HOST`, and unless `SHELFY_PUBLIC_URL` is loopback (`localhost`) |
| `SHELFY_WEB_DIR` | none (`/app/web` in the image) | The built web app (`web/dist`) to serve. Its `index.html` answers every path that no route takes and that is not under `/api`, `/media`, `/health` or `/.well-known`; files under `/assets/` are immutable. The directory must hold `index.html`, or the start fails. Unset: the API only |

Empty values count as unset, so a compose file may pass `SHELFY_SMTP_HOST=` when email is off.
Later tasks add the master key, the capture and egress endpoints and the media budgets (§3.2,
§3.4). The operator commands (`shelfy-server admin create-owner | invite | login-link |
snapshot | verify | user | install-snapshots | migrate-token`) use `SHELFY_DATA_DIR` too and
print their results on stdout, never to the logs.

To sign in, create the owner once, then mint a one-time link (valid 15 minutes):

```sh
shelfy-server admin create-owner --email you@example.com
shelfy-server admin login-link --email you@example.com
```

The link is `<SHELFY_PUBLIC_URL>/login/magic#<token>`. The token sits in the URL's fragment,
which browsers never send to a server, so no proxy or server log sees it. Open the link in a
browser: the web app's sign-in page redeems the token after a click (no `GET` ever redeems a
link, so email scanners cannot use it up). Without the web app, redeem it with `curl`; the
answer sets the `__Host-shelfy_session` cookie:

```sh
TOKEN='<what follows # in the link>'
curl -i -X POST "$SHELFY_PUBLIC_URL/api/v1/auth/magic-links/redeem" \
  -H 'Content-Type: application/json' -H "Origin: $SHELFY_PUBLIC_URL" -H 'X-Shelfy-Client: web' \
  -d "{\"token\":\"$TOKEN\"}"
```

Signed in, register a passkey within 5 minutes of the sign-in (the web app's Settings, from
P1-20; the API is `POST /api/v1/me/passkeys/start` then `POST /api/v1/me/passkeys`): from then
on the passkey signs in without a link or an email. Adding or removing a passkey, like other
sensitive actions, needs a sign-in or a re-authentication from the last 5 minutes. The web app
re-authenticates with a passkey or, when email is on, an emailed link; otherwise mint a
re-authentication link and open it in the browser that is signed in:

```sh
shelfy-server admin login-link --email you@example.com --purpose reauth
```

It prints `<SHELFY_PUBLIC_URL>/login/reauth#<token>`, valid 15 minutes, once.

Every route needs a signed-in session unless it is listed as public (or open to API tokens) in
`crates/server/src/routes/mod.rs`.

## Metrics, logs and rate limits

`GET /metrics` on `SHELFY_METRICS_ADDR` answers in the Prometheus text format (§3.6). No label
carries a user id or any other per-user value. The gauges are sampled every 5 seconds, the disk
every 5 minutes; `crates/server/src/telemetry/metrics.rs` has the buckets. The dashboard
`osn/shelfy/grafana/03-shelfy.json` and the rules in `osn/shelfy/alerting/shelfy.yaml` read these
names and labels: change them together.

| Metric | Type | Labels | Meaning |
| --- | --- | --- | --- |
| `shelfy_http_requests_total` | counter | `route`, `method`, `status` | Responses. `route` is the route template (`/api/v1/posts/{key}`), `spa` for the web app's files, or `unmatched` |
| `shelfy_http_request_duration_seconds` | histogram | `route` | Time to the response headers, with bucket bounds at the §6.2 budgets |
| `shelfy_sse_connections` | gauge | — | Open realtime streams (`GET /api/v1/events`) |
| `shelfy_jobs` | gauge | `kind`, `state` | Jobs the scheduler holds: `ready` (due, paused queues included), `delayed`, `running` |
| `shelfy_job_oldest_queued_seconds` | gauge | `kind` | How long the oldest due job of an unpaused queue has waited; 0 when none |
| `shelfy_job_duration_seconds` | histogram | `kind`, `outcome` | Each ended attempt: `succeeded`, `failed` (for good), `retried`, `requeued`, `interrupted` (shutdown), `cancelled`, `lease_expired` |
| `shelfy_disk_bytes` | gauge | `area` | Bytes of the files under `control`, `users`, `cache`, `work`, `backup_staging` and `other` (the rest of the data directory) |
| `shelfy_open_user_dbs` | gauge | — | Libraries open in the handle cache (at most 64) |
| `shelfy_sqlite_busy_total` | counter | — | SQLite calls that gave up on a lock after `busy_timeout` (5 s) |
| `shelfy_rendition_bytes` | histogram | `variant` | Size of each rendition written (`g480`), with bucket bounds at 35 and 60 KB |
| `shelfy_build_info` | gauge | `version` | Always 1 |

Logs never carry session ids, tokens, email addresses, captions, notes, post URLs or query
strings: requests are logged by route template, and the free text of client error reports is
scrubbed of URLs, addresses, query strings and tokens before it is logged.

Over a rate limit (§2.9) the API answers 429 `rate_limited` with `Retry-After`:

| Requests | Limit | Counted per |
| --- | --- | --- |
| every `/api/v1/*` route of a signed-in user (session or API token) | 20 a second, 60 at once | user |
| searches: `GET /api/v1/search`, and `GET /api/v1/posts` or `/posts/count` with `q` or `concept` | 5 a second | user |
| `POST /api/v1/client-errors` | 10 a minute | user |
| every `/api/v1/auth/*` route | 10 a minute | client address (`SHELFY_TRUSTED_PROXIES`; IPv6 by /64) |

`/media/*`, `/health` and the web app's files are not limited.

## Building and running the image

The image takes the web app as built, so build `web/dist` first. BuildKit is required (the
default of current Docker). A local build targets the host's architecture; releases are
linux/amd64.

```sh
pnpm install --frozen-lockfile
pnpm run web:build
docker build -f deploy/docker/shelfy-api.Dockerfile -t shelfy-api:local .
```

| Path in the image | Contents |
| --- | --- |
| `/app/shelfy-server` | The server. `ENTRYPOINT ["/usr/bin/tini", "--", "/app/shelfy-server"]` and `CMD ["serve"]`, so `docker exec <container> /app/shelfy-server admin …` runs the operator commands |
| `/app/web` | `web/dist`, plus `.br` and `.gz` siblings of its text files over 1 KiB, sent to clients that accept them |
| `/usr/bin/ffmpeg`, `/usr/local/bin/yt-dlp` | Debian's ffmpeg; yt-dlp unpacked in `/opt/yt-dlp`, so it runs with a `noexec` `/tmp` |
| `/data/shelfy` | The data directory, owned by 10100 with mode 0750: mount the volume here. A new named volume takes this ownership |

The image sets `SHELFY_DATA_DIR=/data/shelfy`, `SHELFY_WEB_DIR=/app/web`,
`SHELFY_LISTEN_ADDR=0.0.0.0:8080` and `SHELFY_METRICS_ADDR=0.0.0.0:9464`, and runs as
`10100:10100`. A deployment adds `SHELFY_PUBLIC_URL`, `SHELFY_TRUSTED_PROXIES` (the edge
network) and, if email is on, the SMTP settings. It also gives the container a read-only root,
a tmpfs on `/tmp` and the limits of §3.2; `compose.test.yml` does the same locally. Most of the
image's size is Debian's ffmpeg and the libraries it pulls in.

```sh
docker run -d --name shelfy-api --read-only --tmpfs /tmp:size=256m \
  -v shelfy-data:/data/shelfy -p 127.0.0.1:8080:8080 \
  -e SHELFY_PUBLIC_URL=http://localhost:8080 shelfy-api:local
docker exec shelfy-api /app/shelfy-server admin create-owner --email you@example.com
```

`shelfy-server healthcheck` is the container's health probe (the image's `HEALTHCHECK` and the
compose healthcheck). It asks `GET /health` of `SHELFY_LISTEN_ADDR` (an unspecified address is
probed on the loopback) and exits 0 only on a 200 with `"status": "ok"`, 1 otherwise, within
`--timeout` seconds (default 4).

Every response of the API listener carries the content security policy of plan §7.1 (media
answers add `sandbox`), `X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`.
With an https `SHELFY_PUBLIC_URL` it also carries `Strict-Transport-Security: max-age=31536000`,
for that host only. `index.html` is `Cache-Control: no-cache` with an `ETag`; `/assets/*` is
`public, max-age=31536000, immutable`.

## Releases

`.github/workflows/release-server.yml` runs on tags `server-vX.Y.Z-rc.N` (a release candidate,
on `web/foundations` or `main`) and `server-vX.Y.Z` (on `main` only); `X.Y.Z` must be the
workspace version in `Cargo.toml`. It pushes `ghcr.io/niccolofanton/shelfy-api:vX.Y.Z[-rc.N]` and
`:sha-<7 hex>` for linux/amd64, signs the image with cosign (keyless) and creates a GitHub
release with `shelfy-migrate` for macOS arm64 and x64, Windows x64 and Linux x64, the image's
SBOM (`shelfy-api.spdx.json`, syft) and `SHA256SUMS`. A server release is a pre-release (rc) or
is created with `--latest=false`: it never becomes the "Latest" release that the desktop
updater reads.

```sh
git tag server-v0.1.0-rc.1 origin/web/foundations
git push origin server-v0.1.0-rc.1
# Then check the signature of the digest the release notes give:
cosign verify ghcr.io/niccolofanton/shelfy-api@sha256:<digest> \
  --certificate-identity-regexp '^https://github.com/niccolofanton/shelfy/\.github/workflows/release-server\.yml@' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

The GHCR package starts private. Until it is public or the VPS has a pull token, the image goes
to the VPS with `docker save` and `docker load` under the same tag (P1 assumption G4).

## Moving a desktop library

`shelfy-migrate run` uploads a desktop library to the server (plan §4.1) and installs it into
an empty web library. Until its own device-code sign-in arrives (P1-17, P1-19), it takes an API
token with the `migrate` scope, valid 7 days, that the operator mints:

```sh
shelfy-server admin migrate-token --email you@example.com > migrate-token   # keep it private
shelfy-migrate plan --db "<userData>/shelfy.sqlite" --redact                  # dry run first
shelfy-migrate run --db "<userData>/shelfy.sqlite" --server "$SHELFY_PUBLIC_URL" \
  --token-file migrate-token --work-dir <scratch dir>
```

`<userData>` is the desktop app's data directory (`~/Library/Application Support/Shelfy` on
macOS); `--media-root` points at it when the library file was copied elsewhere. The desktop data
is only read. With Shelfy open, `run` reads a snapshot. Kept videos stay behind unless
`--with-videos` is given. An interrupted run continues where it stopped when run again with the
same `--work-dir`. The run ends with a reconciliation of desktop, bundle and installed counts;
the server keeps the previous web library next to the new one as `library.prev-<id>.sqlite`.

## Backups and restores

Plan §3.5. Four host timers back up to a restic repository (`restic/shelfy` on R2); each job
writes its result for node-exporter's textfile collector. The jobs are the scripts in
`osn/shelfy/backup/`; `lib.sh` there lists their settings, whose defaults are the osn layout
(`/data/shelfy`, `/data/observability/textfile`, the API container `osn-shelfy-api-1`, the
restic credentials in `/etc/shelfy/restic.env`). restic runs in its official image, pinned by
digest, as `nice -n 19 ionice -c3 restic --limit-upload 20480`; the snapshot runs at the same
priority inside the API container.

| Timer | When (UTC) | What | Metric (`…` and `…_timestamp_seconds`) |
| --- | --- | --- | --- |
| `shelfy-db-snapshot` | hourly at :05 | `admin snapshot --changed` into `backup-staging/db`, then `restic backup --tag db` | `shelfy_backup_last_success{set="db"}` |
| `shelfy-media-backup` | daily 03:30 | `restic backup --tag media` of `users/`, without the databases, exports, temporary files and locks | `shelfy_backup_last_success{set="media"}` |
| `shelfy-restic-maintenance` | Sundays 04:30 | `forget` (db: 48 hourly, 14 daily, 8 weekly, 6 monthly; media: 7 daily, 8 weekly, 6 monthly), `prune`, `check --read-data-subset=5%` | `shelfy_restic_check_last_success` |
| `shelfy-restore-drill` | the 1st of the month, 05:30 | restores the control database and one random library from the latest db snapshot, then `admin verify --max-drift 10` | `shelfy_restore_drill_last_success` |

Each metric is 1 when the job's last run succeeded and 0 when it failed;
`…_timestamp_seconds` is the time of the last success, kept across failures, which is what the
"backups stale" alert of §3.6 watches (db over 3 h, media over 36 h, drill over 35 days). The
first backup creates the repository.

The operator commands behind them:

| Command | What it does |
| --- | --- |
| `admin snapshot [--out DIR] [--changed] [--user ID]…` | Copies the control database and the libraries with SQLite's online backup API (default `DIR`: `backup-staging/db`). With `--changed`, a library is copied only if its files changed since its copy was taken. Locked libraries keep their previous copy; copies of deleted users are removed |
| `admin verify DIR [--user ID]… [--max-drift PERCENT]` | Checks the copies in `DIR`: `PRAGMA integrity_check`, foreign keys, the schema version, row counts against the live databases (default: equal; volatile tables such as sessions are only reported) and every media reference against the live store. Exit status 1 on any problem |
| `admin user lock ID [--reason TEXT]` | Locks a user's library: their requests answer 423 `user_locked`, and the server releases the library |
| `admin user unlock ID` | Unlocks it |
| `admin user restore-db ID FILE [--wait-secs N]` | Replaces a locked user's library with `FILE` after checking it, keeping the current one as `users/ID/library.pre-restore-<time>.sqlite`. Waits up to N seconds (default 120) for the server to release the library |
| `admin install-snapshots DIR [--force]` | Full restore, with the server stopped: checks every copy in `DIR`, installs them as the live databases, then runs `verify`. `--force` replaces existing data, keeping each replaced database next to it |

**Restore one user to a point in time.** Snapshot paths are the host paths; restore inside the
data directory so the API container sees the file.

```sh
shelfy-server admin user lock "$USER_ID"
restic restore "$SNAPSHOT:/data/shelfy/backup-staging/db/users" --include "/$USER_ID.sqlite" \
  --target /data/shelfy/backup-staging/restore
shelfy-server admin user restore-db "$USER_ID" "/data/shelfy/backup-staging/restore/$USER_ID.sqlite"
shelfy-server admin user unlock "$USER_ID"
```

**Restore the whole host.** After `just bootstrap`, with `shelfy-api` stopped:

```sh
restic restore latest:/data/shelfy/users --tag media --target /data/shelfy/users
restic restore latest:/data/shelfy/backup-staging/db --tag db --target /data/shelfy/restore/db
shelfy-server admin install-snapshots /data/shelfy/restore/db
```

Then `just deploy` and `just check`. Objects that a restored library references but the store
lost are listed by `verify` and re-archived later (P2–P4).

**Rehearsal.** `deploy/rehearse-backups.sh [WORK_DIR] [PORT]` runs all of this locally on a
synthetic library, with restic in a container and a repository in `WORK_DIR`, and fails unless
every metric reports success.
