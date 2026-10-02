#!/usr/bin/env bash
#
# Local rehearsal of the Shelfy backups and restores (plan §3.5), on
# synthetic data, with restic in a container and a local repository: nothing
# leaves the machine.
#
#   deploy/rehearse-backups.sh [WORK_DIR] [PORT]
#
# 1. builds shelfy-server and seeds a synthetic data directory (two users,
#    7,500 posts, 160 stored images);
# 2. starts the server on PORT (default 18192), signs in and opens a library,
#    so the snapshots copy databases the server holds open;
# 3. runs the four jobs of deploy/osn/shelfy/backup/ (db twice, to show
#    `--changed`), with their textfile metrics in WORK_DIR/textfile;
# 4. restores one user to a point in time (lock, 423, restic restore,
#    restore-db, unlock);
# 5. restores the whole host into a new data directory (restic restore of both
#    sets, install-snapshots, a server on it);
# 6. fails unless every backup metric reports success.
#
# WORK_DIR (default ../shelfy-web-local/data/p1-12/rehearsal, next to the
# repository) is wiped first. Needs cargo, Docker, curl and sqlite3.

# shellcheck source-path=SCRIPTDIR
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
work=${1:-$repo/../shelfy-web-local/data/p1-12/rehearsal}
port=${2:-18192}
mkdir -p "$work"
work=$(cd "$work" && pwd)
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$repo/target}
bin=$CARGO_TARGET_DIR/debug/shelfy-server
url=http://localhost:$port
backup=$repo/deploy/osn/shelfy/backup

step() {
  printf '\n== %s\n' "$*" >&2
}

fail() {
  printf 'REHEARSAL FAILED: %s\n' "$*" >&2
  exit 1
}

