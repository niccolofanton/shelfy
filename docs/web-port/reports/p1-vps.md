# P1 on the VPS: migration and the first restore drill (P1-25)

> **Date:** 2026-10-03. **Host:** `refs.niccolofanton.dev` (osn VPS), behind Cloudflare Access. **Server:** `server-v0.1.0-rc.3`. **Tool:** `shelfy-migrate` 0.1.0 from the same release (macOS arm64). Aggregate counts only (lane rule 9).

## Migration

**Source:** the owner's current desktop library. The desktop app was closed; the CLI opened the database read-only and immutable (45.4 MiB, `user_version` 3, no WAL).

**Sign-in:** `shelfy-migrate login` with the Access service-token headers from a 0600 file. The owner approved the device code on `/device` after a re-auth by link: there was no passkey yet. Clicking Approve before the re-auth spent the sign-in limit (F10).

### Dry run (`plan --redact`)

| Item | Count |
|---|---|
| Posts | 6,138: Instagram 3,997, X 2,140, web 1 |
| Slides | 11,482 (image 5,849, video 5,627, page 6) |
| Folders, memberships | 1, 2,264 |
| Tags, entities | 33, 10 |
| Columns | 121 present: 99 mapped, 22 dropped, 0 unmapped |
| Identity | 6,138 keys; 3,997 IG shortcodes decode to their pk; 0 unmappable |
| Duplicate groups | 0 |

### Run

| Step | Result |
|---|---|
| Bundle | 6,138 posts, 5,379 objects (980.9 MiB), database 24.0 MiB, in 4.4 s |
| Upload | 5,379 objects through Cloudflare and Access in 1,658.8 s (3.2 objects/s, 0.59 MiB/s), 0 resumed |
| Install | 147.4 s: 5,379 objects stored; g480 4,545 rendered, 1 failed; ThumbHash 4,100; cover g480 p50 10.7 KiB, p95 33.7 KiB |
| Archive state | client 1,641 · done 3,528 · partial 746 · pending 223. IG covers still valid: 220 (expire soon; P2's archive drain); expired: 1,641 |
| Settings | language taken from the desktop; the previous (empty) library is kept 7 days |
| Not migrated | 19 files (987.1 MiB) under `assets/` that no row references; videos stay on demand (§4.2) |

### Reconciliation

| | Desktop | Bundle | Installed |
|---|---|---|---|
| Posts (IG / X / web) | 3,997 / 2,140 / 1 | same | same |
| Slides | 11,482 | 11,482 | 11,482 |
| Folders / memberships | 1 / 2,264 | same | same |
| Tags / entities | 33 / 10 | same | same |
| Web captures | 1 | 1 | 1 |
| Media objects | — | 5,379 | 5,379 |
| Files present / missing | 6,038 / 264 | same | — |

**Verdict:** every count matches. The 264 missing files are videos the desktop did not keep (SPIKE-1).

## Backups and the restore drill

| Job | Result |
|---|---|
| Database snapshot | 2 databases, 26.0 MiB, in 2 s |
| Media backup | 9,924 files added, 1.02 GiB, in 52 s |
| Restore drill | 2 databases verified with 0 problems; 5,379 referenced objects, 0 missing, 0 renditions missing; 6 s |

## Findings

- **Upload throughput.** 3.2 objects/s through Cloudflare, against about 10/s locally (P1-19). Each object takes several round trips. With many users, tus creation-with-upload or parallel uploads would help (carry-over to P5).
- **One g480 rendition failed** out of 4,546. The cover falls back to the original; worth a look when P2 archives covers.
- **The owner check** (the library browsable on desktop and phone) is the owner's step on 2026-10-03.
