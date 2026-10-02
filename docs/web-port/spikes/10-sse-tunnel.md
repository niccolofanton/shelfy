# SPIKE-10 — SSE and bearer calls through Cloudflare Tunnel and nginx

**Question.** Do the realtime design of D7 / §2.10 and bearer-token API calls work on the osn path: Cloudflare edge → tunnel → edge nginx → app?

The realtime design is one SSE stream per tab, with:
- typed events carrying `id:`;
- a 20 s comment heartbeat;
- `Last-Event-ID` resume;
- `X-Accel-Buffering: no`.

Targets:
- event latency under 300 ms at p95 over at least 50 events;
- the stream outlives Cloudflare's 100 s idle timeout for at least 5 minutes;
- resume loses nothing;
- bearer calls return 200 with no challenge.

**Answer.**
- **nginx and the app pass every target that does not depend on Cloudflare.**
- **Bearer calls pass through a Cloudflare quick tunnel** with no challenge, for every client type.
- **The heartbeat keeps a stream alive past Cloudflare's idle cut-off,** which was measured.
- **SSE delivery through the tunnel could not be measured.** A Cloudflare quick tunnel buffers the whole event stream; Cloudflare's [TryCloudflare page](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/do-more-with-tunnels/trycloudflare/) states that "Quick Tunnels do not support Server-Sent Events". Event latency and resume through Cloudflare must be verified at the P1 deploy, on the real hostname and the named tunnel.

| Target | nginx only (edge mirror) | Quick tunnel → nginx | Verdict |
|---|---|---|---|
| Event latency p95 < 300 ms, ≥ 50 events | **8 ms** (p50 5 ms, 50 events) | not measurable: 0 bytes of the stream delivered | pass on nginx; **verify at P1** |
| Outlives the 100 s idle timeout, ≥ 5 min | — (nginx `proxy_read_timeout 3600s`) | heartbeat stream open **255 s and 302 s**; silent stream cut by Cloudflare at **125 s** | pass: the heartbeat defeats the cut-off; delivery itself: **verify at P1** |
| `Last-Event-ID` resume without loss | **10/10** replayed in order, 0 duplicates; unknown id → `resync` | not measurable (no delivery) | pass on nginx; **verify at P1** |
| Bearer calls 200, no challenge | 24/24 | **52/52**, 0 challenges, 4 User-Agents; 8 and 16 MiB uploads intact | pass; zone-level bot settings: **verify at P1 deploy on the real hostname** |

Run on 2026-10-02, 11:29–11:49 UTC, on the osn VPS (E1). The quick tunnel was up from 11:30 to 11:49.

**VPS impact (E2).** Hermes stayed up the whole time: running, 0 restarts, 203 → 206 MiB. Available host memory went from 6.3 to 6.26 GB. All spike containers, the network, the pulled images and `/tmp/shelfy-spike` were removed at the end.

## Setup

All parts ran as containers on the VPS, on a private bridge network `shelfy-spike-net`:
- **Limits:** every container ran with `--cpus 1 --memory 512m`.
- **Isolation:** no published ports, and the osn networks and config were untouched.
- **Cleanup:** everything was removed afterwards.

| Container | Image | Role |
|---|---|---|
| `shelfy-spike-sse` | `node:24-alpine` (Node 24.21) | `scripts/spikes/sse-server.mjs`, read-only root, all capabilities dropped, uid 1000 |
| `shelfy-spike-nginx` | `nginx:stable` (1.30.3, the same image id the osn edge runs) | `scripts/spikes/sse-nginx.conf` |
| `shelfy-spike-tunnel` | `cloudflare/cloudflared` 2026.9.3 | quick tunnel `tunnel --no-autoupdate --url http://shelfy-spike-nginx:80` (QUIC, Frankfurt PoP) |
| `shelfy-spike-tunnel2` | same | a second quick tunnel with `--protocol http2`, to rule out the transport (up for 2 minutes) |

**The test server.** It mirrors §2.10:
- `hello` on connect;
- typed events with integer `id:`;
- a comment heartbeat every 20 s;
- `Last-Event-ID` replay from a 256-event / 5-minute ring, and `resync` when it cannot fill the gap;
- `Cache-Control: no-store` and `X-Accel-Buffering: no`.

Its other endpoints:
- `POST /api/v1/emit` pushes an event to every open stream.
- `GET /api/v1/me` and `POST /api/v1/echo` are bearer-protected JSON.
- The stream accepts a cookie (as the SPA will) or a bearer token (as the extension will).

The token was random and created for the spike only.

