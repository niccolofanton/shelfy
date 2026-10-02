#!/usr/bin/env bash
#
# Sundays at 04:30 UTC (shelfy-restic-maintenance.timer, plan §3.5): applies
# the retention (db: 48 hourly, 14 daily, 8 weekly, 6 monthly; media: 7
# daily, 8 weekly, 6 monthly), prunes, and checks the repository by reading
# back 5 % of its data. Metric: shelfy_restic_check_last_success.
# Settings: lib.sh.

# shellcheck source-path=SCRIPTDIR
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

JOB_FILE=shelfy_restic_check
JOB_METRIC=shelfy_restic_check_last_success
JOB_LABELS=''
JOB_HELP='Whether the last weekly maintenance of the Shelfy restic repository (forget, prune, check) succeeded (1) or failed (0).'
JOB_TIMESTAMP_HELP='Unix time of the last successful weekly maintenance of the Shelfy restic repository.'
begin_job "restic maintenance"

ensure_repository
log "forget the db snapshots beyond the retention"
run_restic forget --host "$SHELFY_RESTIC_HOST" --tag db "${DB_RETENTION[@]}"
log "forget the media snapshots beyond the retention"
run_restic forget --host "$SHELFY_RESTIC_HOST" --tag media "${MEDIA_RETENTION[@]}"
log "prune"
run_restic prune
log "check, reading 5 % of the data"
run_restic check --read-data-subset=5%
