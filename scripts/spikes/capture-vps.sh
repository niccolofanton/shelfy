#!/usr/bin/env bash
# SPIKE-4 and SPIKE-11 on the osn VPS (EXECUTION.md E1, E2): the capture
# container's sandbox, its egress through Smokescreen, the SSRF probe suite, and
# what one capture costs.
#
# Everything runs in throwaway containers named shelfy-spike4-* / shelfy-spike11-*
# on two dedicated networks (shelfy-spike-capture, internal; shelfy-spike-egress).
# Nothing is published, nothing of the osn stack is touched, and no host setting
# (sysctl, AppArmor) changes. `purge` removes it all.
#
# Usage:
#   Local (repo root):  scripts/spikes/capture-vps.sh stage [ssh-host]
#       builds scripts/spikes/capture-harness, cross-builds deploy/spikes/capture/egress
#       for amd64 (Docker, golang image), downloads a static ffmpeg, and copies it
#       all to <ssh-host>:/tmp/shelfy-spike4-11 (default host: vpsfant)
#   VPS (as root):      bash /tmp/shelfy-spike4-11/capture-vps.sh <command>
#       hermes                      Hermes status line, as in EXECUTION.md E2, and the load
#       pull                        pull the two images and record their sizes
#       up                          (re)create the networks, the egress proxy and the canary;
#                                   GATEWAY_MODE=default gives the capture network a host
#                                   address, as Docker does by default (isolated otherwise)
#       sandbox                     Chromium sandbox matrix (seccomp, capabilities, AppArmor)
#       probe                       SSRF probe suite + analysis -> results/probe/
#       cost <label> <url> [args]   one capture -> results/cost/<label>/ (args go to
#                                   run-site.cjs). Artifacts are written to
#                                   /var/tmp/shelfy-spike11-work/<label> and deleted
#                                   afterwards unless KEEP_ASSETS=1
#       down [--images]             remove containers and networks (and the images); keep results
#       purge                       down --images, then delete both work directories
#
# Environment (VPS): SELF_IPV4 / SELF_IPV6 (the VPS's public addresses; default:
# read from the default-route interface). They only go into the proxy's deny list
# and the probe arguments, never into the results (the probe masks them).

set -euo pipefail

D=/tmp/shelfy-spike4-11
IMG=mcr.microsoft.com/playwright:v1.60.0-noble
EGRESS_IMG=gcr.io/distroless/static-debian12:nonroot
NET_CAP=shelfy-spike-capture
NET_EGR=shelfy-spike-egress
EGRESS=shelfy-spike4-egress
CANARY=shelfy-spike4-canary
PROXY=http://$EGRESS:4750
# /tmp is a RAM-backed tmpfs on the VPS: capture outputs go to disk, as in
# production (/data/shelfy/work/capture).
WORK=/var/tmp/shelfy-spike11-work

cmd=${1:-}
shift || true

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }

# ─── Local: stage ─────────────────────────────────────────────────────────────

if [ "$cmd" = stage ]; then
  host=${1:-vpsfant}
  root=$(cd "$(dirname "$0")/../.." && pwd)
  stage=$(mktemp -d)
  trap 'rm -rf "$stage"' EXIT
  log "building the harness"
  pnpm --dir "$root/scripts/spikes/capture-harness" install --frozen-lockfile --prod >/dev/null
  node "$root/scripts/spikes/capture-harness/build.mjs" --out "$stage/harness" >/dev/null
  log "building shelfy-egress (linux/amd64)"
  docker run --rm -v "$root/deploy/spikes/capture/egress":/src:ro -v "$stage":/out -w /src \
    -e CGO_ENABLED=0 -e GOOS=linux -e GOARCH=amd64 golang:1.27.1-bookworm \
    go build -trimpath -ldflags '-s -w' -o /out/shelfy-egress . >/dev/null
  log "downloading a static ffmpeg (ffmpeg-static b6.1.1, linux-x64)"
  curl -fsSL https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1/ffmpeg-linux-x64.gz |
    gunzip >"$stage/ffmpeg"
  chmod 755 "$stage/ffmpeg" "$stage/shelfy-egress"
  cp -R "$root/deploy/spikes/capture/smokescreen" "$stage/smokescreen"
  cp "$root/deploy/spikes/capture/chromium-seccomp.json" "$root/scripts/spikes/capture-vps.sh" "$stage/"
  log "copying to $host:$D"
  tar -C "$stage" -czf - . | ssh "$host" "sudo mkdir -p $D && sudo tar -C $D -xzf - && sudo chown -R root:root $D"
  log "staged; next: ssh $host sudo bash $D/capture-vps.sh pull"
  exit 0
