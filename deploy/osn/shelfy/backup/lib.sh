# shellcheck shell=bash
#
# Shared settings and helpers of the Shelfy backup jobs (plan §3.5), sourced by
# the four job scripts next to this file:
#
#   shelfy-db-snapshot.sh         hourly   snapshot the changed databases, restic backup (set db)
#   shelfy-media-backup.sh        daily    restic backup of the media store (set media)
#   shelfy-restic-maintenance.sh  weekly   forget + prune with the §3.5 retention, check 5 %
#   shelfy-restore-drill.sh       monthly  restore the control DB and one random library, verify
#
# Each job writes its result for node-exporter's textfile collector, as
# <metric> (1 = the last run succeeded, 0 = it failed) and
# <metric>_timestamp_seconds (the last success, kept across failures):
#
#   shelfy_backup_last_success{set="db"|"media"}   file shelfy_backup_<set>.prom
#   shelfy_restic_check_last_success               file shelfy_restic_check.prom
#   shelfy_restore_drill_last_success              file shelfy_restore_drill.prom
#
# Settings come from the environment (the systemd units read
# /etc/shelfy/backup.env when it exists); the defaults are the osn VPS layout:
#
#   SHELFY_DATA_DIR             /data/shelfy
#   SHELFY_STAGING_DIR          $SHELFY_DATA_DIR/backup-staging
#   SHELFY_TEXTFILE_DIR         /data/observability/textfile
#   SHELFY_ADMIN                the `shelfy-server admin` command, run at the lowest
#                               CPU and I/O priority inside the API container
#   SHELFY_RESTIC_IMAGE         restic/restic, pinned by digest
#   SHELFY_RESTIC_ENV_FILE      /etc/shelfy/restic.env: RESTIC_REPOSITORY,
#                               RESTIC_PASSWORD and the R2 credentials; mode 0600
#                               (or 0400), or restic does not run
#   SHELFY_RESTIC_CACHE_DIR     /var/cache/shelfy-restic
#   SHELFY_RESTIC_HOST          shelfy (the host name recorded in the snapshots)
#   SHELFY_RESTIC_LIMIT_UPLOAD  20480 (KiB/s)
#   SHELFY_RESTIC_DOCKER_ARGS   extra `docker run` arguments, such as the volume of a
#                               local repository (the rehearsal)
#
# The restic env file is read by `docker run --env-file`: one KEY=value per
# line, no quotes, no `export`. On the VPS (plan §3.4, the osn SOPS keys):
#
#   RESTIC_REPOSITORY=s3:<R2_ENDPOINT>/<R2_BUCKET>/restic/shelfy
#   RESTIC_PASSWORD=<SHELFY_RESTIC_PASSWORD>
#   AWS_ACCESS_KEY_ID=<AWS_ACCESS_KEY_ID>
#   AWS_SECRET_ACCESS_KEY=<AWS_SECRET_ACCESS_KEY>
#   AWS_DEFAULT_REGION=<R2_REGION, auto>
#
# restic runs in a container (the host has no restic binary) as
# `nice -n 19 ionice -c3 restic --limit-upload 20480`, with the data directory
# mounted read-only at its own path, so snapshot paths are the host paths.

: "${SHELFY_DATA_DIR:=/data/shelfy}"
: "${SHELFY_STAGING_DIR:=$SHELFY_DATA_DIR/backup-staging}"
: "${SHELFY_TEXTFILE_DIR:=/data/observability/textfile}"
: "${SHELFY_ADMIN:=docker exec osn-shelfy-api-1 nice -n 19 ionice -c3 -t /app/shelfy-server admin}"
: "${SHELFY_RESTIC_IMAGE:=restic/restic:0.19.1@sha256:136600b6ff6843d61d355f7f71f460a166429f35de6fd11b568fece3c9a4d510}"
: "${SHELFY_RESTIC_ENV_FILE:=/etc/shelfy/restic.env}"
: "${SHELFY_RESTIC_CACHE_DIR:=/var/cache/shelfy-restic}"
: "${SHELFY_RESTIC_HOST:=shelfy}"
: "${SHELFY_RESTIC_LIMIT_UPLOAD:=20480}"
: "${SHELFY_RESTIC_DOCKER_ARGS:=}"

# Retention (plan §3.5), used by shelfy-restic-maintenance.sh.
# shellcheck disable=SC2034
DB_RETENTION=(--keep-hourly 48 --keep-daily 14 --keep-weekly 8 --keep-monthly 6)
# shellcheck disable=SC2034
MEDIA_RETENTION=(--keep-daily 7 --keep-weekly 8 --keep-monthly 6)

# HELP texts of the two backup sets: node-exporter wants a family's HELP equal
# across files.
# shellcheck disable=SC2034
BACKUP_HELP='Whether the last run of the Shelfy backup set succeeded (1) or failed (0).'
# shellcheck disable=SC2034
BACKUP_TIMESTAMP_HELP='Unix time of the last successful run of the Shelfy backup set.'

# Extra `docker run` arguments of one job (the drill adds a writable volume).
RESTIC_MOUNTS=()

log() {
  printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >&2
}

# run_admin ARGS...: `shelfy-server admin ARGS...`.
run_admin() {
  local admin=()
  read -r -a admin <<<"$SHELFY_ADMIN"
  "${admin[@]}" "$@"
}

# file_mode FILE: the permission bits of FILE in octal, such as 600 (GNU stat
# on the hosts, BSD stat for a rehearsal on macOS).
file_mode() {
  stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1"
}

