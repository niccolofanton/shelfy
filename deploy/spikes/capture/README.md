# Capture and egress: deploy candidates (SPIKE-4)

Inputs for osn PR 2 in P4 (plan Appendix B): the `shelfy-capture` and `shelfy-egress`
services, the seccomp profile, and the Smokescreen settings. SPIKE-4 tested all of them
on the osn VPS (Ubuntu 26.04, kernel 7.0, Docker 29.6, AppArmor on); the results are in
[docs/web-port/spikes/04-chromium-sandbox-egress.md](../../../docs/web-port/spikes/04-chromium-sandbox-egress.md)
and the capture costs in [11-capture-cost.md](../../../docs/web-port/spikes/11-capture-cost.md).

The P4 tasks in [phases/P4.md](../../../docs/web-port/phases/P4.md) move these files to their
final places and build the two images. Nothing here runs today.

| File | What it is | Taken by | Lands in osn as |
| --- | --- | --- | --- |
| `compose.capture.yml` | The two services and both networks: plan §3.2 plus the SPIKE-4 changes (see below) | P4-28 / P4-30 (second osn PR) | services in `compose.yml` |
| `chromium-seccomp.json` | Docker 29.6's default seccomp profile plus three rules for Chromium's sandbox | P4-13, as `deploy/osn/shelfy/chromium-seccomp.json` | `/opt/osn/stack/shelfy/chromium-seccomp.json` |
| `smokescreen/config.yaml` | Smokescreen settings: listener, `network: ip4`, timeouts, deny ranges | P4-02, as `deploy/egress/` | `/opt/osn/stack/shelfy/egress/config.yaml` |
| `smokescreen/acl.yaml` | Egress ACL: default rule `open`, a deny list of internal-only names | P4-02, as `deploy/egress/` | `/opt/osn/stack/shelfy/egress/acl.yaml` |
| `egress/` | `shelfy-egress`: Smokescreen (pinned in `go.mod`) with a port allowlist, its unit tests and Dockerfile | P4-02, as `deploy/docker/shelfy-egress.Dockerfile` | the `shelfy-egress` image on GHCR |

## What SPIKE-4 changed against plan §2.18 and §3.2

1. **Seccomp profile.** Docker's default profile refuses what Chromium's sandbox needs
   once the container drops every capability. The profile adds three rules, each tagged
   `shelfy-capture:` in its `comment`:
   - `clone` and `unshare` for new user, PID and network namespaces only: the mount, UTS,
     IPC and cgroup flags stay refused (`(flags & 0x0E020000) == 0`);
   - `chroot`, which the default profile allows only with `CAP_SYS_CHROOT`.

   Playwright's documented profile (`clone`, `setns`, `unshare`) is not enough under
   `cap_drop: [ALL]`, because it lacks `chroot`; it does not need `setns`. AppArmor stays
   `docker-default`, and no host setting changes.
2. **Port allowlist.** Smokescreen has no port policy, so `egress/main.go` adds one as an
   ACL decider: `SHELFY_EGRESS_ALLOWED_PORTS`, default `80,443`. With a decider in place,
   Smokescreen also refuses every IPv6 literal destination.
3. **The VPS's own addresses.** They are public, so Smokescreen's built-in rules allow
   them. Compose adds `--deny-range` for the public IPv4 (`/32`) and the IPv6 `/64`.
4. **Isolated gateway.** On Docker's default internal network the bridge carries a host
   address, and the capture container reached the host's sshd through it.
   `com.docker.network.bridge.gateway_mode_ipv4: isolated` removes it. PR 1 created
   `osn_shelfy_capture` without this option, so PR 2 must recreate that network.
5. **Loopback.** Chromium connects to loopback directly unless the bypass list says
   `<-loopback>`. Use Playwright's `proxy` launch option, which adds it, never a bare
   `--proxy-server` flag (the plan's wording).
6. **Node's fetch.** `NODE_USE_ENV_PROXY=1` with `HTTP_PROXY` and `HTTPS_PROXY` sends the
   capture service's own requests (discovery, og:image, favicon) through the proxy with no
   code change. Leave `NO_PROXY` unset.

## Settings in `smokescreen/config.yaml`

| Setting | Value | Why |
| --- | --- | --- |
| `network` | `ip4` | The Docker networks have no IPv6; an AAAA answer can only fail, or reach something unintended after a later change |
| `allow_missing_role` | `true` | No client certificates: every request gets the ACL's default rule |
| `connect_timeout` / `idle_timeout` / `exit_timeout` | 10 s / 300 s / 15 s | `exit_timeout` fits the 20 s `stop_grace_period` |
| `deny_ranges` | 26 ranges | Repeats the built-in refusals that matter (loopback, RFC 1918, link-local, CGNAT, ULA, NAT64, 6to4, Teredo), so the policy does not depend on the Smokescreen version, and adds the special-purpose ranges Smokescreen allows (`0.0.0.0/8`, `192.0.0.0/24`, TEST-NETs, `198.18.0.0/15`, `240.0.0.0/4`, `fec0::/10`, …) |

Never add `::ffff:0:0/96` to `deny_ranges`: Go maps it to `0.0.0.0/0`, and it would refuse
every IPv4 destination.

## Build and check

```sh
# shelfy-egress image (15 MB) and its unit tests
docker build -t shelfy-egress deploy/spikes/capture/egress
docker run --rm -v "$PWD/deploy/spikes/capture/egress:/src" -w /src golang:1.27.1-bookworm go test ./...

# Re-run SPIKE-4 and SPIKE-11 on the VPS (containers named shelfy-spike4-* / shelfy-spike11-*)
scripts/spikes/capture-vps.sh stage vpsfant
ssh vpsfant sudo bash /tmp/shelfy-spike4-11/capture-vps.sh up        # then: sandbox, probe, cost …, purge
```

`scripts/spikes/ssrf-probe.mjs` is the probe suite, and `scripts/spikes/capture-harness/`
runs capture v2 outside Electron; both have usage headers.

## Rebuilding the seccomp profile

The base is `seccomp/default.json` from `github.com/moby/profiles` at `seccomp/v0.2.3`,
which Docker 29.6.0 embeds (SHA-256
`536529b665dd0972c37bfb569f5d4ac8a53592e7b00752bc39ff063ca9864c74`). To follow a Docker
upgrade, take the new `default.json` and append the three `shelfy-capture:` rules
unchanged; then re-run `capture-vps.sh sandbox`, which checks that Chromium starts with its
sandbox and that mount, UTS, IPC and cgroup namespaces stay refused.