fi

# ─── VPS helpers ──────────────────────────────────────────────────────────────

[ "$(id -u)" = 0 ] || { echo "run as root on the VPS (sudo)" >&2; exit 1; }
mkdir -p "$D/results"

default_if() { ip route show default | awk '{print $5; exit}'; }
self_ipv4() { echo "${SELF_IPV4:-$(ip -4 -o addr show dev "$(default_if)" scope global | awk '{print $4}' | cut -d/ -f1 | head -1)}"; }
self_ipv6() { echo "${SELF_IPV6:-$(ip -6 -o addr show dev "$(default_if)" scope global | awk '{print $4}' | cut -d/ -f1 | head -1)}"; }
# The host's tailnet address (CGNAT), if Tailscale runs: a realistic internal target.
tailnet_ipv4() { ip -4 -o addr show dev tailscale0 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -1; }
# --self-ips for the probe: public IPv4, public IPv6, tailnet IPv4 (shown as <self-0..2>).
self_ips() {
  local v4 v6 ts
  v4=$(self_ipv4)
  v6=$(self_ipv6)
  ts=$(tailnet_ipv4)
  echo "$v4${v6:+,$v6}${ts:+,$ts}"
}
ip_on() { docker inspect "$1" --format "{{(index .NetworkSettings.Networks \"$2\").IPAddress}}"; }
# The host's address on a network; none for an isolated-gateway network.
gw_of() { docker network inspect "$1" --format '{{(index .IPAM.Config 0).Gateway}}' 2>/dev/null | grep -E '^[0-9.]+$' || true; }

hermes() {
  docker inspect osn-hermes-1 --format "hermes {{.State.Status}} restarts={{.RestartCount}} started={{.State.StartedAt}} mem={{.HostConfig.Memory}} cpus={{.HostConfig.NanoCpus}}"
  docker exec osn-hermes-1 python3 /opt/data/bin/node_status.py 2>&1 | tail -1
  uptime
}

# The capture container, as compose.capture.yml will run it (plan §2.18, §3.2).
# shellcheck disable=SC2054 # the commas belong to --tmpfs's options
capture_flags=(
  --network "$NET_CAP" --user 10100:10100 --read-only
  --tmpfs /tmp:size=1g,mode=1777 --shm-size 512m
  --cap-drop ALL --security-opt no-new-privileges:true
  --security-opt "seccomp=$D/chromium-seccomp.json"
  --cpus 1.5 --memory 1.5g --memory-swap 1.5g --pids-limit 1024
  --cpu-shares 256 --oom-score-adj 600
  -e HOME=/tmp -e "CAPTURE_PROXY=$PROXY" -e NODE_USE_ENV_PROXY=1
  -e "HTTP_PROXY=$PROXY" -e "HTTPS_PROXY=$PROXY"
  -e FFMPEG_BIN=/opt/ffmpeg -v "$D/ffmpeg:/opt/ffmpeg:ro"
  -v "$D/harness:/harness:ro" -w /harness
)

