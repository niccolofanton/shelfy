#!/usr/bin/env bash
#
# Monthly (shelfy-restore-drill.timer, plan §3.5): restores the control
# database and one random library from the latest "db" snapshot into
# $SHELFY_STAGING_DIR/drill, then runs `shelfy-server admin verify` on them:
# PRAGMA integrity_check and foreign keys, row counts within 10 % of the live
# databases (the live data moved on since the snapshot), and every media
# reference of the library resolvable in the live store. Metric:
# shelfy_restore_drill_last_success. Settings: lib.sh.
#
# The restored files are removed after a success and kept for inspection
# after a failure (the next drill clears them).

# shellcheck source-path=SCRIPTDIR
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

JOB_FILE=shelfy_restore_drill
JOB_METRIC=shelfy_restore_drill_last_success
JOB_LABELS=''
JOB_HELP='Whether the last monthly Shelfy restore drill succeeded (1) or failed (0).'
JOB_TIMESTAMP_HELP='Unix time of the last successful Shelfy restore drill.'
begin_job "restore drill"

db_dir="$SHELFY_STAGING_DIR/db"
drill_dir="$SHELFY_STAGING_DIR/drill"
rm -rf "$drill_dir"
mkdir -p "$drill_dir"

snapshot=$(run_restic snapshots --host "$SHELFY_RESTIC_HOST" --tag db --latest 1 --json |
  sed -n 's/.*"short_id":"\([0-9a-f]*\)".*/\1/p' | tail -n 1)
if [ -z "$snapshot" ]; then
  log "no db snapshot to restore"
  exit 1
fi

users=$(run_restic ls "$snapshot" "$db_dir/users" |
  sed -n "s|^$db_dir/users/\([A-Za-z0-9]*\)\.sqlite\$|\1|p")
count=$(printf '%s\n' "$users" | grep -c . || true)
if [ "$count" -eq 0 ]; then
  log "snapshot $snapshot holds no library"
  exit 1
fi
user=$(printf '%s\n' "$users" | sed -n "$((RANDOM % count + 1))p")
log "snapshot $snapshot: restoring the control database and user $user's library (one of $count)"

started=$SECONDS
RESTIC_MOUNTS=(--volume "$drill_dir:$drill_dir")
run_restic restore "$snapshot" --target "$drill_dir" \
  --include "$db_dir/control.sqlite" --include "$db_dir/users/$user.sqlite"
restore_seconds=$((SECONDS - started))
restored="$drill_dir$db_dir"
if [ "$(id -u)" = 0 ]; then
  # restic restores as root; the API container reads as the data directory's owner.
  chown -R "$(stat -c '%u:%g' "$SHELFY_DATA_DIR")" "$drill_dir"
fi
log "restored $(du -sk "$restored" | awk '{print $1}') KiB in ${restore_seconds}s"

started=$SECONDS
run_admin verify "$restored" --user "$user" --max-drift 10
log "verified in $((SECONDS - started))s"

# Cleaning up is not part of the drill: the next one starts by clearing it.
rm -rf "$drill_dir" || log "cannot remove $drill_dir yet; the next drill clears it"
