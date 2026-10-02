# SPIKE-2 — CDN media fetches from the datacenter IP

**Question.** Do the Instagram, X and Pinterest CDNs serve anonymous media fetches to the osn VPS (Hetzner datacenter IP)? The answer sets the per-platform archive mode of plan §2.13 / D14: `server`, `client` or `auto`.

**Answer.** Yes for Instagram and X: 100 % of the URLs that work from a residential connection also work from the VPS, with no block, challenge or throttling at 2 requests/s. Pinterest could not be tested: the reference library has no Pinterest posts.

| Platform | Mode | Basis |
|---|---|---|
| Instagram | `server` | 280 / 280 fresh URLs that work from the Mac also return 2xx images from the VPS (100 %; ≥ 98.9 % at 95 % confidence) |
| X | `server` | 300 / 300 (100 %; ≥ 99.0 % at 95 % confidence) |
| Pinterest | `auto` (provisional) | no sample; server-first with the breaker (D14) until the O1 extension export provides URLs |

Run on 2026-10-02, 11:23–11:26 UTC (E1: on the osn VPS, no throwaway server). Hermes stayed up throughout (running, 0 restarts). The probe container, its image and `/tmp/shelfy-spike` were removed at the end.

## Method

**Sample.** Read-only queries on the reference desktop library (`shelfy-web-local/ref/shelfy.sqlite`), written by `scripts/spikes/cdn-sample.mjs` to a TSV outside the repo. Cover URLs (`posts.thumbnail_url`) and media URLs (`post_media.source_url`, image slides and video posters) were deduplicated, then sampled with a seeded shuffle.

| | Instagram | X | Pinterest |
|---|---|---|---|
| Posts in the library | 3,997 | 2,140 | 0 |
| Unique media/cover URLs | 11,039 | 2,222 | 0 |
| Still valid (`oe` expiry more than 2 h away) | 3,003 (27 %) | no expiry | — |
| Already expired | 8,036 (73 %) | — | — |
| Sampled | 280 fresh (41 covers, 154 slides, 85 video posters) + 20 expired controls | 300 (87 photos, 213 video thumbnails) | 0 |

All sampled URLs are images; the desktop stores the poster image for video slides. Video files are on-demand (D4, D15) and belong to SPIKE-9.

**Probe.** `scripts/spikes/cdn-probe.mjs` sent one anonymous GET per URL with no cookies, a desktop Chrome 141 User-Agent, a browser image `Accept`, `Accept-Language` and the platform `Referer` (the request of §2.13). It used HTTP/1.1, one keep-alive connection per host group, manual redirects (≤ 5), a 30 s timeout and a 15 MiB cap. It recorded status, redirect hosts, TTFB, total time, bytes, content type, a body hash prefix, and a block/challenge label for non-image answers. Output is keyed by opaque sample ids; it holds no URLs.

**Pacing.** Each host group ran its requests one at a time, at least 500 ms apart plus 0–100 ms of jitter. That is ≤ 2 requests/s per host; the effective rate was 1.7–1.8/s, and each run took about 170 s.

**Two vantage points, same list, same minute.**
- **VPS:** container `shelfy-spike-cdn` (`node:24-alpine`, `--cpus 1 --memory 512m`; peak 24 MiB RAM, about 3 % CPU).
- **Mac:** the owner's residential connection in Italy.

A URL counts as "working" only if the Mac gets a 2xx image. That separates expired or deleted URLs from datacenter blocks. `scripts/spikes/cdn-compare.mjs` joins the two runs by id.

**Privacy.** The URL list was copied only to `/tmp/shelfy-spike/` on the VPS and deleted right after the run. The local copy lived outside the repo and was deleted at the end. This note reports aggregates only.

## Results

### Reachability

| | Instagram | X |
|---|---|---|
| Fresh URLs that work from the Mac | 280 / 280 | 300 / 300 |
| …that also return 2xx images from the VPS | **280 / 280 (100 %)** | **300 / 300 (100 %)** |
| Block or challenge pages, 429, `Retry-After` (VPS) | 0 | 0 |
| Redirects | 0 | 0 |
| Content types (VPS) | `image/jpeg` ×280 | `image/jpeg` ×292, `image/png` ×8 |
| Byte-identical to the Mac's copy | 280 / 280 | 293 / 300 |
| Expired controls (both vantage points) | 403 `text/plain` "URL signature expired" ×20 | — |

- **X bodies:** the 7 that differ are valid JPEGs within −321 B to +2.5 KB of the Mac's copy. From the VPS, `pbs.twimg.com` answers through Cloudflare (`server: cloudflare`). From the Mac it answers without that header, so through a different CDN path that encodes the same image slightly differently.
- **Instagram:** an expired signature looks the same from both IPs, a 403 from `proxygen-bolt` with a 21-byte text body.

### Latency (2xx only)

| | Instagram VPS | Instagram Mac | X VPS | X Mac |
|---|---|---|---|---|
| TTFB p50 / p95 / p99 / max | 107 / 417 / 590 / 696 ms | 15 / 28 ms (p50 / p95) | 277 / 434 / 508 / 578 ms | 41 / 355 ms (p50 / p95) |
| Total p50 / p95 / p99 / max | 119 / 450 / 683 / 728 ms | 19 / 77 ms (p50 / p95) | 388 / 730 / 892 / 974 ms | 46 / 356 ms (p50 / p95) |

