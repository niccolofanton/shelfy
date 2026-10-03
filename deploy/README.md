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
| `test-backup-jobs.sh` | Checks of the backup jobs' shared helpers that need no restic: the result of a job stopped by a signal, the textfile directory's mode, the restic env file's mode, restic's errors in the log. Runs in `debian:bookworm-slim` | F4 |
| `compose.test.yml` | Test stack (§3.8): `shelfy-api` from a local image with its own data (a named volume, or a host path), production's user, read-only root, limits and healthcheck, API on `127.0.0.1:8081`. Optional `mail` profile with mailpit (E4). The `ssrf` profile (P4-02) adds the egress proxy, a CoreDNS test resolver and nginx fixtures on their own `10.133.0.0/24` subnet. The mock AI provider and the fixture CDN join in P2–P4; P1-21 runs the web e2e suite on it | P1-09 |
| `restart-check.sh` | Restarts `shelfy-api` in the test stack and fails unless `/health` answers 200 within 3 s (§6.2) | P1-09 |
| `spikes/capture/` | SPIKE-4 deploy candidates for osn PR 2: the capture and egress compose services, the Chromium seccomp profile, the Smokescreen config and ACL, and `shelfy-egress` (Smokescreen with a port allowlist). [README](spikes/capture/README.md) | SPIKE-4; moved to `osn/shelfy/` in P4 |
| Dockerfile for `shelfy-capture` | `node:24-bookworm-slim`, pinned `playwright-core` with `chromium-headless-shell`, Debian `ffmpeg`, Noto and Liberation fonts (§2.18) | P4 |
| `docker/shelfy-egress.Dockerfile` (with `shelfy-egress.Dockerfile.dockerignore` and the Go sources in `docker/shelfy-egress/`) | The `shelfy-egress` image (§3.2, SPIKE-4): Smokescreen pinned to `fa5bb56` with the port-allowlist wrapper, built on `golang:1.27.1` into `distroless/static:nonroot`, uid 10101, ~15.5 MB. [egress/README.md](egress/README.md) | P4-02 |
| `egress/` (`config.yaml`, `acl.yaml`) | The egress proxy's policy (§2.18, §3.2, SPIKE-4): public destinations only via the `deny_ranges`, ports 80 and 443 (the wrapper), a `global_deny_list` of internal-only names, no proxy chaining. Production adds `--deny-range` for the VPS's own addresses; the test config adds `--allow-range` for the fixture subnet | P4-02 |
| `egress/ssrf/` | The SSRF suite (§6.1, §7.1): a CoreDNS test resolver, an nginx fixture/redirect origin, and the probe list `probes.json`. `crates/server/tests/ssrf.rs` runs it against the real proxy (CI `ssrf` job); every §6.1 probe is refused and the positive controls pass | P4-02 |
| `osn/shelfy/chromium-seccomp.json` | Chromium seccomp profile mounted into `shelfy-capture` (§3.2). Candidate: `spikes/capture/chromium-seccomp.json` | P4 (SPIKE-4) |

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
| `SHELFY_EGRESS_PROXY` | none | The egress proxy (§3.2: `http://shelfy-egress:4750`, from P4). Set: every outbound request goes through it, and the proxy resolves names and refuses private destinations. Unset: the server connects directly, and its own resolver refuses loopback, private, link-local (169.254.169.254 included), CGNAT, ULA, multicast and IPv4-mapped addresses. In both modes only http(s) on ports 80 and 443 is allowed, with at most 5 redirects, each checked again |
| `SHELFY_EGRESS_ALLOW_ORIGINS` | none | Exact origins `scheme://host:port`, comma-separated, of the operator's own AI node (L15), reached directly even at a private address (a Tailscale `100.64.0.0/10` address). Only the operator's AI integration uses them; URLs a user enters keep the strict rules. Never put a user-reachable service here |
| `SHELFY_CAPTURE_URL` | none | The capture service (§3.2: `http://shelfy-capture:8080`, P4): the only origin the internal client reaches, without the proxy |
| `SHELFY_OPERATOR_AI_URL` | none | The operator's own AI node (L15): an OpenAI-compatible base URL used verbatim plus the suffix (`http://<node>:8080/v1`). Its origin must also be in `SHELFY_EGRESS_ALLOW_ORIGINS`, or the start fails. Unset: the operator provider is off, and cloud keys (BYOK) are the only AI. From osn SOPS in production |
| `SHELFY_OPERATOR_AI_KEY` | none | The node's Bearer key. A secret (osn SOPS); never logged, returned or stored in a vault |
| `SHELFY_OPERATOR_AI_MODEL` | none | The text model id (`ornith-1.5-35b-a3b`). Required when `SHELFY_OPERATOR_AI_URL` is set |
| `SHELFY_OPERATOR_AI_VISION_MODEL` | none | The vision model id (`qwen3.8-27b`, L16). Without it, cataloging and screenshot QC have no operator route |
| `SHELFY_OPERATOR_AI_EMBED_MODEL` | none | The embedding model id. Optional (embeddings degrade gracefully) |
| `SHELFY_OPERATOR_AI_LABEL` | `Operator node` | The display name in `GET /me/providers` |
| `SHELFY_OPERATOR_AI_CONCURRENCY` | `1` | Calls in flight to the node, 1 or 2 (L19). The node runs each model with `--parallel 1`, so 1 leaves a slot for Hermes; 2 lets llama.cpp batch two |
| `SHELFY_OPERATOR_AI_TIMEOUT` | `60` | Seconds per operator call. The default is low for cataloging: qwen runs ~5 tok/s, so a 768-token answer takes ~150 s. SPIKE-6 (P3-10) recommends a value well above 60 for cataloging |
| `SHELFY_OPERATOR_STT_URL` | none | The node's whisper.cpp `/inference` URL (dictation). Its origin must be in `SHELFY_EGRESS_ALLOW_ORIGINS`. Needs `SHELFY_OPERATOR_AI_URL`. Unset: dictation has no operator route |
| `SHELFY_OPERATOR_STT_KEY` | none | The whisper.cpp Bearer key. A secret |
| `SHELFY_ARCHIVE_RATE_INSTAGRAM`, `SHELFY_ARCHIVE_RATE_X`, `SHELFY_ARCHIVE_RATE_PINTEREST` | `2` | Requests per second to each CDN host group (SPIKE-2), above 0 and at most 100. Raise one step per quiet breaker week (P2-20). Concurrency is 4, 8 and 4; Instagram and Pinterest fetches start after a random 120–400 ms. The hydration hosts are fixed at 1 request per 3 s (`www.instagram.com`) and 1 per second (X and Pinterest, SPIKE-9) |
| `SHELFY_ARCHIVE_MODE_INSTAGRAM`, `SHELFY_ARCHIVE_MODE_X`, `SHELFY_ARCHIVE_MODE_PINTEREST` | `server` | Who archives each platform's covers and image slides (P2-10, D14): `server` or `auto` (the server's `archive.drain`; while the host group's breaker is open the platform's items go to the extension, `archive_state = 'client'`, and come back when it closes) or `client` (the extension uploads everything). `server` everywhere follows SPIKE-2 and, for Pinterest, SPIKE-9 (L17). A change applies to the states derived from then on: restart the server, whose start-up pass derives every library again |
| `SHELFY_DEV_EGRESS_HOSTS` | none | Local runs and tests only: `host=127.0.0.1:port` pairs, comma-separated, that send those names to a loopback fixture without DNS or the address check. Refused unless `SHELFY_PUBLIC_URL` is loopback, and together with `SHELFY_EGRESS_PROXY` |
| `SHELFY_DEV_EGRESS_CA` | none | Local runs and tests only: a PEM file of extra root certificates to trust (a fixture CDN's CA). Refused unless `SHELFY_PUBLIC_URL` is loopback |
| `SHELFY_IMPORT_MAX_GB` | `10` | Largest file a user may import (a desktop JSON export or an export bundle), in GiB, 1–1024: the cap of an `import` upload (`POST /api/v1/uploads`, P4-08). A user's uploads that wait to be used (imports, bookmark files) may hold this plus 1 GiB at once; complete ones wait 24 h, under `<data>/work/uploads/` |
| `SHELFY_YTDLP_BIN` | `/opt/yt-dlp/yt-dlp` | yt-dlp, for on-demand videos (D15, L17): the image's pinned, unpacked build (L6). An absolute path. A missing binary turns the yt-dlp route off; the server still starts. Runs anonymously (no config, cookies, cache or plugins), through the egress proxy |
| `SHELFY_FFMPEG_BIN` | `/usr/bin/ffmpeg` | ffmpeg, which readies videos for the browser (`-c copy`, `+faststart` only when the index comes last) and extracts posters and keyframes: Debian's in the image (L5). An absolute path. yt-dlp and ffmpeg run two at a time, at `nice +10`, without the server's environment |
| `SHELFY_MEDIA_BUDGET_GB` | `30` | The media budget of every user library together, in GiB (§3.1): a store that would take the `users` area of the data directory past it is refused with 507 `storage_full`, for every user. 0 turns it off. See [Quotas and limits](#quotas-and-limits) |

Empty values count as unset, so a compose file may pass `SHELFY_SMTP_HOST=` when email is off.
Later tasks add the master key (§3.2, §3.4). The operator commands
(`shelfy-server admin create-owner | invite | login-link | snapshot | verify | user |
install-snapshots | migrate-token | synth | bench | flags`) use `SHELFY_DATA_DIR` too and print
their results on stdout, never to the logs.

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

## The browser extension

The extension pairs from the web app: Settings asks for a 60-second code
(`POST /api/v1/me/tokens/pairing-code`, which needs a sign-in from the last 5 minutes) and hands
it to the extension, which exchanges it at `POST /api/v1/extension/pair` for its own token. Its
requests carry `X-Shelfy-Extension: <version>`; below `minVersion`, every route but
`GET /api/v1/extension/config` answers 426 `extension_outdated`.

`GET /api/v1/extension/config` serves the minimum version, the kill switches, the pacing and the
stop thresholds. They are flags in the control database, with defaults in the code; `admin flags`
reads and changes them, and a running server applies a change within 30 seconds:

```sh
shelfy-server admin flags list
shelfy-server admin flags set extension.instagram.replay false   # a broken parser: kill switch
shelfy-server admin flags unset extension.instagram.replay        # back to the default
shelfy-server admin flags set extension.minVersion 0.3.0
```

| Flags | Default |
|---|---|
| `extension.minVersion` | `0.2.0` |
| `extension.<instagram\|twitter\|pinterest>.passive`, `.scroll`; `extension.instagram.replay` | `true` (kill switches) |
| `extension.<platform>.stopAfterKnown` | 10, 20, 25 |
| `extension.instagram.replayGapMs`, `.replayMaxPages`; `extension.<platform>.scrollSettleMs` | 700, 100; 650, 750, 650 |
| `extension.maxSteps`, `maxRunMs`, `taskPollMinutes`, `refreshPerSession` | 16,000, 1,800,000, 5, 200 |

Every value is checked against its flag's type and bounds, and every change is audited (`flag.set`,
`flag.unset`). To cut an extension off, revoke its token from Settings (or
`DELETE /api/v1/me/tokens/{id}`); pairing the same browser again also replaces its token.

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
| `shelfy_job_duration_seconds` | histogram | `kind`, `outcome` | Each ended attempt: `succeeded`, `failed` (for good), `retried`, `requeued` (again later without using a try, also while the user's library is locked), `interrupted` (shutdown), `cancelled`, `lease_expired` |
| `shelfy_disk_bytes` | gauge | `area` | Bytes of the files under `control`, `users`, `cache`, `work`, `backup_staging` and `other` (the rest of the data directory) |
| `shelfy_open_user_dbs` | gauge | — | Libraries open in the handle cache (at most 64) |
| `shelfy_sqlite_busy_total` | counter | — | SQLite calls that gave up on a lock after `busy_timeout` (5 s) |
| `shelfy_rendition_bytes` | histogram | `variant` | Size of each rendition written (`g480`), with bucket bounds at 35 and 60 KB |
| `shelfy_egress_requests_total` | counter | `purpose`, `outcome` | Outbound HTTP requests, once per request (redirects included). `purpose`: `cdn`, `link`, `ai`, `ai_operator`, `video`, `feedback`, `capture`. `outcome`: `ok`, `client_error`, `server_error`, `refused` (our policy or the proxy refused a URL, or too many redirects), `timeout`, `failed` |
| `shelfy_media_fetch_total` | counter | `host_group`, `outcome` | The archive's CDN fetches. `host_group`: `instagram`, `x`, `pinterest`, or `none` for a URL outside them. `outcome`: `stored`, `expired` (no request, or the CDN's expiry answer), `gone`, `blocked`, `transient`, `rejected`, `breaker_open` (no request) |
| `shelfy_breaker_open` | gauge | `host_group` | 1 while a host group's breaker is open or half-open, else 0, from the start: the three CDN groups and `instagram_web`, `x_web`, `pinterest_web` (link hydration) |
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
| every `/api/v1/auth/*` route but the device poll | 10 a minute | client address (`SHELFY_TRUSTED_PROXIES`; IPv6 by /64) |
| `POST /api/v1/auth/device/poll` (the migration CLI's sign-in) | 20 a minute, and `slow_down` below the 5 s interval | device code |
| `POST /api/v1/auth/device/approve` | 10 every 10 minutes, on top of the sign-in limit | user |

`/media/*`, `/health` and the web app's files are not limited.

## Quotas and limits

A user's usage is the bytes of the media objects their library records plus its database file
(§2.13); `GET /api/v1/me/usage` shows it. Every store of media (archive, bookmarks, captures,
kept videos, imports, migrations) reserves its bytes first and is refused when they do not fit;
a tus upload that will be stored is checked the same way when it is created:

| Limit | Refusal |
| --- | --- |
| the user's quota, media plus database; 0 means unlimited (the owner's) | 403 `quota_exceeded`, and a `quota.exceeded` notification at most once a day |
| `SHELFY_MEDIA_BUDGET_GB`, every library together, from the 5-minute disk sample of `users/` | 507 `storage_full`, logged at most once a minute |

Usage moves as objects are stored and as the nightly GC deletes them; the `usage.recompute` job
counts every library again at 03:00 UTC and after installs and purges, and logs any drift it
corrects. There are no admin pages (E4): the operator sets a user's limits with the CLI, and a
running server applies them at its next reservation.

```sh
shelfy-server admin user limits "$USER_ID" --quota-gb 5 --capture-daily 20
shelfy-server admin user limits "$USER_ID"     # prints them
```

`--quota-gb` takes decimals (`0.5`); 0 means unlimited for both. A change is audited as
`user.limits`.

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

**Rollbacks and schema versions** (plan §3.8). A rollback deploys the previous tag. Databases
only migrate forward: the control database at boot, each library when it is first opened or by
the sweep after boot. Migrations follow expand/contract, so an older build runs on a newer
library as is, and records that it opened it in the library's `meta` table, under
`schema.older_build`. Rolling forward again does not re-run the newer migrations, so the rows
written during the rollback miss whatever those migrations derive from the user's rows. **Rule:**
a migration that adds such derived data ships an idempotent re-derivation that the builds from
that migration on run when they open a library whose record names an older build. The first is
library v2's `posts_infix` (P1-05), re-derived by `search::index::rebuild_infix`. Nothing runs
the re-derivations from the record yet: after a rollback across library v2, run
`rebuild_infix` on the libraries that record an older build. The details are in
`crates/core/src/schema/mod.rs`.

## Synthetic libraries and the latency bench

`admin synth` fills one user's empty library with synthetic posts and media, for benchmarks and
tests (plan §6.3, App. C). `admin bench` times the read routes on a library against the §6.2
server budgets:

```sh
shelfy-server admin create-owner --email owner@example.test
shelfy-server admin synth --email owner@example.test --posts 20000 --profile reference
shelfy-server admin bench --user "$USER_ID" [--requests 400] [--strict]
```

| Command | What it does |
| --- | --- |
| `admin synth (--user ID \| --email EMAIL) --posts N [--profile reference] [--seed S]` | The `reference` profile is the reference library's shape: platform and media-type mix, two posts in three with stored media, captions about 150 topics. Renditions are real `g480` WebP files of about 25 KB; masters are sparse placeholders of realistic sizes that take almost no disk. Posts carry no remote URL, so a browser makes no third-party request. Fills an empty library only. Run it with the server stopped, or restart the server afterwards: a running server's ETags and caches do not see another process's writes |
| `admin bench --user ID [--requests N] [--strict] [--seed S]` | Runs the work of `GET /posts`, `GET /search`, `GET /posts?q=`, `GET /posts/{key}` and `GET /media/<sha>.g480.webp` after authentication, in process, one request at a time. It prints p50, p95 and p99 per route, the budget and the result, and search times per kind of query. It prints aggregates only, never a key, caption or query. `--strict` exits 1 when a route misses its budget. Use a release build or the image; it reads only |

## Moving a desktop library

`shelfy-migrate` moves a desktop library to the server (plan §4.1). It signs in with the device
flow (RFC 8628): `login` shows a code, which the owner approves on the web app's `/device` page
(after a sign-in or re-authentication in the last 5 minutes), and saves the `migrate` token,
valid 7 days, to `~/.config/shelfy-migrate/token` with mode 0600. While Cloudflare Access guards
the host, every command takes the Access service token as `--header` (G2); `--header @FILE` reads
the headers from a file, so the secret stays out of the shell history.

```sh
# Quit the desktop app first: plan and run refuse while it holds the library (exit status 4).
shelfy-migrate login "$SHELFY_PUBLIC_URL" --header @access.headers
shelfy-migrate plan --db "<userData>/shelfy.sqlite" --server "$SHELFY_PUBLIC_URL" \
  --header @access.headers --redact                    # dry run, settings and quota check
shelfy-migrate run --db "<userData>/shelfy.sqlite" --server "$SHELFY_PUBLIC_URL" \
  --header @access.headers --work-dir <scratch dir>    # add --merge if the web library has posts
```

`access.headers` (mode 0600) holds two lines, `CF-Access-Client-Id: …` and
`CF-Access-Client-Secret: …`. `<userData>` is the desktop app's data directory
(`~/Library/Application Support/Shelfy` on macOS); `--media-root` points at it when the library
file was copied elsewhere. The desktop data is only read; `--allow-open` reads a snapshot while
the app runs. Kept videos stay behind unless `--with-videos` is given. The per-user rate limit
(20 requests a second) paces the upload at about 10 objects a second; the CLI waits out each
429. An interrupted run, even a killed one, continues where it stopped when run again with the
same `--work-dir`. The server installs the bundle as a `migrate` job (2
tries, a 60-minute lease; `job.updated` on the web app): it replaces an empty web library
atomically, or merges into one with posts (`--merge`, the duplicate policy of plan §4.2). The run
ends with a reconciliation of desktop, bundle and installed counts, also stored as a
notification. The server keeps the library as it was before the install as
`library.prev-<job id>.sqlite` for 7 days. Without `login` (no browser at hand), the operator can
mint the token into a private file, `(umask 077; shelfy-server admin migrate-token --email … >
token)`, and pass `--token-file token`: the CLI refuses a token file others can read.

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

Each metric is 1 when the job's last run succeeded and 0 when it failed, also when a signal
stopped it (systemd's `TimeoutStartSec`, a stop, a reboot); `…_timestamp_seconds` is the time
of the last success, kept across failures, which is what the "backups stale" alerts of §3.6
watch (db over 3 h, media over 36 h, drill over 35 days, restic maintenance over 8 days). The
first backup creates the repository. A job refuses to run restic unless `restic.env` has mode
0600 (or 0400), and logs restic's message when it cannot open the repository; restic never
prints the password or the keys. The jobs create the textfile directory with mode 0755, which
node-exporter can read under the units' `UMask=0077`.

**Missing metrics.** A stale rule compares `time()` with a `…_timestamp_seconds` series, so it
stays silent when the series is missing: a job that never ran, a textfile directory that
node-exporter cannot read. `osn/shelfy/alerting/shelfy-backups.yaml` (P1-16) therefore also has
`shelfy_backup_metrics_missing`, an `absent(…)` rule over the db, media and drill time stamps.
The weekly restic maintenance is not in it, since its metric only appears after the first
Sunday; an unreadable directory hides it along with the three others, which the rule does see.

The operator commands behind them:

| Command | What it does |
| --- | --- |
| `admin snapshot [--out DIR] [--changed] [--user ID]…` | Copies the control database and the libraries with SQLite's online backup API (default `DIR`: `backup-staging/db`). With `--changed`, a library is copied only if its files changed since its copy was taken. Locked libraries keep their previous copy. Without `--user`, the copies of deleted accounts (gone from the control database, or being deleted) are removed, while an account whose library is missing from the data directory keeps its last copy and fails the run |
| `admin verify DIR [--user ID]… [--max-drift PERCENT]` | Checks the copies in `DIR`: `PRAGMA integrity_check`, foreign keys, the schema version, row counts against the live databases and every media reference against the live store. Row counts must be equal by default; with `--max-drift`, they may differ by that share of the larger count, and by at least 5 rows. Volatile tables such as sessions are only reported, and the live library of a locked user is not opened. Without `--user`, the set must be complete: the control database has users, each with a library copy. Exit status 1 on any problem |
| `admin user lock ID [--reason TEXT]` | Locks a user's library: their requests answer 423 `user_locked`, their jobs wait without using a try, and the server releases the library and cannot open it again until the unlock |
| `admin user unlock ID` | Unlocks it |
| `admin user restore-db ID FILE [--wait-secs N]` | Replaces a locked user's library with `FILE` after checking it, keeping a copy of the current one as `users/ID/library.pre-restore-<time>.sqlite`. Waits up to N seconds (default 120) for the server to release the library, then copies the restored pages into the live file under an exclusive lock: the file is never renamed over, so no connection can pair it with another file's log. Stray `-wal`, `-shm` or `-journal` files of a missing or unreadable library are moved aside with it. Prints the result before it writes the audit row |
| `admin install-snapshots DIR [--force]` | Full restore, with the server stopped. Checks every copy in `DIR` and that the set is complete (the control database has users, each with a library copy), stages every copy next to its target, takes every live database, then installs them and runs `verify`. A failed check, a full disk while staging or a running server installs nothing. `--force` replaces existing data, keeping a copy of each replaced database next to it |

`restic` below is restic as the jobs run it: the pinned image with `/etc/shelfy/restic.env`
(`run_restic` in `lib.sh`). Snapshot paths are the host paths.

**Restore one user to a point in time.** Pick the snapshot with `restic snapshots --host shelfy
--tag db`, and restore inside the data directory so the API container sees the file. While the
user is locked, their requests answer 423, their jobs wait (one attempt a minute, which uses no
try), and neither the server nor an operator command opens the library, not even a request or a
job that took it before the lock.

```sh
shelfy-server admin user lock "$USER_ID"
restic restore "$SNAPSHOT:/data/shelfy/backup-staging/db/users" --include "/$USER_ID.sqlite" \
  --target /data/shelfy/backup-staging/restore
shelfy-server admin user restore-db "$USER_ID" "/data/shelfy/backup-staging/restore/$USER_ID.sqlite"
shelfy-server admin user unlock "$USER_ID"
```

If `restore-db` printed "restored user …" and then failed on the audit row, the library is
restored: do not run it again.

**Restore the whole host.** After `just bootstrap`, stop the backup timers
(`systemctl stop 'shelfy-*.timer'`) and keep `shelfy-api` stopped. Restore pinned snapshots,
never `latest`: once the new host runs, its own first backups (of an empty host) become the
latest ones. Take the last db snapshot before the loss, and the last media snapshot before it:

```sh
restic snapshots --host shelfy --tag db      # pick DB_SNAPSHOT
restic snapshots --host shelfy --tag media   # pick MEDIA_SNAPSHOT, taken before DB_SNAPSHOT
restic restore "$MEDIA_SNAPSHOT:/data/shelfy/users" --target /data/shelfy/users
restic restore "$DB_SNAPSHOT:/data/shelfy/backup-staging/db" --target /data/shelfy/restore/db
```

The operator commands run in a one-off container of the stopped service, on its volume and as its
user (the image's entrypoint is `shelfy-server`), from the osn stack directory:

```sh
docker compose run --rm --no-deps shelfy-api admin install-snapshots /data/shelfy/restore/db
```

`install-snapshots` refuses a control database without users (the backup of an empty host) and a
set that lacks an account's library, and then installs nothing. Then `just deploy`, `just check`,
and start the timers again (`systemctl start 'shelfy-*.timer'`). Objects that a restored library
references but the store lost are listed by `verify` and re-archived later (P2–P4).

**Rehearsal.** `deploy/rehearse-backups.sh [WORK_DIR] [PORT]` runs all of this locally on a
synthetic library, with restic in a container and a repository in `WORK_DIR`, and fails unless
every metric reports success. `deploy/test-backup-jobs.sh` checks what the jobs record when they
fail or are stopped; run it on Linux, as the VPS does:

```sh
docker run --rm -v "$PWD:/w:ro" -w /w debian:bookworm-slim deploy/test-backup-jobs.sh
```