**The nginx config.** It copies the osn edge (`/opt/osn/stack/edge/nginx.conf`, read-only):
- the same `http` block, the `set $upstream` / `proxy_pass` pattern and the same proxy headers;
- the Shelfy additions of §3.3: `client_max_body_size 20m`, `proxy_request_buffering off`, 3600 s read/send timeouts;
- like the edge, no `proxy_buffering` directive.

Only `server_name` differs.

**The client.** `scripts/spikes/sse-probe.mjs` takes every timing on one machine, so clock skew does not matter.
- **Latency** is POST `/emit` start → the event read from the open stream.
- **Survival:** a heartbeat stream and a silent control stream (`hb=0`) idle side by side.
- **Resume:** close the stream, emit 10 events, reconnect with `Last-Event-ID`, then reconnect with an unknown id.
- **Bearer calls** use four User-Agents: Chrome desktop for the extension, iOS Shortcuts (CFNetwork), `shelfy-migrate/0.1.0` for the CLI, and the Node default.

## Results

### nginx only (probe in a container on the spike network)

| Measure | Result |
|---|---|
| Stream headers / `hello` | 131 / 133 ms (first request, includes resolver lookup) |
| Event latency, 50 events at 200 ms | p50 **5 ms**, p95 **8 ms**, p99 25 ms, max 25 ms; 50/50 delivered |
| Resume after 10 missed events | ids 51–60 replayed exactly, 0 duplicates; live events after resume 2–3 ms |
| Unknown `Last-Event-ID` | `resync` (`unknown_id`) |
| Bearer calls | 24/24 `200` JSON; no token → the app's `401` JSON |
| Uploads (`proxy_request_buffering off`, 20m limit) | 8 MiB in 77 ms and 16 MiB in 70 ms, sha256 intact |
| Control without `X-Accel-Buffering: no` | still delivered in 3–4 ms |

- **The header is a guard, not a requirement here.** nginx 1.30 with the edge's config does not hold small SSE chunks even without it. Keep it: it costs nothing and protects the stream if the edge ever gains `gzip` or buffering.
- **Expected:** nginx consumes the header, so clients never see it.

### Quick tunnel → nginx (probe on the owner's Mac)

**Delivery.** The stream headers arrive in about 0.4 s: `200`, `text/event-stream; charset=utf-8`, `cf-cache-status: DYNAMIC`, chunked, no `content-encoding`. No body byte ever reached the client, in 5 attempts:
- in 3 probe runs, the `hello` never arrived;
- in 2 buffering tests (5 small events, 8 × 1 KB, 40 × 1 KB over 30 s), nginx's log shows 52 KB handed to cloudflared while the client received **0 bytes**. The result was the same over QUIC and over HTTP/2.

**Where the buffering happens.** cloudflared flushes any response that has no `Content-Length`, is chunked, or has a `text/event-stream` content type (its `shouldFlush`). So the buffering sits on the trycloudflare edge, as Cloudflare documents for quick tunnels. It says nothing about named tunnels.

**Idle survival.**

| Stream | Run 1 (240 s idle) | Run 2 (330 s idle) |
|---|---|---|
| Heartbeat 20 s | still open at 255 s; closed by the client at the end of the run | open **302 s** (15 heartbeats written); then the client closed it: undici's 300 s `bodyTimeout`, because the edge delivered nothing |
| Silent after `hello` (`hb=0`) | cut by Cloudflare at **125 s** ("stream canceled by remote") | cut by Cloudflare at **125 s** |

The idle cut-off is real (about 100 s plus slack) and counts origin activity, not delivered bytes. A heartbeat every 20 s keeps the connection open with a 5× margin.

**Bearer calls.**
- **Calls:** 52/52 returned `200` `application/json` with no challenge. Each of the 4 User-Agents made 10 × `GET /me` and 3 × `POST /echo` JSON calls.
- **Latency:** round-trip p50 43–51 ms, p95 46–55 ms, plus one 387 ms first call that included connection setup.
- **No token:** the app's own `401` JSON passed through unchanged; no Cloudflare page.
- **Uploads:** 8 MiB in 612 ms and 16 MiB in 1,082 ms, sha256 intact.

**Projected event latency.** On a tunnel that streams, a POST → event round trip is one request leg up plus one event leg down, about 1 RTT. That projects to roughly 50 ms at p95 from this connection, far inside the 300 ms target. It is a projection, not a measurement.

## Decision