server_pid=
stop_server() {
  if [ -n "$server_pid" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
    server_pid=
  fi
}
trap stop_server EXIT

# start_server DATA_DIR: runs the server in the background until /health answers.
start_server() {
  "$bin" serve --data-dir "$1" --listen "127.0.0.1:$port" \
    --metrics-listen "127.0.0.1:$((port + 1000))" --public-url "$url" \
    --log-format text >>"$work/server.log" 2>&1 &
  server_pid=$!
  local tries=0
  until curl -fs -o /dev/null "$url/health"; do
    tries=$((tries + 1))
    [ "$tries" -lt 100 ] || fail "the server did not start: see $work/server.log"
    sleep 0.1
  done
}

# sign_in DATA_DIR: prints a session cookie of the owner.
sign_in() {
  local link token
  link=$("$bin" admin --data-dir "$1" login-link --public-url "$url" \
    --email owner@example.test | sed -n 2p)
  token=${link#*#}
  curl -fsS -D - -o /dev/null -X POST "$url/api/v1/auth/magic-links/redeem" \
    -H 'Content-Type: application/json' -H "Origin: $url" -H 'X-Shelfy-Client: web' \
    -d "{\"token\":\"$token\"}" |
    sed -n 's/^[Ss]et-[Cc]ookie: __Host-shelfy_session=\([^;]*\);.*/\1/p'
}

# posts_total COOKIE: the owner's post count through the API, or the HTTP status.
posts_total() {
  local body status
  body=$(curl -sS -w '\n%{http_code}' -H "Cookie: __Host-shelfy_session=$1" \
    "$url/api/v1/posts?limit=1&includeTotal=true")
  status=${body##*$'\n'}
  if [ "$status" = 200 ]; then
    printf '%s\n' "$body" | sed -n 's/.*"total":\([0-9]*\).*/\1/p'
  else
    printf 'HTTP %s\n' "$status"
  fi
}

step "build shelfy-server"
cargo build --locked --quiet --package shelfy-server

step "synthetic data directory in $work"
rm -rf "$work"
mkdir -p "$work/textfile" "$work/restic-repo" "$work/restic-cache"
data=$work/shelfy
SHELFY_REHEARSAL_DATA_DIR=$data cargo test --locked --quiet --package shelfy-server \
  --test backup -- --ignored --exact seed_rehearsal_library --nocapture 2>&1 |
  grep -E '^seeded' >&2
owner=$(sqlite3 "$data/control/control.sqlite" "SELECT id FROM users WHERE role = 'owner'")
member=$(sqlite3 "$data/control/control.sqlite" "SELECT id FROM users WHERE role = 'member'")

step "server on $url, signed in, the owner's library open"
start_server "$data"
cookie=$(sign_in "$data")
[ -n "$cookie" ] || fail "no session cookie"
before=$(posts_total "$cookie")
echo "owner $owner: $before posts through the API" >&2

# The jobs, configured for this machine: the admin command is the local
# binary, restic's repository a local directory mounted into its container.
umask 077
printf 'RESTIC_REPOSITORY=/repo\nRESTIC_PASSWORD=%s\n' \
  "$(od -An -tx1 -N24 /dev/urandom | tr -d ' \n')" >"$work/restic.env"
umask 022
export SHELFY_DATA_DIR=$data
export SHELFY_STAGING_DIR=$data/backup-staging
export SHELFY_TEXTFILE_DIR=$work/textfile
export SHELFY_ADMIN="nice -n 19 $bin admin"
export SHELFY_RESTIC_ENV_FILE=$work/restic.env
export SHELFY_RESTIC_CACHE_DIR=$work/restic-cache
export SHELFY_RESTIC_DOCKER_ARGS="--volume $work/restic-repo:/repo"

step "shelfy-db-snapshot.sh (first run: everything is copied)"
# Files written in the last 2 s are copied again on the next run (the
# snapshot's guard against clock ticks): let the seeded ones settle first.
sleep 3
"$backup/shelfy-db-snapshot.sh"
step "shelfy-db-snapshot.sh (second run: only the control database is copied)"
"$backup/shelfy-db-snapshot.sh"
step "shelfy-media-backup.sh"
"$backup/shelfy-media-backup.sh"
step "shelfy-restic-maintenance.sh"
"$backup/shelfy-restic-maintenance.sh"
step "shelfy-restore-drill.sh"
"$backup/shelfy-restore-drill.sh"

# shellcheck source=osn/shelfy/backup/lib.sh
. "$backup/lib.sh"

step "restore one user to a point in time"
library=$data/users/$owner/library.sqlite
sqlite3 "$library" "PRAGMA foreign_keys = ON;
  DELETE FROM posts WHERE id IN (SELECT id FROM posts ORDER BY id LIMIT 100);"
echo "a mistake deleted 100 posts: $(posts_total "$cookie") posts through the API" >&2
"$bin" admin --data-dir "$data" user lock "$owner" --reason rehearsal
locked=$(posts_total "$cookie")
echo "while locked, the API answers: $locked" >&2
[ "$locked" = "HTTP 423" ] || fail "a locked user got $locked"
restore_dir=$data/backup-staging/restore-one
mkdir -p "$restore_dir"
RESTIC_MOUNTS=(--volume "$restore_dir:$restore_dir")
run_restic restore "latest:$SHELFY_STAGING_DIR/db/users" --host "$SHELFY_RESTIC_HOST" \
  --tag db --target "$restore_dir" --include "/$owner.sqlite"
"$bin" admin --data-dir "$data" user restore-db "$owner" "$restore_dir/$owner.sqlite"
"$bin" admin --data-dir "$data" user unlock "$owner"
after=$(posts_total "$cookie")
echo "after the restore: $after posts through the API" >&2
[ "$after" = "$before" ] || fail "the restore brought back $after posts, not $before"
rm -rf "$restore_dir"

step "restore the whole host into a new data directory"
stop_server
host=$work/restored-host
mkdir -p "$host/users" "$work/restored-db"
RESTIC_MOUNTS=(--volume "$host/users:$host/users" --volume "$work/restored-db:$work/restored-db")
run_restic restore "latest:$data/users" --host "$SHELFY_RESTIC_HOST" --tag media \
  --target "$host/users"
run_restic restore "latest:$SHELFY_STAGING_DIR/db" --host "$SHELFY_RESTIC_HOST" --tag db \
  --target "$work/restored-db"
"$bin" admin --data-dir "$host" install-snapshots "$work/restored-db"
start_server "$host"
cookie=$(sign_in "$host")
restored=$(posts_total "$cookie")
echo "the restored host serves the owner's $restored posts" >&2
[ "$restored" = "$before" ] || fail "the restored host has $restored posts, not $before"
member_posts=$(sqlite3 "$host/users/$member/library.sqlite" "SELECT count(*) FROM posts")
echo "and the member's $member_posts posts" >&2
stop_server

step "textfile metrics"
cat "$work"/textfile/*.prom
failed=$(awk '$1 != "#" && $1 ~ /_last_success($|\{)/ && $2 != 1' "$work"/textfile/*.prom)
[ -z "$failed" ] || fail "metrics not green: $failed"
count=$(grep -c -E '^shelfy_[a-z_]+_last_success(\{[^}]*\})? 1$' "$work"/textfile/*.prom |
  awk -F: '{ n += $2 } END { print n }')
[ "$count" -eq 4 ] || fail "expected 4 successful jobs, found $count"
step "rehearsal green: every metric reports success"