# Host CPU busy seconds so far (all cores), from /proc/stat.
host_busy_s() { awk '/^cpu /{print ($2+$3+$4+$7+$8+$9)/100; exit}' /proc/stat; }
# A container's cgroup (systemd driver).
cgroup_of() { echo "/sys/fs/cgroup/system.slice/docker-$(docker inspect -f '{{.Id}}' "$1").scope"; }
# Microseconds Hermes's tasks have waited for CPU since it started (PSI "some" total).
hermes_cpu_wait_us() { awk '/^some/{sub("total=", "", $5); print $5}' "$(cgroup_of osn-hermes-1)/cpu.pressure" 2>/dev/null || echo 0; }
# The proxy's cgroup: its CPU and memory peak since it started.
egress_cgroup() { cgroup_of "$EGRESS"; }
egress_cpu_us() { awk '/^usage_usec/{print $2}' "$(egress_cgroup)/cpu.stat" 2>/dev/null || echo 0; }

case "$cmd" in
hermes)
  hermes
  ;;

pull)
  docker pull -q "$IMG"
  docker pull -q "$EGRESS_IMG"
  docker images --format '{{.Repository}}:{{.Tag}} {{.ID}} {{.Size}}' | grep -E 'playwright|distroless' | tee "$D/results/images.txt"
  df -h / | tail -1
  ;;

up)
  # GATEWAY_MODE=isolated (default, as shipped): the internal network's bridge
  # gets no host address, so the host (sshd, …) is unreachable from it.
  # GATEWAY_MODE=default reproduces Docker's default internal network.
  docker rm -f "$EGRESS" "$CANARY" >/dev/null 2>&1 || true
  for n in "$NET_CAP" "$NET_EGR"; do docker network rm "$n" >/dev/null 2>&1 || true; done
  capopts=()
  [ "${GATEWAY_MODE:-isolated}" = isolated ] && capopts=(-o com.docker.network.bridge.gateway_mode_ipv4=isolated)
  docker network create --internal "${capopts[@]}" --label shelfy-spike=4-11 "$NET_CAP" >/dev/null
  docker network create --label shelfy-spike=4-11 "$NET_EGR" >/dev/null
  v4=$(self_ipv4)
  v6=$(self_ipv6)
  deny=(--deny-range "$v4/32")
  if [ -n "$v6" ]; then
    deny+=(--deny-range "$(python3 -c 'import ipaddress,sys; print(ipaddress.ip_network(sys.argv[1] + "/64", strict=False))' "$v6")")
  fi
  docker rm -f "$EGRESS" "$CANARY" >/dev/null 2>&1 || true
  # shelfy-egress: the plan's limits (§3.1), read-only, no capabilities.
  docker create --name "$EGRESS" --network "$NET_EGR" --user 10101:10101 --read-only \
    --cap-drop ALL --security-opt no-new-privileges:true --memory 96m --cpus 0.5 --pids-limit 256 \
    -e SHELFY_EGRESS_ALLOWED_PORTS=80,443 \
    -v "$D/shelfy-egress:/usr/local/bin/shelfy-egress:ro" -v "$D/smokescreen:/etc/shelfy-egress:ro" \
    "$EGRESS_IMG" /usr/local/bin/shelfy-egress --config-file /etc/shelfy-egress/config.yaml "${deny[@]}" >/dev/null
  docker network connect "$NET_CAP" "$EGRESS"
  docker start "$EGRESS" >/dev/null
  # Canary: logs any TCP/UDP that reaches it, on both networks.
  docker create --name "$CANARY" --network "$NET_CAP" --user 10100:10100 --read-only \
    --cap-drop ALL --security-opt no-new-privileges:true --memory 128m --cpus 0.25 \
    -v "$D/harness:/harness:ro" "$IMG" node /harness/ssrf-probe.mjs canary --ports 80,443,8080 --udp 53 >/dev/null
  docker network connect "$NET_EGR" "$CANARY"
  docker start "$CANARY" >/dev/null
  sleep 2
  {
    echo "PROXY_IPS=$(ip_on "$EGRESS" "$NET_CAP"),$(ip_on "$EGRESS" "$NET_EGR")"
    echo "CANARY_IPS=$(ip_on "$CANARY" "$NET_CAP"),$(ip_on "$CANARY" "$NET_EGR")"
    echo "GATEWAYS=$(gw_of "$NET_CAP"),$(gw_of "$NET_EGR"),$(gw_of bridge),$(gw_of osn_edge 2>/dev/null || true)"
  } >"$D/env"
  cat "$D/env"
  docker logs "$EGRESS" 2>&1 | tail -3
  ;;

