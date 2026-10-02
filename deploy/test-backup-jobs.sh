#!/usr/bin/env bash
#
# Checks of the backup jobs' shared helpers, deploy/osn/shelfy/backup/lib.sh,
# that need neither restic nor a server (rehearse-backups.sh runs the real
# jobs):
#
# - a job records 1 and a new time stamp when it succeeds, and 0 with the
#   previous time stamp when it fails, also when a signal stops it: TERM
#   (systemd at TimeoutStartSec, a stop, a reboot), INT and HUP;
# - the textfile directory it creates stays readable by node-exporter under
#   the units' UMask=0077;
# - restic does not run with an env file that others can read, and a failure
#   to open the repository is logged with what restic said.
#
# Linux bash and coreutils, as on the VPS:
#
#   docker run --rm -v "$PWD:/w:ro" -w /w debian:bookworm-slim deploy/test-backup-jobs.sh
#
# Exits 1 unless every check passes.

# shellcheck source-path=SCRIPTDIR
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
lib=$repo/deploy/osn/shelfy/backup/lib.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failed=0

pass() {
  printf 'ok    %s\n' "$*"
}

fail() {
  printf 'FAIL  %s\n' "$*"
  failed=$((failed + 1))
}

mode_of() {
  stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1"
}

# The time of the last success before each test.
OLD_STAMP=1700000000
OK_SERIES='shelfy_backup_last_success{set="db"}'
STAMP_SERIES='shelfy_backup_last_success_timestamp_seconds{set="db"}'

# job_script BODY: a job like the four of lib.sh (with the database backup's
# metric) that runs BODY.
job_script() {
  cat >"$work/job.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
. "$lib"
JOB_FILE=shelfy_backup_db
JOB_METRIC=shelfy_backup_last_success
JOB_LABELS='{set="db"}'
JOB_HELP=\$BACKUP_HELP
JOB_TIMESTAMP_HELP=\$BACKUP_TIMESTAMP_HELP
begin_job "test job"
$1
EOF
  chmod +x "$work/job.sh"
}

# seed DIR: a textfile directory where the last run succeeded at OLD_STAMP.
seed() {
  mkdir -p "$1"
  printf '%s 1\n%s %s\n' "$OK_SERIES" "$STAMP_SERIES" "$OLD_STAMP" >"$1/shelfy_backup_db.prom"
}

# value DIR SERIES: the value of SERIES in the job's textfile.
value() {
  awk -v series="$2" '$1 == series { print $2 }' "$1/shelfy_backup_db.prom"
}

# --- The result of a job -----------------------------------------------------

dir=$work/success
seed "$dir"
job_script 'true'
status=0
SHELFY_TEXTFILE_DIR=$dir "$work/job.sh" 2>/dev/null || status=$?
if [ "$status" = 0 ] && [ "$(value "$dir" "$OK_SERIES")" = 1 ] &&
  [ "$(value "$dir" "$STAMP_SERIES")" -gt "$OLD_STAMP" ]; then
  pass "a job that succeeds records 1 and a new time stamp"
else
  fail "a job that succeeds (exit $status): $(cat "$dir/shelfy_backup_db.prom")"
fi

dir=$work/failure
seed "$dir"
job_script 'exit 3'
status=0
SHELFY_TEXTFILE_DIR=$dir "$work/job.sh" 2>/dev/null || status=$?
if [ "$status" = 3 ] && [ "$(value "$dir" "$OK_SERIES")" = 0 ] &&
  [ "$(value "$dir" "$STAMP_SERIES")" = "$OLD_STAMP" ]; then
  pass "a job that fails records 0 and keeps the last success's time stamp"
else
  fail "a job that fails (exit $status): $(cat "$dir/shelfy_backup_db.prom")"
fi