- **Instagram:** the URLs pin one edge host (Milan or Rome, where the owner browses), so both vantage points hit the same edge. Both runs walked the list in the same order at the same pace, with the VPS about 2 s ahead. The VPS therefore most likely took the cache misses and the Mac the hits. The gap is not evidence of a datacenter penalty.
- **X:** the two vantage points reach different CDNs (see above), so their caches are independent. The slower VPS TTFB is consistent with cold-cache misses at its edge.
- **Overall:** neither platform is slow enough to matter for a background archive worker.

### Size (VPS, 2xx)

| | n | p50 | p90 | max | mean |
|---|---|---|---|---|---|
| Instagram covers | 41 | 104 KB | 283 KB | 3,044 KB | 269 KB |
| Instagram slides | 154 | 263 KB | 716 KB | 3,218 KB | 422 KB |
| Instagram video posters | 85 | 62 KB | 220 KB | 2,797 KB | 121 KB |
| X photos (`/media/`) | 87 | 125 KB | 349 KB | 1,289 KB | 170 KB |
| X video thumbnails | 213 | 73 KB | 159 KB | 323 KB | 82 KB |

- **Per image:** Instagram averages 308 KB, and 11 / 280 (3.9 %) exceed D4's 1.5 MB keep-original threshold. X averages 107 KB, and none exceed it.
- **Totals:** 84 MB for Instagram and 32 MB for X.

### Throttling

There was none:
- **2xx share:** 100 % in both the first and the last third of each run.
- **TTFB p50 per 50-request window, Instagram:** 112, 219, 69, 72, 68, 90 ms.
- **TTFB p50 per 50-request window, X:** 294, 272, 265, 275, 264, 315 ms.
- **Rate limits:** no 429 and no `Retry-After`.

## Decision

- **Instagram:** `server`.
- **X:** `server`.
- **Pinterest:** `auto` until a sample exists.
- **Breaker:** stays on for every platform. It is the safety net for what this spike cannot see: higher rates, longer windows and policy changes.

## Follow-ups for the plan

1. **§2.13 defaults.**
   - Set `instagram: server`, `x: server`, `pinterest: auto`.
   - Re-run the three scripts on the O1 extension export, which provides fresh Instagram and X URLs and the first Pinterest sample. The scripts take any TSV in the documented format.
2. **§2.13 breaker: expired is not blocked.**
   - An expired Instagram signature is a 403 from any IP. Counted naively, a backlog of stale URLs (the desktop migration) would trip the breaker on its own.
   - The archive worker should parse `oe` before fetching. An expired URL becomes an extension refresh task with no request.
   - A 403 whose body is the expiry text should count as `expired`, not `blocked`. The same applies to 404 (deleted media).
3. **§2.13 rates are untested above 2 req/s.**
   - This spike validates ≤ 2 req/s per host group. The plan's limits (Instagram 8 req/s, X 20 req/s) were not exercised.
   - Start at 2 req/s. That archives the reference library's ~11k Instagram and ~2k X images in about 1.5 hours, with the two host groups in parallel. Raise toward the §2.13 values only while the breaker metric stays at zero.
4. **Instagram URL lifetime (R17, migration).**
   - 73 % of the reference library's Instagram URLs had already expired.
   - The sampled valid ones expired 12.5–104 h after the probe (median 14.7 h).
   - Archiving right after ingest, ordered by expiry, is required, not an optimization.
   - The migration (T2/T9) must route expired Instagram URLs to refresh tasks or `link_only` instead of counting them as fetch failures.
5. **D4 sizes.**
   - The 15 MB fetch cap has about 5× headroom over the largest image seen (3.1 MB).
   - The re-encode path for images over 1.5 MB is real for Instagram (about 4 %) but rare.
   - Capacity model inputs: about 310 KB per Instagram image and 110 KB per X image.
6. **X bytes vary by edge.**
   - The same X image can hash differently when fetched from different edges.
   - CAS dedupe is still correct, at worst one duplicate object; nothing should assume byte identity across re-fetches.

## Limits

- **Coverage:** one datacenter IP, one three-minute window, about 2 req/s, images only.
- **What this does not prove:** that Meta or X never refuse the VPS. It shows they do not refuse it at archive-worker pace today.
- **Pinterest:** untested.

## Reproduce

```sh
node scripts/spikes/cdn-sample.mjs --db <shelfy.sqlite> --out /tmp/sample.tsv   # personal data: keep outside the repo
node scripts/spikes/cdn-probe.mjs --in /tmp/sample.tsv --out /tmp/mac.jsonl --label mac
# on the VPS, same TSV, inside a limited container:
docker run --rm --name shelfy-spike-cdn --cpus 1 --memory 512m --user 1000:1000 \
  -v /tmp/shelfy-spike:/work -w /work node:24-alpine \
  node cdn-probe.mjs --in sample.tsv --out vps.jsonl --label vps
node scripts/spikes/cdn-compare.mjs --ref /tmp/mac.jsonl --test /tmp/vps.jsonl
```
