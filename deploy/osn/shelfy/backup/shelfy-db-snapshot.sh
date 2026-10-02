#!/usr/bin/env bash
#
# Hourly at :05 (shelfy-db-snapshot.timer, plan §3.5): copies the control
# database and every library changed since the last run into
# $SHELFY_STAGING_DIR/db with SQLite's online backup API
# (`shelfy-server admin snapshot --changed`), then backs that directory up
# with restic, tag "db". Metric: shelfy_backup_last_success{set="db"}.
#
# A library that cannot be copied does not stop the others: the backup still
# runs, and the job fails afterwards. Settings: lib.sh.

# shellcheck source-path=SCRIPTDIR
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

JOB_FILE=shelfy_backup_db
JOB_METRIC=shelfy_backup_last_success
JOB_LABELS='{set="db"}'
JOB_HELP=$BACKUP_HELP
JOB_TIMESTAMP_HELP=$BACKUP_TIMESTAMP_HELP
begin_job "database backup"

out="$SHELFY_STAGING_DIR/db"
snapshot_status=0
log "snapshot of the changed databases into $out"
run_admin snapshot --changed --out "$out" || snapshot_status=$?

ensure_repository
log "restic backup of $out (tag db)"
run_restic backup --host "$SHELFY_RESTIC_HOST" --tag db \
  --exclude .lock --exclude snapshot-state.json --exclude '*.partial' \
  "$out"

if [ "$snapshot_status" -ne 0 ]; then
  log "the snapshot failed (exit $snapshot_status): the backup holds the previous copy of what failed"
  exit "$snapshot_status"
fi
