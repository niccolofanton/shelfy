# Import v1 JSON

`POST /api/v1/imports` accepts `{ "uploadId": "<complete import upload>" }`
with a web session, CSRF headers and `Idempotency-Key`. Upload bearer tokens
can stage bytes but cannot start or read imports. Claim and job insertion are
one control transaction. A replay returns the original 202 response; another
key using consumed bytes returns `upload_consumed`. Poll `GET /imports/{id}`
for `{job, report}`; another user's job is 404. Cancel/retry use `/jobs/{id}`.

Accepted UTF-8 JSON (optional BOM):

- Desktop export: `{ "posts": [...], "collections": [...] }`.
- Bare array of post objects, including legacy Chrome exports.
- Instagram: `platform: "instagram"` or a nonempty `shortcode`; `caption`
  takes precedence over `text`. Composite IDs, decimal PKs and shortcode
  aliases map to one `ig_<pk>` key.
- X: `platform: "twitter"`, or unmarked records with `text` and
  `authorUsername`; decimal IDs or an allowed X status URL establish identity.
- Explicit `pinterest`, `web`, and `manual` desktop posts are retained.

Unknown envelopes, duplicate `posts`/`collections` properties, malformed JSON,
trailing input, unsupported formats (including ZIP until import v2), and files
containing no recognizable post shape fail permanently with
`import_format_unknown`. A recognized export can contain individually rejected
records (`bad_item`, `bad_id`) with zero-based source indices. The complete
syntax/collection prepass runs before the first library write, including when
`collections` follows `posts` in the envelope.

## Bounds and commits

The reader streams exports larger than 200 MiB. One serialized record is at
most 1 MiB and 16,384 structural tokens; JSON depth is bounded. At most 500
records and 4 MiB of conservatively charged serde tree allocations are held
per batch, with one queued batch. Collection definitions total at most 5,000
and 4 MiB. Text/note/description fields are limited to 20,000 characters,
other strings to 4,096, arrays to 500; existing ingest validation additionally
limits IDs, media and URLs. Input paths and object IDs are never trusted.

Each batch transaction contains its posts, memberships, cumulative report and
next source index. Retry reparses the source and skips committed indices;
cancellation or queue pause stops between batches. A batch already holding the
writer when cancellation commits can finish. Partial results remain available
on failed/cancelled jobs. Usage is counted in the write transaction and a batch
that exceeds the user's actual database-plus-media quota rolls back. Existing
quota/media-budget admission applies before processing; database growth follows
the shared quota accounting contract (disk budget sampled by metrics).

`report` has `imported`, `updated`, `collections`, `links`, `skipped`,
`rejected: [{index, code}]`, and `rejectedCount`. Rejection details contain the
first 1,000 records; `rejectedCount` remains exact. Batch changes emit
`posts.changed` with reason `import` and `stats.changed`. Completion records
one durable `import.done` notification in the same transaction as its completion
checkpoint. Consumed upload bytes retain the shared seven-day retention so
failed/cancelled jobs can resume; housekeeping removes them afterwards.

## Desktop parity and deliberate differences

IG/X field normalization is checked byte-for-byte against the actual desktop
normalizers in `shared/golden/import/`. Before `overwrite_ai` merging, repeated
canonical keys in a batch fold field-presence last-wins; membership keys unite.
An omitted analysis timestamp keeps the stored one and is NULL on a new undated
analysis, avoiding a new wall-clock write on every reimport. Known post user
notes and manual tags are never overwritten; new posts retain `note`/`userNote`
and `manualTags`/`userTags`. Imported local files are not published or read, and
no archive/hydration/capture job is scheduled.

The desktop defaults every non-Instagram record to X; the web port deliberately
retains explicit Pinterest/web/manual platforms. Website identity is the shared
normalized scheme-less URL hash, and a site remains a placeholder with no
capture. A manual `manual:<id>` maps via the shared deterministic legacy-ID
converter with its source timestamp or epoch, making separate reimports stable;
manual notes without files are `link_only`.

Collection keys are `x:<externalId>` or `n:<trimmed name>`. External identity
matches first (platform plus external ID on the web schema), then an equally
named manual collection, otherwise a collection is created. Existing names and
colors are preserved, and duplicate memberships do not write again. A missing
definition is reconstructed from its membership key. Invalid top-level
collection definitions fail before any post mutation.
