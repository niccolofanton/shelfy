# Local covers for existing Instagram posts

`shelfy-server admin backfill-covers` recovers local images/posters without
importing posts or fetching URLs. Its output contains aggregate counts only.
Keep the bundle outside the checkout, with directory permissions `0700` and
file permissions `0600`; it contains private owner/post identifiers.

The manifest is JSON version 1. Example values below are synthetic:

```json
{
  "version": 1,
  "userId": "synthetic-owner",
  "entries": [{
    "postKey": "ig_123",
    "nativeId": "123",
    "shortcode": "synthetic123",
    "mediaType": "video",
    "file": "images/0001.jpg",
    "ext": "jpg",
    "sha256": "<64 lowercase hexadecimal SHA-256 characters>",
    "bytes": 12345
  }]
}
```

The owner must be the unique active owner. Each entry must match an existing
Instagram post by key, native ID, shortcode and media type. Files must be
regular files inside the bundle, with matching byte count, hash and image
header, and must decode within the image pipeline's limits. Absolute paths,
traversal and final file symlinks are refused. The manifest supports at most
1,000 entries, 15 MiB per image and 256 MiB total input.

Dry-run is the default and opens existing SQLite databases read-only:

```sh
shelfy-server admin --data-dir /data/shelfy backfill-covers \
  --manifest /private/backfill/manifest.json
```

Stop the API before applying. Run the same deployed server binary/image in
one offline process, with its existing data volume writable and the private
bundle mounted read-only. Preserve the instance's `SHELFY_ARCHIVE_*` flags
and `SHELFY_MEDIA_BUDGET_GB` (or equivalent command flags): those determine
archive state and the operator's storage budget.

```sh
shelfy-server admin --data-dir /data/shelfy backfill-covers \
  --manifest /private/backfill/manifest.json --apply --server-stopped
```

Restart the API after the command finishes, including after a partial failure.
Its quota ledger and library caches must be reconstructed from the writes.
Apply refuses a missing offline acknowledgement. The acknowledgement is an
operator assertion, not automatic detection of other processes.

Validation of every entry precedes media writes. Apply checks identity and
absence of **all** cover/image/video objects again inside each library write
transaction. Existing media and trashed posts are skipped. Captions, notes,
tags and AI results are preserved. Each new cover uses the existing archive
store/quota path: CAS deduplication, renditions, ThumbHash, media links and
derived archive state, with reservations released on failure and only newly
stored master bytes counted. This inherits that path's transaction semantics;
it does not introduce a cross-database crash-atomic transaction.

Entries commit independently. Replaying the same manifest skips committed
posts and does not count their bytes twice. An error identifies only its entry
number/reason; earlier entries may have committed. Inspect/apply output reports
`entries`, `eligible`, `skippedExisting`, `skippedTrashed`, `inputBytes`,
`stored` and `addedBytes`.