D7 stands:
- **nginx:** the edge pattern needs no change for SSE.
- **App:** the app-side mechanics (`id:`, ring-buffer resume, `resync`, cookie and bearer auth) work.
- **Heartbeat:** 20 s is the right order of magnitude against Cloudflare's idle cut-off.

Not validated:
- event delivery and latency through Cloudflare;
- zone-level bot settings.

Both move to the P1 deploy.

## Follow-ups for the plan

1. **P1 relies on this spike for what it could not measure.**
   - `phases/P1.md` takes "SSE and bearer calls work through the tunnel" from T6. Its §6.2 table lists the on-VPS source of "Write → SSE p95 ≤300 ms" as "T6 (SPIKE-10); not re-measured live".
   - This spike measured that budget only on the nginx leg (8 ms p95). The Cloudflare leg is unmeasured.
   - **P1-23** should extend its SSE check ("delivers `hello` and the heartbeat") to cover:
     - **latency:** a write and its `posts.changed`, or `sse-probe.mjs` against a spike instance behind the named tunnel; p95 ≤ 300 ms over ≥ 50 events;
     - **survival:** one stream held for ≥ 5 min across an idle window over 100 s, with heartbeats arriving every 20 s;
     - **resume:** a `Last-Event-ID` reconnect with no loss.
   - The budget row should then cite P1-23.
   - **Access (G2):** while Access fronts the hostname, any probe needs the service token. `sse-probe.mjs` reads extra headers from `SPIKE_HEADERS` (`CF-Access-Client-Id: …` and `CF-Access-Client-Secret: …`, one per line), so the secret stays off the command line.
2. **Zone bot settings: verify at P1 deploy on the real hostname.**
   - Quick-tunnel hostnames are not on the owner's zone, so this spike says nothing about the `niccolofanton.dev` zone settings.
   - Bot Fight Mode applies to the whole zone. [Cloudflare's docs](https://developers.cloudflare.com/bots/get-started/bot-fight-mode/) say WAF custom rules and Page Rules can neither skip nor scope it, and that it may challenge API and mobile-app traffic. If it is on, it can challenge extension, Shortcut and CLI calls.
   - **P1-23** should run the bearer phase with the four User-Agents through Cloudflare and Access: every call must return 200 with no challenge.
   - Reading the zone's Bot Fight Mode setting needs zone permissions that App. B does not request (G9). The owner can confirm in the dashboard that it is off.
   - It must be off before P2 removes Access. Add this to the §3.3 Cloudflare checklist next to the existing bot-protection bullet.
3. **No quick tunnels for anything that streams.**
   - Dev previews and demos that rely on SSE, or on chat token streaming (§2.10), need the named tunnel or a direct path.
   - P1-24's fallback (a quick tunnel to a spike instance) is fine for passkeys. The SPA's live updates will not arrive through it.
4. **Keep the §2.10 headers as specified.** Keep `text/event-stream`, `Cache-Control: no-store` and `X-Accel-Buffering: no`, and never enable `gzip` for `text/event-stream` on the edge.
   - The `charset` parameter is harmless: cloudflared matches the content-type by prefix.
5. **Clients must expect heartbeats to arrive.**
   - Node/undici clients (tests, scripts, a future CLI stream) drop a stream after 300 s without bytes.
   - With a 20 s heartbeat that only happens when something upstream buffers. Treat it as a failure to report, not to retry silently.
6. **Uploads.** 16 MiB tus chunks pass nginx (20m) and the tunnel intact. The 100 MB zone body limit of §3.3 applies on the real hostname. P1-25's `shelfy-migrate run` through Cloudflare and Access is the natural recheck.

## Reproduce

```sh
# VPS: containers on a private network (see Setup), token in an env file
docker network create shelfy-spike-net
docker run -d --rm --name shelfy-spike-sse --network shelfy-spike-net --cpus 1 --memory 512m \
  --user 1000:1000 --read-only --env-file spike.env \
  -v "$PWD/sse-server.mjs:/app/sse-server.mjs:ro" node:24-alpine node /app/sse-server.mjs
docker run -d --rm --name shelfy-spike-nginx --network shelfy-spike-net --cpus 1 --memory 512m \
  -v "$PWD/sse-nginx.conf:/etc/nginx/nginx.conf:ro" nginx:stable
docker run -d --rm --name shelfy-spike-tunnel --network shelfy-spike-net --cpus 1 --memory 512m \
  cloudflare/cloudflared:latest tunnel --no-autoupdate --url http://shelfy-spike-nginx:80
# client
SPIKE_TOKEN=... node scripts/spikes/sse-probe.mjs --base https://<host> --out result.json
```