# check_restic_env: the restic env file holds the repository password and the
# R2 keys, so only its owner may read it.
check_restic_env() {
  local mode
  if [ ! -f "$SHELFY_RESTIC_ENV_FILE" ]; then
    log "no restic env file at $SHELFY_RESTIC_ENV_FILE"
    return 1
  fi
  mode=$(file_mode "$SHELFY_RESTIC_ENV_FILE")
  case "$mode" in
    600 | 400) ;;
    *)
      log "$SHELFY_RESTIC_ENV_FILE has mode $mode: it holds secrets, make it 0600 (chmod 600)"
      return 1
      ;;
  esac
}

# run_restic ARGS...: restic in its container.
run_restic() {
  local extra=()
  # Explicit: errexit is off in a command substitution and in a condition.
  check_restic_env || return
  if [ -n "$SHELFY_RESTIC_DOCKER_ARGS" ]; then
    read -r -a extra <<<"$SHELFY_RESTIC_DOCKER_ARGS"
  fi
  docker run --rm \
    --env-file "$SHELFY_RESTIC_ENV_FILE" \
    --env RESTIC_CACHE_DIR=/cache \
    --volume "$SHELFY_RESTIC_CACHE_DIR:/cache" \
    --volume "$SHELFY_DATA_DIR:$SHELFY_DATA_DIR:ro" \
    ${extra[@]+"${extra[@]}"} \
    ${RESTIC_MOUNTS[@]+"${RESTIC_MOUNTS[@]}"} \
    --entrypoint nice \
    "$SHELFY_RESTIC_IMAGE" -n 19 ionice -c3 \
    restic --retry-lock 30m --limit-upload "$SHELFY_RESTIC_LIMIT_UPLOAD" "$@"
}

# ensure_repository: creates the repository on the first run (restic exit
# code 10: it does not exist); any other failure stops the job, with what
# restic said. restic never prints the password or the keys: its messages name
# the repository location at most, with any password in a URL stripped.
ensure_repository() {
  local status=0 said line
  said=$(run_restic cat config 2>&1 >/dev/null) || status=$?
  case "$status" in
    0) ;;
    10)
      log "the restic repository does not exist yet: creating it"
      run_restic init
      ;;
    *)
      log "cannot open the restic repository (restic exit $status)"
      while IFS= read -r line; do
        [ -z "$line" ] || log "restic: $line"
      done <<<"$said"
      return "$status"
      ;;
  esac
}

# last_success FILE METRIC: the last success time recorded in FILE, or 0.
last_success() {
  local file=$1 name="$2_timestamp_seconds"
  if [ -f "$file" ]; then
    awk -v name="$name" '$1 != "#" && index($1, name) == 1 { v = $2 } END { print (v == "" ? 0 : v) }' "$file"
  else
    echo 0
  fi
}

# record_result OK: writes the job's metrics (JOB_FILE, JOB_METRIC,
# JOB_LABELS, JOB_HELP and JOB_TIMESTAMP_HELP, set by the job) atomically.
record_result() {
  local ok=$1 file last tmp
  file="$SHELFY_TEXTFILE_DIR/$JOB_FILE.prom"
  last=$(last_success "$file" "$JOB_METRIC")
  if [ "$ok" = 1 ]; then
    last=$(date +%s)
  fi
  # The units run with UMask=0077, but node-exporter reads the directory as
  # another user: create it, and any missing parent, with mode 0755.
  (umask 022 && mkdir -p "$SHELFY_TEXTFILE_DIR")
  tmp=$(mktemp "$SHELFY_TEXTFILE_DIR/.$JOB_FILE.XXXXXX")
  {
    printf '# HELP %s %s\n' "$JOB_METRIC" "$JOB_HELP"
    printf '# TYPE %s gauge\n' "$JOB_METRIC"
    printf '%s%s %s\n' "$JOB_METRIC" "$JOB_LABELS" "$ok"
    printf '# HELP %s_timestamp_seconds %s\n' "$JOB_METRIC" "$JOB_TIMESTAMP_HELP"
    printf '# TYPE %s_timestamp_seconds gauge\n' "$JOB_METRIC"
    printf '%s_timestamp_seconds%s %s\n' "$JOB_METRIC" "$JOB_LABELS" "$last"
  } >"$tmp"
  chmod 0644 "$tmp"
  mv -f "$tmp" "$file"
}

# finish_job: the EXIT trap of every job. Records the result and keeps the
# job's exit status.
finish_job() {
  local status=$? ok=0
  if [ "$status" -eq 0 ]; then
    ok=1
  fi
  record_result "$ok" || log "cannot write the metrics to $SHELFY_TEXTFILE_DIR"
  if [ "$ok" = 1 ]; then
    log "$JOB_NAME done in ${SECONDS}s"
  else
    log "$JOB_NAME FAILED (exit $status) after ${SECONDS}s"
  fi
  exit "$status"
}

# begin_job NAME: logs the start and records the result on exit.
#
# A job stopped by a signal fails: systemd's TERM at TimeoutStartSec, at
# `systemctl stop` or at a reboot, Ctrl-C (INT), a hangup. Without these traps
# bash would run the EXIT trap with the status of the last command that
# finished, often 0, and record a success with a fresh time stamp, so the
# "backups stale" alert would never fire.
begin_job() {
  JOB_NAME=$1
  SECONDS=0
  trap finish_job EXIT
  trap 'exit 143' TERM
  trap 'exit 130' INT
  trap 'exit 129' HUP
  log "$JOB_NAME started"
}
