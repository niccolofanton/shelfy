# egress

The egress proxy's runtime policy and the SSRF suite (plan §2.18, §3.2, §6.1,
§7.1; SPIKE-4). The image is built from
[`../docker/shelfy-egress.Dockerfile`](../docker/shelfy-egress.Dockerfile)
(Smokescreen pinned to `fa5bb56` with the port-allowlist wrapper, ~15.5 MB on
`distroless/static:nonroot`, uid 10101). These files are what it reads and what
the suite exercises; in osn they land under `/opt/osn/stack/shelfy/egress/`
(second osn PR, P4-28/P4-30).

| File | What |
| --- | --- |
| `config.yaml` | Smokescreen settings: listener `:4750`, `network: ip4` (the Docker networks have no IPv6), timeouts, and the 26 `deny_ranges` that refuse every non-public destination after DNS resolution. Mounted at `/etc/shelfy-egress/config.yaml` |
| `acl.yaml` | The egress ACL: default rule `open` (the capture service fetches arbitrary public sites; the address policy is the control), a `global_deny_list` of internal-only name suffixes, and no `allowed_external_proxies` (so proxy chaining is refused). Mounted at `/etc/shelfy-egress/acl.yaml` |
| `ssrf/` | The SSRF suite: the test resolver, the fixture/redirect origin and the probe list. See below |

## Arguments

The wrapper takes Smokescreen's flags; the config file holds the standing
policy, the command line adds what is environment-specific.

- **Production** (osn `compose.yml`, P4-30):

  ```
  --config-file=/etc/shelfy-egress/config.yaml
  --deny-range=${OSN_PUBLIC_IPV4}/32
  --deny-range=${OSN_PUBLIC_IPV6_PREFIX}
  ```

  The VPS's own public addresses are global unicast, so Smokescreen would allow
  them; the two `--deny-range` flags refuse them (SPIKE-4). `OSN_PUBLIC_IPV4`
  and `OSN_PUBLIC_IPV6_PREFIX` come from osn's `.env`. Ports are fixed to
  `80,443` by the wrapper (`SHELFY_EGRESS_ALLOWED_PORTS`); **production never
  passes `--allow-range`.**

- **Test** (`../compose.test.yml`, the `ssrf` CI job): production plus one flag,

  ```
  --allow-range=10.133.0.0/24
  ```

  the fixture subnet, so the positive controls have a destination to reach. The
  subnet is inside the proxy's `10.0.0.0/8` deny range, so `--allow-range`
  (which takes precedence) is what lets exactly the fixtures through while every
  other private address stays refused.

## The SSRF suite (`ssrf/`)

`crates/server/tests/ssrf.rs` drives [`ssrf/probes.json`](ssrf/probes.json)
through the API's real outbound client (`crates/server/src/outbound`, L11)
against a live proxy, and proves both directions: every probe of §6.1 is
refused and the positive controls pass. The probe categories reuse
`scripts/spikes/ssrf-probe.mjs` (A controls, B direct IPv4, C IPv6 literals, D
encodings, E inward names, F redirects, G ports, H WebSocket/CONNECT, I the API
and self addresses, K rebinding, L proxy chaining). P4-13's
`capture-isolation-check.sh` reuses the same list from inside the capture
container.

| File | What |
| --- | --- |
| `Corefile`, `hosts` | A CoreDNS resolver (pinned by digest): it answers the fixtures on the allowed subnet and the names that resolve inward (loopback, RFC 1918, link-local, CGNAT, a public-but-deny-listed address, an AAAA-only name), and NXDOMAIN for everything else, so no probe reaches a real network |
| `fixture.conf` | The nginx fixture (pinned by digest): `GET /` answers 200 (the positive control, and `:443` accepts TCP for the CONNECT control), and `/to-*` are the redirect origin of category F, 302-ing to private targets |
| `probes.json` | The probe list: each entry is `refuse` or (category A) `allow`, with where the refusal is expected (`client` — the outbound client's own URL checks — or `proxy`) |

Run it locally (Docker required; the API image is **not** needed):

```sh
docker build -f deploy/docker/shelfy-egress.Dockerfile -t shelfy-egress:local .
docker compose -f deploy/compose.test.yml -p shelfy-ssrf up -d --wait shelfy-ssrf-egress
SHELFY_SSRF_PROXY=http://127.0.0.1:4750 cargo test -p shelfy-server --test ssrf -- --nocapture
docker compose -f deploy/compose.test.yml -p shelfy-ssrf down -v
```

Without `SHELFY_SSRF_PROXY` the test skips, so `cargo test --workspace` does not
need Docker. The CI `ssrf` job (`.github/workflows/ci.yml`) runs exactly these
steps on every push to `web/**`.

## Logging (§3.7)

Smokescreen writes one `CANONICAL-PROXY-DECISION` JSON line per request with
`allow`, `decision_reason`, `requested_host` and `outbound_remote_addr` — the
**host only, never the path or query string**, so post URLs (user data) never
reach the proxy log. The per-request id correlates the decision with its
`CANONICAL-PROXY-CN-CLOSE`. Alert on any `allow:true` line whose
`outbound_remote_addr` is not a public address, and on the 407 rate
(`scripts/spikes/ssrf-probe.mjs analyze` implements both checks); Prometheus
metrics would need the proxy on the `internal` network and are skipped unless
needed (SPIKE-4 input 6).
