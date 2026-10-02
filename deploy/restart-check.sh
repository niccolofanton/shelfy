#!/usr/bin/env bash
# Restarts shelfy-api in the test stack and checks that GET /health answers 200
# within the budget of plan §6.2, "API restart to healthy ≤ 3 s". The time runs
# from the restart command to the first 200: graceful stop, start, and the
# server's boot together.
#
#   docker compose -f deploy/compose.test.yml up -d --wait
#   deploy/restart-check.sh
#
# Settings, from the environment:
#   COMPOSE_FILE              the stack (default: compose.test.yml next to this script)
#   SHELFY_TEST_PORT          the API's loopback port, as in the stack (default 8081)
#   SHELFY_TEST_HEALTH_URL    the probed URL (default http://127.0.0.1:<port>/health)
#   SHELFY_RESTART_BUDGET_MS  the budget (default 3000)
#
# Exits 0 within the budget, 1 over it or when the API does not come back.
# Works with macOS's bash 3.2: milliseconds come from perl when bash has no
# EPOCHREALTIME.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
compose_file="${COMPOSE_FILE:-$here/compose.test.yml}"
service=shelfy-api
url="${SHELFY_TEST_HEALTH_URL:-http://127.0.0.1:${SHELFY_TEST_PORT:-8081}/health}"
budget_ms="${SHELFY_RESTART_BUDGET_MS:-3000}"
give_up_ms=30000

now_ms() {
  if [[ -n "${EPOCHREALTIME:-}" ]]; then
    local micros="${EPOCHREALTIME/[.,]/}"
    echo $((10#$micros / 1000))
  else
    perl -MTime::HiRes=time -e 'printf "%d\n", time() * 1000'
  fi
}

status() {
  curl --silent --output /dev/null --write-out '%{http_code}' --max-time 1 "$url" || true
}

code="$(status)"
if [[ "$code" != 200 ]]; then
  echo "error: $url answers ${code:-nothing} before the restart; start the stack first" >&2
  exit 1
fi

start="$(now_ms)"
docker compose -f "$compose_file" restart "$service" >/dev/null 2>&1
restarted="$(now_ms)"
until [[ "$(status)" == 200 ]]; do
  if (($(now_ms) - start > give_up_ms)); then
    echo "error: $url did not answer 200 within $((give_up_ms / 1000)) s of the restart" >&2
    exit 1
  fi
  sleep 0.05
done
healthy="$(now_ms)"

total=$((healthy - start))
echo "restart to healthy: ${total} ms (docker compose restart $((restarted - start)) ms," \
  "then 200 after $((healthy - restarted)) ms); budget ${budget_ms} ms"
if ((total > budget_ms)); then
  echo "error: over the ${budget_ms} ms budget" >&2
  exit 1
fi
