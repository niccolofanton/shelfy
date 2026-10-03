# SPIKE-4 — Chromium sandbox in Docker, and egress through Smokescreen

**Question.** Can the capture container (plan §2.18, §3.2) run Chromium with its sandbox on, with no `--no-sandbox`, `--privileged` or `SYS_ADMIN`, on the VPS's own OS? And does Smokescreen, as its only way out, refuse every internal destination while CONNECT and WebSockets keep working?

**Answer.** Yes to both, with no host change. Four settings differ from plan §2.18/§3.2, and all four are in the deploy candidates in [`deploy/spikes/capture/`](../../../deploy/spikes/capture/README.md):

1. **Seccomp profile.** It is Docker's default plus three rules: `clone` and `unshare` for user, PID and network namespaces only, and `chroot`. Playwright's documented profile fails once the container drops every capability.
2. **Port allowlist.** Smokescreen has no port policy. A 140-line wrapper adds one (80 and 443).
3. **The VPS's own addresses.** They are public, so Smokescreen allows them by default. They are added to its deny list.
4. **Isolated gateway.** On Docker's default internal network, the capture container still reached the host's sshd through the bridge's gateway address. `gateway_mode_ipv4: isolated` removes that address.

| Criterion (plan §9) | Result | Verdict |
|---|---|---|
| Sandbox on with the seccomp profile | Renderers run in their own user, PID and network namespaces under Chromium's seccomp-bpf filter, with `cap_drop: ALL`, `no-new-privileges`, AppArmor `docker-default`, uid 10100 and a read-only root | **pass** |
| Smokescreen: CONNECT and WebSockets work | HTTPS (CONNECT), plain HTTP, Node `fetch` and a WSS echo all work through the proxy | **pass** |
| Only ports 80 and 443 | Every other port is refused with "destination port N is not allowed" | **pass** (wrapper) |
| Every SSRF probe refused | 263 probes: 245 pass, 0 fail, 18 informational. The proxy log has 0 allowed connections to a non-public address and 0 on another port, and the canary received nothing through the proxy | **pass** (isolated gateway) |
| Same, on Docker's default internal network | 1 fail: a TCP connection from the capture container to the host's sshd at the network's gateway address | fixed by item 4 |