# A job stopped by a signal while a step runs (restic hanging on the network):
# the signal reaches the job and its child, as systemd's KillMode=control-group
# sends it, so each runs in a process group of its own.
for signal in TERM INT HUP; do
  dir=$work/$signal
  seed "$dir"
  job_script 'sleep 30'
  set -m
  SHELFY_TEXTFILE_DIR=$dir "$work/job.sh" 2>"$work/$signal.log" &
  pid=$!
  set +m
  sleep 1
  kill -s "$signal" -- "-$pid"
  status=0
  wait "$pid" || status=$?
  expected=$((128 + $(kill -l "$signal")))
  ok=$(value "$dir" "$OK_SERIES")
  stamp=$(value "$dir" "$STAMP_SERIES")
  if [ "$status" = "$expected" ] && [ "$ok" = 0 ] && [ "$stamp" = "$OLD_STAMP" ]; then
    pass "a job stopped by SIG$signal records 0 and keeps the last success's time stamp"
  else
    fail "a job stopped by SIG$signal: exit $status, recorded $ok at $stamp"
    sed 's/^/      /' "$work/$signal.log"
  fi
done

# --- The textfile directory --------------------------------------------------

dir=$work/fresh/textfile
job_script 'true'
(
  umask 077
  SHELFY_TEXTFILE_DIR=$dir "$work/job.sh" 2>/dev/null
)
modes="$(mode_of "$work/fresh") $(mode_of "$dir") $(mode_of "$dir/shelfy_backup_db.prom")"
if [ "$modes" = "755 755 644" ]; then
  pass "under UMask=0077 the textfile directory and its parents are created 0755, the file 0644"
else
  fail "textfile modes (parent, directory, file): $modes, want 755 755 644"
fi

# --- restic ------------------------------------------------------------------

env_file=$work/restic.env
printf 'RESTIC_REPOSITORY=/repo\nRESTIC_PASSWORD=not-a-secret\n' >"$env_file"

# restic_with MODE COMMAND: COMMAND after sourcing lib.sh, with the env file at
# MODE and `docker` replaced by the function `fake_docker` of the caller.
restic_with() {
  chmod "$1" "$env_file"
  (
    SHELFY_RESTIC_ENV_FILE=$env_file
    # shellcheck source=osn/shelfy/backup/lib.sh
    . "$lib"
    # shellcheck disable=SC2329 # run_restic calls it
    docker() {
      fake_docker "$@"
    }
    "$2"
  ) 2>&1
}

# shellcheck disable=SC2329 # docker() calls it
fake_docker() {
  echo "docker $*"
}
run_snapshots() {
  run_restic snapshots
}
if out=$(restic_with 0644 run_snapshots); then
  fail "restic ran with an env file of mode 0644"
elif grep -q '^docker ' <<<"$out"; then
  fail "docker ran with an env file of mode 0644"
elif grep -q 'has mode 644' <<<"$out"; then
  pass "restic does not run with an env file that others can read"
else
  fail "an env file of mode 0644: $out"
fi
if out=$(restic_with 0600 run_snapshots) && grep -q '^docker run' <<<"$out"; then
  pass "restic runs with an env file of mode 0600"
else
  fail "an env file of mode 0600: $out"
fi

# shellcheck disable=SC2329 # docker() calls it
fake_docker() {
  echo 'Fatal: unable to open config file: Stat: 403 Forbidden' >&2
  return 1
}
if out=$(restic_with 0600 ensure_repository); then
  fail "ensure_repository succeeded on a repository it cannot open"
elif grep -q 'restic: Fatal: unable to open config file: Stat: 403 Forbidden' <<<"$out"; then
  pass "a repository that cannot be opened is logged with restic's message"
else
  fail "restic's message is not in the log: $out"
fi

# shellcheck disable=SC2329 # docker() calls it
fake_docker() {
  case "$*" in
    *' cat config') return 10 ;;
    *' init') echo 'created restic repository' ;;
    *) return 1 ;;
  esac
}
if out=$(restic_with 0600 ensure_repository) && grep -q 'created restic repository' <<<"$out"; then
  pass "a missing repository is created"
else
  fail "a missing repository: $out"
fi

if [ "$failed" -ne 0 ]; then
  printf '%s checks failed\n' "$failed"
  exit 1
fi
echo "all checks passed"
