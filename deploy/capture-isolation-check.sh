#!/bin/sh
# Inside the image: probe the configured runtime. On the host: preflight Docker
# and exec the same probe in an already healthy compose.test capture stack.
set -eu
if [ -f /app/isolation-check.mjs ]; then
  exec node /app/isolation-check.mjs "$@"
fi
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
major=$(docker version --format '{{.Server.Version}}' | cut -d. -f1)
case "$major" in ''|*[!0-9]*) echo 'Cannot determine Docker Engine version' >&2; exit 1;; esac
if [ "$major" -lt 28 ]; then
  echo 'Docker Engine >= 28 is required: older internal bridges can reach the host' >&2
  exit 1
fi
compose_file="$script_dir/compose.test.yml"
capture_id=$(docker compose -f "$compose_file" --profile capture ps -q shelfy-capture)
[ -n "$capture_id" ] || { echo 'Start the capture profile first' >&2; exit 1; }
# Reject additional network attachments and a missing isolated gateway setting.
network=$(docker inspect --format '{{range $name, $config := .NetworkSettings.Networks}}{{$name}} {{end}}' "$capture_id")
set -- $network
[ "$#" -eq 1 ] || { echo 'Capture must have exactly one network' >&2; exit 1; }
[ "$(docker network inspect --format '{{.Internal}}/{{index .Options "com.docker.network.bridge.gateway_mode_ipv4"}}' "$1")" = true/isolated ] || {
  echo 'Capture network must be internal with isolated gateway mode' >&2; exit 1;
}
exec docker compose -f "$compose_file" --profile capture exec -T shelfy-capture /app/capture-isolation-check.sh