sandbox)
  out=$D/results/sandbox
  mkdir -p "$out"
  # Variants of the shipped profile: Docker's default alone (the three rules
  # tagged "shelfy-capture:" removed), Playwright's documented addition, and
  # smaller unconditional sets.
  python3 - "$D/chromium-seccomp.json" "$out" <<'PY'
import json, sys
src, out = sys.argv[1], sys.argv[2]
d = json.load(open(src))
base = [r for r in d["syscalls"] if not str(r.get("comment", "")).startswith("shelfy-capture:")]
assert len(base) == len(d["syscalls"]) - 3, "expected three shelfy-capture rules"
def write(name, extra):
    json.dump(dict(d, syscalls=base + extra), open(f"{out}/{name}.json", "w"))
def allow(names):
    return [{"names": names, "action": "SCMP_ACT_ALLOW"}]
write("docker-default", [])
write("playwright-3", allow(["clone", "setns", "unshare"]))
write("only-clone-chroot", allow(["clone", "chroot"]))
write("only-clone-unshare-chroot", allow(["clone", "unshare", "chroot"]))
PY
  since=$(date -u '+%Y-%m-%d %H:%M:%S')
  run() { # name, then docker run flags
    local name=$1
    shift
    log "sandbox: $name"
    docker run --rm --name "shelfy-spike4-sandbox" --network none --user 10100:10100 --read-only \
      --tmpfs /tmp:size=1g,mode=1777 --shm-size 512m --security-opt no-new-privileges:true \
      --cpus 1.5 --memory 1.5g -e HOME=/tmp -v "$D/harness:/harness:ro" -w /harness "$@" "$IMG" \
      node /harness/ssrf-probe.mjs sandbox | tee "$out/$name.json" || true
  }
  run 1-default-seccomp --cap-drop ALL
  run 2-chromium-seccomp --cap-drop ALL --security-opt "seccomp=$D/chromium-seccomp.json"
  run 3-playwright-3-capdrop --cap-drop ALL --security-opt "seccomp=$out/playwright-3.json"
  run 4-playwright-3-default-caps --security-opt "seccomp=$out/playwright-3.json"
  run 5-default-seccomp-default-caps
  run 6-sandbox-off-diagnostic --cap-drop ALL -e SHELFY_DISABLE_SANDBOX=1
  run 7-only-clone-chroot --cap-drop ALL --security-opt "seccomp=$out/only-clone-chroot.json"
  run 8-only-clone-unshare-chroot --cap-drop ALL --security-opt "seccomp=$out/only-clone-unshare-chroot.json"
  docker run --rm --name shelfy-spike4-sandbox --network none --cap-drop ALL --security-opt "seccomp=$D/chromium-seccomp.json" \
    --entrypoint cat "$IMG" /proc/self/attr/current >"$out/apparmor-label.txt" 2>&1 || true
  # What the shipped profile lets any process in the container do with namespaces.
  log "namespaces under the shipped profile"
  docker run --rm --name shelfy-spike4-sandbox --network none --user 10100:10100 --cap-drop ALL \
    --security-opt no-new-privileges:true --security-opt "seccomp=$D/chromium-seccomp.json" --entrypoint sh "$IMG" -c '
      t() { if unshare --user --map-root-user "$@" true 2>/dev/null; then echo "allowed: user $*"; else echo "refused: user $*"; fi; }
      t; t --net; t --pid --fork; t --mount; t --uts; t --ipc; t --cgroup' | tee "$out/namespaces.txt"
  journalctl -k --since "$since" --no-pager 2>/dev/null | grep -E 'apparmor="DENIED"|type=1326|audit' | tail -50 >"$out/kernel-log.txt" || true
  log "AppArmor label inside the container: $(cat "$out/apparmor-label.txt")"
  log "kernel audit lines since $since: $(wc -l <"$out/kernel-log.txt")"
  ;;