Run on 2026-10-02 between 23:26 and 23:38 UTC, on the osn VPS (E1):
- **Containers:** `shelfy-spike4-*`, on the dedicated networks `shelfy-spike-capture` (internal) and `shelfy-spike-egress`.
- **Exposure:** no published ports, and no osn container, network or setting was touched.
- **Hermes:** checked before and after every run: running, 0 restarts, same start time (`2026-10-02T10:11:58Z`), 2 GiB / 2 CPU, node status online.
- **Cleanup:** everything was removed at the end; see [11-capture-cost.md](11-capture-cost.md#cleanup).

## Host

| Item | Value |
|---|---|
| OS, kernel | Ubuntu 26.04 LTS, `7.0.0-31-generic`, 4 vCPU, 7.6 GiB |
| Docker | 29.6.0, cgroup v2 (systemd), containerd image store, built-in seccomp profile, AppArmor, cgroup namespaces |
| User namespaces | `kernel.unprivileged_userns_clone=1`, `user.max_user_namespaces=26835` |
| AppArmor | `kernel.apparmor_restrict_unprivileged_userns=1` and `…_unconfined=1` (Ubuntu's defaults). Container processes run under `docker-default (enforce)` |
| Images | `mcr.microsoft.com/playwright:v1.60.0-noble` (Ubuntu 24.04, Node 24.15.0, `chrome-headless-shell` 148.0.7778.96): 3.43 GB as Docker reports it, 3 GB of disk. `gcr.io/distroless/static-debian12:nonroot`: 6.2 MB |
| Proxy | Smokescreen `fa5bb56` (2026-10-01) with the wrapper in `deploy/spikes/capture/egress`, Go 1.27.1, static linux/amd64 binary of 14 MB; the image is 15 MB |

## Method

**Capture container.** It is configured as `compose.capture.yml` will run it:
- `--user 10100:10100`, `--read-only`, `--tmpfs /tmp:size=1g`, `--shm-size 512m`;
- `--cap-drop ALL`, `no-new-privileges`, the seccomp profile under test;
- `--cpus 1.5 --memory 1.5g --memory-swap 1.5g`, `--pids-limit 1024`, `--cpu-shares 256`, `--oom-score-adj 600`;
- only on `shelfy-spike-capture` (`--internal`).

Chromium was launched by Playwright 1.60 with `chromiumSandbox: true`, Playwright's `proxy` option, the capture v2 flags (SwiftShader WebGL), `--disable-quic` and `--force-webrtc-ip-handling-policy=disable_non_proxied_udp`.

**Egress.** `shelfy-spike4-egress` is on both networks:
- **Image and user:** distroless, uid 10101, read-only, no capabilities.
- **Limits:** 96 MiB, 0.5 CPU (plan §3.1).
- **Config:** [`config.yaml`](../../../deploy/spikes/capture/smokescreen/config.yaml) and [`acl.yaml`](../../../deploy/spikes/capture/smokescreen/acl.yaml), plus `--deny-range` for the VPS's public IPv4 `/32` and IPv6 `/64`.

**Canary.** `shelfy-spike4-canary` is on both networks too. It logs every TCP connection on ports 80, 443 and 8080 and every UDP datagram on port 53. Anything that reaches it through the proxy is a leak.

**Sandbox matrix.** `capture-vps.sh sandbox` runs eight launches. Each launch opens a page, draws WebGL, and reads every Chromium process from `/proc`:
- its type (`--type=`);
- whether its user, PID and network namespace links differ from the browser process's;
- whether it carries more seccomp filters than the browser process (`Seccomp_filters`; Docker's own filter is the first one).

The GPU process's state comes from Chromium itself (`SystemInfo.getInfo`, `auxAttributes.sandboxed`).

**Probe suite.** [`scripts/spikes/ssrf-probe.mjs`](../../../scripts/spikes/ssrf-probe.mjs) runs inside the capture container and sends probes three ways:
- **raw:** requests written byte by byte to the proxy, so encodings reach Smokescreen unnormalized;
- **Chromium:** navigations, WebSockets and WebRTC from pages, with the launch settings above;
- **direct:** sockets with no proxy.

It runs the full suite twice: on Docker's default internal network, then with the isolated gateway. `analyze` then checks three invariants on the proxy's own JSON log and the canary's log:
- no allowed connection to a non-public address;
- none on a port other than 80 or 443;
- no canary hit coming from the proxy.

## Results

### Sandbox matrix

All runs used AppArmor `docker-default (enforce)`, uid 10100, `no-new-privileges` and a read-only root.

| # | Seccomp profile | Capabilities | Outcome |
|---|---|---|---|
| 1 | Docker's default | none | fails: "No usable sandbox!" |
| 5 | Docker's default | Docker's default set | fails: same |
| 3 | Playwright's documented profile (default + `clone`, `setns`, `unshare`) | none | fails: `zygote_host_impl_linux.cc:221 Check failed` (no `chroot`) |
| 4 | Playwright's documented profile | Docker's default set (includes `CAP_SYS_CHROOT`) | sandbox on |
| 7 | default + `clone`, `chroot` | none | fails: "No usable sandbox!" |
| 8 | default + `clone`, `unshare`, `chroot` | none | sandbox on |
| **2** | **shipped:** default + `clone`/`unshare` for user, PID and net namespaces only + `chroot` | **none** | **sandbox on** |
| 6 | Docker's default, sandbox off (control) | none | starts; renderer in the browser's namespaces, no extra filter |

Under the shipped profile, a plain `unshare` in the container can create user, user+network and user+PID namespaces. It is refused for mount, UTS, IPC and cgroup namespaces. The kernel log has no AppArmor denial for any run.

Ubuntu restricts unprivileged user namespaces through AppArmor, yet Chromium creates them inside the container. A likely reason: Docker 29's `docker-default` template pins AppArmor ABI 3.0, which has no user-namespace mediation. So the host needs no sysctl or AppArmor change, and the fallback (hardened `--no-sandbox`) is not needed.

**Processes under the shipped profile.** `own` means that a process differs from the browser process in that respect:

| Process | Own user ns | Own PID ns | Own net ns | Chromium seccomp filter | Note |
|---|---|---|---|---|---|
| browser | — | — | — | — | runs as uid 10100 under Docker's filter and AppArmor |
| zygote ×3 | 2 of 3 | 2 of 3 | 2 of 3 | — | the third is Chromium's unsandboxed zygote, which starts the GPU process |
| renderer | yes | yes | yes | yes | where page content runs |
| GPU process | — | — | — | — | Chromium reports `sandboxed: false` (SwiftShader WebGL) |
| network service | — | — | — | — | started with `--service-sandbox-type=none` (Chromium on Linux) |

The GPU process runs WebGL in SwiftShader, without its own sandbox. In a local test, forcing that sandbox on (`--gpu-sandbox-start-early`) turned WebGL off (`gl=disabled`), which would blank the WebGL heroes capture v2 exists for. So a renderer exploit that also breaks the GPU process lands in the container, which is the boundary below.

### SSRF probes (isolated gateway, the shipped configuration)

The suite took 70 s, with 263 probes. Refusal reasons are Smokescreen's own: `address: …` is the rule that refused the resolved address.

| Cat | Probes | What | How it was refused | Verdict |
|---|---|---|---|---|
| A | 6 | controls: `CONNECT example.com:443`, `GET http://example.com/`, Chromium on https and http, Node `fetch` through `NODE_USE_ENV_PROXY`, a WSS echo | all succeed | pass |
| B | 56 | 17 reserved addresses, 3 host bridge addresses (spike egress network, `docker0`, `osn_edge`), the proxy's 2 addresses, the canary's 2. Each as HTTP and as CONNECT, 7 also from Chromium, plus `CONNECT 127.0.0.1:4750` | 407: `User Configured` (the deny list), `Not Global Unicast`, `Self Connection`, port rule | pass |
| C | 33 | 15 IPv6 literals: loopback, unspecified, IPv4-mapped (dotted and hex, incl. metadata and RFC 1918), IPv4-compatible, link-local, ULA, NAT64 incl. local-use, 6to4, Teredo, site-local. HTTP and CONNECT, 3 also from Chromium | any IPv6 literal (with an ACL decider in place, Smokescreen refuses them all), or the address rule | pass |
| D | 30 | `2130706433`, `0x7f000001`, `017700000001`, `0177.0.0.1`, `127.1`, `0x7f.1`, decimal and octal metadata, hex RFC 1918, zero-padded. Raw and from Chromium | Chromium canonicalizes, then the address rule. Raw: Smokescreen treats them as names and DNS fails | pass |
| E | 37 | names resolving inward: `*.nip.io` (loopback, metadata, 10/8, 192.168/16, CGNAT, `docker0`, `0.0.0.0`, the canary), `localtest.me`, `--1.sslip.io`, `localhost`, `*.localhost`, `metadata.google.internal`, `host.docker.internal`, `ip6-localhost`, a container name | the address rule after resolution; the ACL deny list for internal names; `ip6-localhost` has no A record (`network: ip4`) | pass |
| F | 8 | `httpbin.org` 302 to metadata, `https://127.0.0.1`, `10.0.0.1` and the canary, from Chromium and from Node `fetch` | 407 at the redirect hop; Chromium shows `ERR_PROXY_AUTH_UNSUPPORTED` for the https hop | pass |
| G | 10 | CONNECT to 8443, 22, 25, 6379, 4750 and 53; HTTP to 8080 and 81; Chromium to 8080 and 8443 | "destination port N is not allowed" | pass |
| H | 14 | `ws://`/`wss://` from a Chromium page to loopback (the proxy port and a local listener), `localhost`, metadata, 10/8, `[::1]`, a nip.io loopback name, the canary (4 ways), its container name, a host bridge address, and a public host on 8080 | every socket closed with 1006; nothing reached the canary | pass |
| I | 14 | the VPS's public IPv4 and IPv6 and its tailnet IPv4: HTTP, CONNECT to 443 and 22, Chromium, nip.io | `User Configured` (compose's deny ranges; CGNAT for the tailnet), the port rule, the IPv6 literal rule | pass |
| J | 37 | direct, no proxy: TCP to `1.1.1.1:443`, `8.8.8.8:53`, `example.com:80` and the VPS's public and tailnet addresses; UDP DNS to `8.8.8.8`; an external name through Docker's resolver; 3 host bridge addresses × 8 ports (22, 53, 80, 443, 2375, 4750, 8080, 9100); WebRTC with a STUN server; Chromium to a listener on the container's own `127.0.0.1`, `localhost` and `[::1]` | `ENETUNREACH`, `EAI_AGAIN`; Docker's resolver answers `SERVFAIL` for external names; 0 WebRTC candidates; loopback goes through the proxy and gets a 407 | pass (2 info) |
| K | 16 | DNS rebinding: `rbndr.us` and `1u.ms` names that alternate between 127.0.0.1 and 1.1.1.1 | `rbndr.us` did not resolve from the VPS, and every `1u.ms` answer was 1.1.1.1 (resolver caching), so no rebind happened | info |
| L | 2 | CONNECT with `X-Upstream-Https-Proxy` (private, then public) | "connect proxy host not allowed in rule" | pass |

**Cross-checks.**
- **Proxy log:** it recorded 236 decisions. Every connection the proxy actually opened went to a public address on port 80 or 443. The log also marks `allow: true` on requests the ACL let through but whose name then failed to resolve (the raw encodings of D, `rbndr.us`); no connection was opened for those.
- **Canary:** it saw exactly one connection, from the informational `lateral-canary` probe: a direct TCP connection from the capture container to a peer on its own network, reachable by design. Nothing reached it through the proxy.

**Rebinding.** The rebinding probes saw no flip, but Smokescreen resolves once, checks the address and dials that same address (`dialContext` uses `Decision.ResolvedAddr`). A name that changes its answer between the check and the dial therefore cannot reach an internal address.

**Informational probes:**
- `lateral-canary`: a peer on the capture network is reachable, by design. In production that peer is `shelfy-api` (see input 6).
- `loopback-control`: Chromium launched with a bare `--proxy-server` flag, as plan §2.18 words it, did reach the listener on its own `127.0.0.1`. With Playwright's `proxy` option, which adds `--proxy-bypass-list=<-loopback>`, the same navigation got a 407.

### Docker's default internal network (first run)

On the same suite, the only failure was `J/gw-0-22`: a TCP handshake from the capture container to `172.19.0.1:22`. The internal network's bridge carries the host's gateway address. ufw drops every other port there, but it allows SSH, so the container reached the host's sshd. Every other port on that address timed out.

The real `osn_shelfy_capture` network (`172.18.0.0/16`, gateway `172.18.0.1`) has the same layout. Creating the network with `com.docker.network.bridge.gateway_mode_ipv4=isolated` leaves the bridge without an address, and every host address became unreachable (`ENETUNREACH`); the probe then passed. Containers on the network still reach each other, and Docker's DNS still resolves `shelfy-spike4-egress`.

### Resources

Measured on the proxy's cgroup, across the last probe run and all 35 SPIKE-11 captures (2.3 h, 2.45 GB relayed over 459 connections):

| `shelfy-spike4-egress` | Idle | Under load |
|---|---|---|
| Memory | 5 MiB | 22 MiB peak (`memory.peak`), against the 96 MiB limit |
| CPU | 0 % | under 18 CPU-s in total: about 7 CPU-s per GB relayed, against the 0.5 CPU quota |

## Decisions

- **D16 and D17 stand.** Chromium keeps its sandbox. The plan's fallback (hardened `--no-sandbox`) is not needed, and neither is a host change.
- **Seccomp:** ship `chromium-seccomp.json`. This is Docker 29.6's default profile plus `clone`/`unshare` for user, PID and network namespaces only, and `chroot`. It replaces the plan's "Playwright's seccomp profile", which fails under `cap_drop: [ALL]` and allows more than needed (`setns`, every namespace type).
- **Proxy:** Smokescreen `fa5bb56` plus the port-allowlist wrapper (`deploy/spikes/capture/egress`), as `shelfy-egress`. The configuration is the committed `config.yaml` and `acl.yaml`, with the VPS's own addresses added from the osn environment.
- **Network:** `shelfy_capture` is `internal: true` with `gateway_mode_ipv4: isolated`.
- **Chromium:** launch it through Playwright's `proxy` option, never with a bare `--proxy-server` flag. Add `--disable-quic` and `--force-webrtc-ip-handling-policy=disable_non_proxied_udp`.
- **Node:** `NODE_USE_ENV_PROXY=1` with `HTTP_PROXY`/`HTTPS_PROXY` and no `NO_PROXY`. Capture v2's own fetches go through the proxy with no code change.

**Fallbacks, if a future Docker or Ubuntu breaks the sandbox:**
1. Re-derive the profile from the new Docker default; `capture-vps.sh sandbox` checks it.
2. Give the container a custom AppArmor profile that allows `userns`. That is a host change, so it is a lead decision.
3. Only then, `--no-sandbox` with everything else unchanged.

## Inputs for P4

Mapped onto the tasks of [phases/P4.md](../phases/P4.md):

1. **P4-02 (egress proxy).** Take `deploy/spikes/capture/egress/` as `deploy/docker/shelfy-egress.Dockerfile` plus its sources, and `smokescreen/` as `deploy/egress/`.
   - **Image:** a plain Smokescreen build cannot meet "ports 80/443 only": the wrapper is the port policy. Its image is 15 MB.
   - **Test configuration:** production plus `--allow-range` for the fixture subnet.
   - **Probes:** reuse `ssrf-probe.mjs`'s categories. The suite's test resolver adds what this spike could not control: a rebinding name that really flips, and `localhost.` with a trailing dot.
   - **CI:** the raw-proxy categories (B, C, D, G, L) need no DNS beyond Docker's.
2. **P4-03 (capture service).** Its acceptance says "Chromium starts with `--proxy-server`". Change it to Playwright's `proxy` launch option, or add `--proxy-bypass-list=<-loopback>` next to the flag. A bare flag lets pages reach the container's own loopback, including the service's port 8080 (probe `loopback-control`).
   - **Node:** `NODE_USE_ENV_PROXY=1` covers every Node fetch.
   - **Sandbox:** in production it must not depend on `SHELFY_DISABLE_SANDBOX`.
3. **Errors (P4-03).** A refused destination reaches Chromium in two forms. Map all of them to one capture failure code (for example `capture_refused_destination`), not to a generic navigation error:
   - **plain HTTP:** HTTP 407 with an `X-Smokescreen-Error` header;
   - **HTTPS:** `ERR_PROXY_AUTH_UNSUPPORTED` or `ERR_TUNNEL_CONNECTION_FAILED`;
   - **Node's `fetch`:** a 407 response, or `fetch failed`.
4. **P4-13 (capture image and isolation).** Take `chromium-seccomp.json` as `deploy/osn/shelfy/chromium-seccomp.json`, and give `shelfy_capture` the isolated gateway in `compose.test` too.
   - **Isolation check:** without the isolated gateway, `capture-isolation-check.sh`'s "no route to the host" fails, because sshd answers on the gateway address.
   - **Sandbox mode:** the check script can read it from `/proc`, as `ssrf-probe.mjs sandbox` does. With the sandbox on, the renderer runs in its own user, PID and net namespaces, under an extra seccomp filter.
   - **Lateral reach:** the capture container reaches `shelfy-api:8080` and `:9464` directly on `shelfy_capture` (probe `lateral-canary`). PG14 (the API refuses peers in `SHELFY_CAPTURE_SUBNET`) closes that.
5. **P4-28 and P4-30 (second osn PR).** Take `compose.capture.yml`.
   - **Network:** PR 1 created `osn_shelfy_capture` without the isolated gateway, so PR 2 must recreate it, which detaches `shelfy-api` from it for a moment.
   - **Environment:** add `OSN_PUBLIC_IPV4` and `OSN_PUBLIC_IPV6_PREFIX` to osn's `.env`.
6. **Proxy observability (P4-28's alert rules).** Smokescreen writes one JSON `CANONICAL-PROXY-DECISION` line per request, with `allow`, `decision_reason`, `requested_host` and the outbound address.
   - **Alert:** on any connection to a non-public address; the probe's `analyze` mode already implements the check.
   - **Denial rate:** count the 407s.
   - **Prometheus:** its metrics would need the proxy on the `internal` network; skip them unless needed.
7. **Residual risk, for §7.1.**
   - **User namespaces** inside the capture container widen the kernel surface for code that already runs there. That is the price of the renderer sandbox.
   - **Unsandboxed processes:** the GPU process (SwiftShader) and the network service run without Chromium's own sandbox. The container is their boundary: non-root, no capabilities, seccomp, AppArmor, read-only, isolated network.
   - **Chromium:** keep it current; it is pinned through Playwright.
8. **Upgrades.** When the host's Docker is upgraded, re-derive the seccomp profile and re-run `capture-vps.sh sandbox` and `probe`.

## Limits

- **One host, one moment:** the results cover Docker 29.6.0, kernel 7.0 and Chromium 148. A Docker or Ubuntu upgrade can change the user-namespace behaviour.
- **Rebinding:** not observed in practice (see K); the guarantee rests on Smokescreen's code path.
- **IPv6:** the Docker networks have no IPv6, so IPv6 was tested as literals and names only.
- **GPU sandbox finding:** the WebGL result with the GPU sandbox forced on comes from a local Docker Desktop run, not from the VPS.

## Reproduce

```sh
scripts/spikes/capture-vps.sh stage vpsfant                          # build, copy to /tmp/shelfy-spike4-11
ssh vpsfant sudo bash /tmp/shelfy-spike4-11/capture-vps.sh pull
ssh vpsfant sudo bash /tmp/shelfy-spike4-11/capture-vps.sh up        # GATEWAY_MODE=default for Docker's default
ssh vpsfant sudo bash /tmp/shelfy-spike4-11/capture-vps.sh sandbox   # results/sandbox/*.json
ssh vpsfant sudo bash /tmp/shelfy-spike4-11/capture-vps.sh probe     # results/probe/{probe.txt,analysis.md}
ssh vpsfant sudo bash /tmp/shelfy-spike4-11/capture-vps.sh purge
```
