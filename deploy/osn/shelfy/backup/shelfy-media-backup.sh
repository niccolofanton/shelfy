#!/usr/bin/env bash
#
# Daily at 03:30 UTC (shelfy-media-backup.timer, plan §3.5): backs up the
# media stores, $SHELFY_DATA_DIR/users, with restic, tag "media". The
# databases (the hourly db set has them), exports, files being written and
# maintenance locks are left out; the video cache and work directories live
# elsewhere and are never backed up. CAS files never change, so restic's
# deduplication keeps this cheap. Metric:
# shelfy_backup_last_success{set="media"}. Settings: lib.sh.

# shellcheck source-path=SCRIPTDIR
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

JOB_FILE=shelfy_backup_media
JOB_METRIC=shelfy_backup_last_success
JOB_LABELS='{set="media"}'
JOB_HELP=$BACKUP_HELP
JOB_TIMESTAMP_HELP=$BACKUP_TIMESTAMP_HELP
begin_job "media backup"

ensure_repository
log "restic backup of $SHELFY_DATA_DIR/users (tag media)"
run_restic backup --host "$SHELFY_RESTIC_HOST" --tag media \
  --exclude '*.sqlite*' --exclude exports --exclude .tmp --exclude LOCKED \
  "$SHELFY_DATA_DIR/users"