probe)
  out=$D/results/probe
  mkdir -p "$out"
  chmod 777 "$out"
  # shellcheck disable=SC1091
  . "$D/env"
  hermes | tee "$out/hermes-before.txt"
  selfips=$(self_ips)
  since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  docker run --rm --name shelfy-spike4-probe "${capture_flags[@]}" -v "$out:/results" "$IMG" \
    node /harness/ssrf-probe.mjs probe --proxy "$PROXY" --out /results/results.json \
    --canary-host "$CANARY" --canary-ips "$CANARY_IPS" --proxy-ips "$PROXY_IPS" \
    --gateways "$GATEWAYS" --self-ips "$selfips" >"$out/probe.txt" || true
  docker logs --since "$since" "$EGRESS" >"$out/egress.log" 2>&1
  docker logs "$CANARY" >"$out/canary.log" 2>&1
  docker run --rm --network none -v "$out:/results" -v "$D/harness:/harness:ro" "$IMG" \
    node /harness/ssrf-probe.mjs analyze --results /results/results.json --proxy-log /results/egress.log \
    --canary-log /results/canary.log --proxy-ips "$PROXY_IPS" --self-ips "$selfips" --markdown \
    >"$out/analysis.md" || true
  # The probe masks the VPS addresses; the raw proxy log does not: mask it too.
  i=0
  for ip in ${selfips//,/ }; do
    sed -i -e "s/${ip//./\\.}/<self-$i>/g" "$out/egress.log" "$out/analysis.md" "$out/probe.txt"
    i=$((i + 1))
  done
  grep -E '^(pass|FAIL|info) ' "$out/probe.txt" | awk '{print $1}' | sort | uniq -c
  tail -40 "$out/analysis.md"
  hermes | tee "$out/hermes-after.txt"
  ;;

cost)
  label=$1
  url=$2
  shift 2
  out=$D/results/cost/$label
  work=$WORK/$label
  rm -rf "$out" "$work"
  mkdir -p "$out" "$work"
  chown 10100:10100 "$work"
  hermes >"$out/hermes-before.txt"
  others=$(docker ps --format '{{.Names}}' | grep -v -E '^(osn-|shelfy-spike4-egress|shelfy-spike4-canary)' || true)
  load0=$(cut -d' ' -f1-3 /proc/loadavg)
  busy0=$(host_busy_s)
  ecpu0=$(egress_cpu_us)
  hwait0=$(hermes_cpu_wait_us)
  since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  t0=$(date +%s.%N)
  # Load during the run, every 2 s.
  (while sleep 2; do echo "$(date +%s) $(cut -d' ' -f1 /proc/loadavg) $(docker ps -q | wc -l)"; done) >"$out/load.txt" &
  sampler=$!
  docker run --rm --name "shelfy-spike11-$label" "${capture_flags[@]}" -v "$work:/work" "$IMG" \
    node /harness/run-site.cjs --url "$url" --out /work --label "$label" "$@" >"$out/stdout.json" 2>"$out/stderr.txt" || true
  t1=$(date +%s.%N)
  kill "$sampler" 2>/dev/null || true
  busy1=$(host_busy_s)
  ecpu1=$(egress_cpu_us)
  hwait1=$(hermes_cpu_wait_us)
  epeak=$(cat "$(egress_cgroup)/memory.peak" 2>/dev/null || echo 0)
  load1=$(cut -d' ' -f1-3 /proc/loadavg)
  others1=$(docker ps --format '{{.Names}}' | grep -v -E '^(osn-|shelfy-spike4-egress|shelfy-spike4-canary)' || true)
  docker logs --since "$since" "$EGRESS" 2>&1 | grep -E 'CANONICAL-PROXY-(CN-CLOSE|DECISION)' >"$out/egress.log" || true
  hermes >"$out/hermes-after.txt"
  cp "$work"/metrics.json "$work"/manifest.json "$work"/events.ndjson "$out/" 2>/dev/null || true
  python3 - "$out" "$t0" "$t1" "$busy0" "$busy1" "$load0" "$load1" "$others" "$others1" "$ecpu0" "$ecpu1" "$epeak" "$hwait0" "$hwait1" <<'PY'
