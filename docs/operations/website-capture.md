# Website capture jobs

The authenticated, idempotent `POST /api/v1/sites` saves a website placeholder
and queues `capture.site`. URL identity ignores the HTTP/HTTPS scheme. The
request accepts `url`, `maxPages` (1–8, default 6) and `singlePage` (default false).
`POST /api/v1/sites/{key}/recapture` queues a new version; omitted `singlePage`
reuses the current version. `POST /links` queues website capture when configured.

Configure `SHELFY_CAPTURE_URL` and `SHELFY_INTERNAL_TOKEN` together. The capture
service sees `/work/<ULID>` on the volume the API mounts at
`/data/shelfy/work/capture`. Both containers use UID/GID 10100. The global site
limit is `SHELFY_CAPTURE_SITES_PARALLEL=1` (maximum 2); each user has one running
capture. Busy/unreachable service waits up to 15 minutes without consuming
tries, then uses the job's two-try policy. Daily limits count completed captures
plus active uncounted jobs; zero means unlimited.

Artifact ingest validates the full manifest contract and every file before any
CAS publication. Directory-relative opens reject links, traversal, nonregular
files and images with trailing payloads; caps are 15 MiB per image, 40 MiB per
video and 80 MiB per site. It reserves 80 MiB before dispatch and commits only
new master bytes, reusing the ordinary quota/store contract. Cover G480 and
ThumbHash are generated locally. Captures retain manual notes, tags and folders.
A blocked capture can store a complete bounded OG fallback; no fallback leaves
a failed placeholder. Cancelling a first capture removes only an unedited
placeholder. Retry repairs a removed placeholder, and restart uses a fresh work
ULID. Durable capture/job receipts prevent duplicate version creation and daily
charges on recovery. After ingest or receipt recovery, the P3-27 typed hook
enqueues a fenced website catalog job when automatic analysis and a vision route
are configured; off/unconfigured analysis leaves the capture complete without AI.
Hourly cleanup removes orphan ULID directories older than
24 hours, excluding running attempts.

`GET /health/capture` is public, outside `/api`, and returns status only.
`capture.event` is a live opt-in topic with bounded known codes and scalar
parameters. Metrics report only clamped durations, RSS and artifact bytes.

The operator can run `shelfy-server admin --data-dir /data/shelfy capture-eval
--corpus /private/corpus.json --out /private/aggregate.json`, with the capture URL
and token supplied in the environment. The corpus is a JSON array or one public
URL per line (maximum 1,000). It uses temporary work directories, performs no
user ingest and prints aggregates only. This command performs real website
captures; synthetic tests use the recorded fixture instead.

Validation: `cargo test -p shelfy-server --test capture` uses an in-process HTTP
fake, recorded images and isolated temporary data. Node fetch tests use stubbed
responses and exercise truncation, byte caps and redirect SSRF refusal without
network or AI providers. Container isolation and the complete real-network
Compose gate belong to P4-13/P4-27.
