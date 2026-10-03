# Shelfy export v2

A ZIP64 archive of one user's library. All names use `/`; there are no absolute
paths, symlinks, user-directory prefixes, credentials or control database files.
Every entry has ZIP64 local headers; the archive has a ZIP64 end record even when
it is small. `library.sqlite` and `manifest.json` use Deflate; media use Stored.

| Entry | Contents |
|---|---|
| `manifest.json` | UTF-8 JSON described below |
| `library.sqlite` | Consistent SQLite online-backup snapshot, application id `SHLB`; no WAL/SHM files |
| `media/<aa>/<sha256>.<ext>` | Every `media_objects` master, including unreferenced objects and kept videos; lowercase hex digest, first two digits as shard |

Renditions and the global transient video cache are excluded. Derived library
rows (FTS, embeddings, aliases, clusters, AI metadata, notifications and settings)
are included. The importer rebuilds derived data using the existing migration
install path; it checks `library.sqlite` and installs the masters through the CAS.
The bundle never changes the original library.

## Manifest

```json
{
  "format": "shelfy-export",
  "version": 2,
  "createdAt": 1790985600000,
  "serverVersion": "0.1.0",
  "librarySchemaVersion": 4,
  "counts": { "posts": 10, "media_objects": 12 },
  "entries": [
    { "path": "library.sqlite", "sha256": "<64 lowercase hex digits>", "bytes": 245760 },
    { "path": "media/aa/<64 lowercase hex digits>.jpg", "sha256": "<same digest>", "bytes": 12345 }
  ]
}
```

`counts` contains every user table in the snapshot, excluding SQLite internal
and virtual-table shadow tables. Each payload entry appears exactly once in
`entries`; `manifest.json` does not list/hash itself. Hashes and sizes describe
**uncompressed bytes**. Creation and expiry timestamps use Unix milliseconds.
The creation time is the queued request time; the snapshot is taken when the
worker starts. Entry ordering is not a compatibility requirement.

The writer copies 64 KiB at a time and re-hashes every master, refusing missing,
non-regular, changed-size or corrupt objects. Manifest entry metadata is spooled
on disk; only SQLite pages, compression buffers and ZIP directory metadata are
kept in memory, never complete objects or the complete database.

## Lifecycle and API

- `POST /api/v1/exports` requires `Idempotency-Key`; 202 returns
  `{id, jobId, createdAt, expiresAt, bytes}` (`bytes: null` while building).
  `Location` points to `/api/v1/jobs/<jobId>` for progress, cancel and retry.
- `GET /api/v1/exports` returns the user's live bundles with size/expiry.
- `GET /api/v1/exports/<id>/download` returns `application/zip`, `no-store`,
  `attachment; filename="shelfy-export-YYYY-MM-DD.zip"`; a single byte Range
  returns 206, an unsatisfiable range 416. Not ready is 409; unknown/expired or
  another user's id is 404. These routes accept sessions only, never API tokens.
- `DELETE /api/v1/exports/<id>` tombstones the bundle before cancelling its
  worker. Publication checks the tombstone and the worker's attempt fence.

A user has at most one live bundle, including a ready one: another creation
returns it until deletion or expiry. A failed/cancelled job can be retried through
the jobs API while its bundle is live. Exports do not count against user quotas.
A conservative estimate reserves available filesystem bytes for queued/building
exports; `storage_full` refuses a request that cannot fit. One worker runs overall.

Metadata and the queued job commit atomically. Admission to the scheduler occurs
only after commit; boot recovery loads a job committed before admission. Each
attempt acquires its export lock and rebuilds abandoned scratch files. Publication
fsyncs the ZIP, renames `users/<id>/exports/<ulid>.zip.part` to `<ulid>.zip`, fsyncs
the directory, then records its size under the job's attempt fence. A crash between
rename and metadata commit is recovered by rebuilding. No partial ZIP is served.

Exports expire seven days after creation. The hourly maintenance sweep cancels
expired workers, removes their ZIP/scratch files, then deletes metadata. A live
worker lock defers removal until that worker stops. Restic's existing `exports`
exclusion applies; these bundles can always be recreated.