import json, sys, os
out, t0, t1, b0, b1, l0, l1, o0, o1, e0, e1, ep, hw0, hw1 = sys.argv[1:]
host = {
    "containerWallS": round(float(t1) - float(t0), 1),
    "hostBusyS": round(float(b1) - float(b0), 1),
    "loadavgBefore": l0, "loadavgAfter": l1,
    "otherContainersBefore": [x for x in o0.split() if x],
    "otherContainersAfter": [x for x in o1.split() if x],
    # How long Hermes's tasks waited for CPU during the run (cgroup PSI).
    "hermesCpuWaitMs": round((int(hw1 or 0) - int(hw0 or 0)) / 1000, 1),
}
loads = [float(l.split()[1]) for l in open(f"{out}/load.txt") if len(l.split()) > 1]
host["loadavg1Max"] = max(loads) if loads else None
pin = pout = conns = 0
for line in open(f"{out}/egress.log"):
    try:
        e = json.loads(line[line.index("{"):])
    except Exception:
        continue
    if e.get("msg") == "CANONICAL-PROXY-CN-CLOSE":
        conns += 1
        pin += int(e.get("bytes_in") or 0)
        pout += int(e.get("bytes_out") or 0)
host["proxy"] = {"connections": conns, "bytesFromInternet": pin, "bytesToInternet": pout,
                 "cpuS": round((int(e1 or 0) - int(e0 or 0)) / 1e6, 2),
                 "memPeakMiB": round(int(ep or 0) / 1048576, 1)}
m = {}
try:
    m = json.load(open(f"{out}/metrics.json"))
    host["otherCpuS"] = round(host["hostBusyS"] - (m.get("cpuS", {}).get("total") or 0), 1)
except Exception as e:
    host["error"] = str(e)
json.dump(host, open(f"{out}/host.json", "w"), indent=2)
print(json.dumps(host))
PY
  # Keep the measurements; the artifacts' sizes are in metrics.json. KEEP_ASSETS=1 keeps them.
  du -sb "$work/assets" 2>/dev/null | awk '{print "assets_bytes " $1}' >"$out/du.txt" || true
  [ "${KEEP_ASSETS:-0}" = 1 ] || rm -rf "$work"
  head -c 3000 "$out/stdout.json"
  echo
  ;;

down)
  docker ps -a --format '{{.Names}}' | grep -E '^shelfy-spike(4|11)-' | xargs -r docker rm -f >/dev/null
  for n in "$NET_CAP" "$NET_EGR"; do docker network rm "$n" >/dev/null 2>&1 || true; done
  if [ "${1:-}" = --images ]; then docker rmi "$IMG" "$EGRESS_IMG" >/dev/null 2>&1 || true; fi
  docker ps -a --format '{{.Names}}' | grep -E '^shelfy-spike' || echo "no spike containers"
  docker network ls --format '{{.Name}}' | grep -E '^shelfy-spike' || echo "no spike networks"
  ;;

purge)
  bash "$0" down --images
  rm -rf "$D" "$WORK"
  ls -d /tmp/shelfy* "$WORK" 2>/dev/null || echo "no spike files left in /tmp or /var/tmp"
  ;;

*)
  sed -n '2,30p' "$0"
  exit 1
  ;;
esac
