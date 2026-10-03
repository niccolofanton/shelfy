# SPIKE-9 — On-demand video and link hydration from the VPS

**Question.** Plan §9: how long do Instagram video URLs stay valid, and which public endpoints let the server fetch a video, or hydrate a shared link, for Instagram, X and Pinterest from the osn VPS without user cookies? The answer sets the on-demand video chain (D15, §2.13, P4), the hydration endpoints of `link.hydrate` (§2.17, P2) and Pinterest's archive mode, which SPIKE-2 left at `auto`.

**Answer.** Every platform works from the VPS today, anonymously: 360 route runs, at most 1 request every 3 s to Instagram and 1 per second to the other hosts, met no 429, challenge or login wall.

| Question | Result |
|---|---|
| Instagram URL lifetime (Q1) | Images: `oe` is 104–108 h after capture (123 URLs from the extension capture, median 106.3 h). Videos: two lifetimes, set per file: 32–35 h for 17 of 30 progressive files, 104–108 h for the other 13. Every URL tried from the VPS served: 123 / 123 images, 30 / 30 videos. |
| On-demand video (Q2) | Instagram 30 / 30 (post page, GraphQL, yt-dlp); X 29 / 29 available posts (syndication, yt-dlp); Pinterest 30 / 30 (pidgets, PinResource, yt-dlp). |
| Link hydration (Q3) | Instagram post page, X `tweet-result` and Pinterest PinResource return caption, author, date, image and video for every available post. |

| Decision | Instagram | X | Pinterest |
|---|---|---|---|
| Archive mode (P2, §2.13) | `server` | `server` | `server` (was `auto`) |
| Hydration endpoint (P2, §2.17) | post page, then GraphQL, then the extension | `tweet-result`, then oEmbed | PinResource, then pidgets `pins/info` |
| On-demand video (P4, D15) | stored URL while `oe` holds, then post page or GraphQL, then yt-dlp, then the extension | stored variant, then `tweet-result`, then yt-dlp, then the extension | stored MP4, then PinResource or pidgets, then yt-dlp, then the extension |

Run on 2026-10-02, 23:20–23:55 UTC, on the osn VPS (E1) in throwaway containers of the production image. Hermes stayed up: running, 0 restarts, the same start time before and after, node status online. The containers, the Node binary and `/tmp/shelfy-spike9` were removed at the end.

## Method

### Samples

| | Instagram | X | Pinterest |
|---|---|---|---|
| Source | reference library, posts with media type `video` (3,020) | reference library, `video` (1,611) | 45 public video pins from 8 brand accounts and boards, at most 8 per source (`video-pins.mjs`): the library has no Pinterest posts |
| Sampled | 30, seeded shuffle | 30 | 30 |
| Library caption and author to compare with | 28 and 30 | 28 and 30 | none: compared with PinResource instead |

Q1 used the owner's SPIKE-3 export `shelfy-spike3-capture-20261002-171059.json`. Its name stamp is UTC: the 123 Instagram posts were captured at 17:10:01–17:10:31 UTC, 19:10 local time. The export holds no video URLs, because the SPIKE-3 parsers keep only the poster of Instagram and X videos (SPIKE-3 deviation table). Each post's cover URL is also its poster, so Q1 measures 123 unique image URLs. The video URL lifetime comes from the video URLs that the anonymous routes returned on the VPS.

### Routes

| Platform | Route | Request | Video it fetches |
|---|---|---|---|
| Instagram | post page | `GET /p/<code>/` as a document load; reads the logged-out media object `xig_polaris_media.if_not_gated_logged_out` in the inline JSON | `video_versions[0]` |
| | GraphQL | `POST /api/graphql`, `PolarisLoggedOutDesktopWWWPostRootContentQuery`, doc_id `27130156389949648`, `{media_id}`, with the LSD token of the home page (fetched once per 10 min); the recipe of yt-dlp 2026.08.19 | same object, `video_versions[0]` |
| | embed | `GET /p/<code>/embed/captioned/` as a cross-site iframe load; reads `contextJSON.gql_data.shortcode_media` | `video_url` |
| | yt-dlp | home page, `get_ruling_for_content`, the same GraphQL query | plan §2.13 format string |
| X | syndication | `GET cdn.syndication.twimg.com/tweet-result?id=<id>&token=<t>`, the endpoint of X's embed widget; `t = ((id / 1e15) * π).toString(36)` without zeros and dots | best MP4 variant whose short side is at most 1080 px |
| | oEmbed | `GET publish.x.com/oembed?url=<post>` | none (hydration only) |
| | yt-dlp | guest token from `api.x.com`, then GraphQL `TweetResultByRestId` | plan §2.13 format string |
| Pinterest | pidgets | `GET widgets.pinterest.com/v3/pidgets/pins/info/?pin_ids=<id>`, the public widget API | `V_720P`, else the widest MP4 |
| | PinResource | `GET www.pinterest.com/resource/PinResource/get/` with `field_set_key: unauth_react_main_pin`, yt-dlp's endpoint | same |
| | pin page | `GET /pin/<id>/`; reads the Relay responses inlined in the HTML | same |
| | oEmbed | `GET www.pinterest.com/oembed.json?url=<pin>` | none (hydration only) |
| | yt-dlp | PinResource | plan §2.13 format string |

These are the desktop's three anonymous Instagram routes, which the desktop validated from a residential IP on 2026-06-25 (yt-dlp without cookies, the page's `video_versions`, logged-out GraphQL), plus each platform's embed endpoint. The desktop ships only the first (`execVideo` in `electron/downloader.ts`). Its two-stage login fallback is gone from the current tree, and this spike tests anonymous routes only.

### Probe

**What it does.** `scripts/spikes/video-probe.mjs` runs every route of a post in turn and records:
- what came back: status, timings, block signals, the hydration fields found, and whether caption and author match the library;
- the video it would fetch, and for Instagram the hours left before the `oe` of the video and poster URLs.

**Requests.** They go through `node:https` with explicit browser headers: Node's `fetch` rewrites `Sec-Fetch-Mode` to `cors`, and Instagram then serves the embed as a client-side app with no data. Redirects into login or challenge pages are never followed.

**Downloads.**
- The first route that resolves a file downloads it, with a 300 MiB cap and a 180 s timeout, then runs ffprobe (codecs, size, duration) and checks whether `moov` comes before `mdat`. The file is deleted at once.
- A later route of the same post that resolves the same CDN file only checks its own signed URL, with a 1 KiB range request.
- yt-dlp always downloads its own selection.

**yt-dlp.** It ran with the flags of plan §2.13: `--ignore-config --no-cookies --no-cookies-from-browser --no-cache-dir --no-plugin-dirs --use-extractors <extractor> -f "bv*[ext=mp4]+ba[ext=m4a]/b[ext=mp4]/b" --merge-output-format mp4 --ffmpeg-location /usr/bin/ffmpeg`. On top of those: `--no-playlist --sleep-requests 1 --max-filesize 300M --socket-timeout 30 --write-info-json`.
- No `--user-agent`: the pinned build sends its own Chrome 146 identity.
- No `--proxy`: Smokescreen is P4, and the egress IP is the same.

**Pacing and stops.**
- **Spacing:** requests start at least 3 s apart to `www.instagram.com` and 1 s apart per other host group, plus 0–250 ms of jitter.
- **Platform stop:** at the first 429, rate-limit message or challenge.
- **Route stop:** after two posts in a row behind a login wall.
- **Retries:** none.

**Containers.**
- **Image:** `ghcr.io/niccolofanton/shelfy-api:v0.1.0-rc.2` (yt-dlp 2026.08.19, ffmpeg 5.1.9). The entrypoint was replaced by Node 24.20.0: the official linux-x64 build, checked against `SHASUMS256.txt` and mounted read-only.
- **Limits:** `--cpus 0.5 --memory 768m --cap-drop ALL --security-opt no-new-privileges`, uid 10100, work files on the host tmpfs under `/tmp/shelfy-spike9`.
- **Order:** one container per platform, one after the other. The other lane's SPIKE-4 and SPIKE-11 containers ran at the same time, and host load stayed under 3.6 on 4 vCPUs.

**Two vantage points.** The VPS ran everything with downloads. The owner's residential connection ran the same sample with `--resolve-only` (no downloads) to separate posts that are gone from refusals of the datacenter IP.
- **Instagram and X:** the reference is complete.
- **Pinterest:** it stopped at a 429 on the second pin page (see "Blocks").

**Privacy.**
- **Where the data lived:** the sample (the owner's post URLs) and the capture's URLs were copied only to `/tmp/shelfy-spike9/` on the VPS and deleted after the run; the local copies lived outside the repo and were deleted at the end.
- **What the results hold:** opaque sample ids, statuses, timings, sizes, booleans, codec data and 12-hex hashes; no URLs, captions or usernames.
- **This note:** aggregates only.

## Results

### Q1 — Instagram URL lifetime

| URLs in the capture | Unique | `oe` − capture, h: min / p5 / p25 / p50 / p75 / p95 / max | Valid 5.9 h after capture | Served to the VPS (23:21 UTC) | Served to the Mac |
|---|---|---|---|---|---|
| cover = poster (image) | 123 | 104.0 / 104.1 / 105.1 / 106.3 / 107.0 / 107.8 / 108.0 | 123 | **123 / 123** | 123 / 123 |

- **VPS fetch:** byte-identical to the Mac's copy 123 / 123, no block, TTFB p50 27 ms and p95 153 ms (`cdn-probe.mjs` at 1 request per second).
- **Spread:** the lifetime does not depend on the post's age: the median is 105.9 h for posts under 30 days old and 106.3 h for posts over a year old.

Video URLs as the VPS routes returned them, in hours left when the route answered:

| File | 32–35 h | 104–108 h |
|---|---|---|
| progressive `video_versions[0]` (post page and GraphQL) | 17 files (32.2–35.3) | 13 (104.0–108.0) |
| embed `video_url` (the same file as `video_versions[0]`, same `oe` within 1 h, 21 / 21) | 10 (32.7–35.3) | 11 (104.9–108.0) |
| DASH representation picked by yt-dlp | 10 (32.2–35.2) | 20 (104.1–107.9) |
| poster image | 0 | 30 (104.1–108.0) |

- **Per file:** the expiry is set for each file. The progressive file and the DASH file of one post can fall in different groups, and nothing in between was seen.
- **Video vs cover:** a video URL captured with its post can expire after about 32 h, three times sooner than the cover.
- **Logged-in URLs:** whether the logged-in API that the extension reads signs videos the same way is unmeasured, because the capture holds no video URL.

### Q2 — On-demand video

| Platform | Route | Valid video served to the VPS | Same post resolved from the Mac | Resolve s p50 / p95 | Requests per post | Failures |
|---|---|---|---|---|---|---|
| Instagram | post page | **30 / 30** | 30 / 30 | 2.5 / 3.5 | 1 (931 KB HTML) | — |
| | GraphQL | **30 / 30** | 30 / 30 | 2.4 / 2.9 | 1, plus 1 home page per 10 min | — |
| | embed | 21 / 30 | 21 / 21 | 3.7 / 4.0 | 1 | 9 embeds mark the post as a video but give no URL; the same from the Mac |
| | yt-dlp | **30 / 30** | 30 / 30 | 4.7 / 5.2 (extraction) | 3, plus 2 CDN | — |
| X | syndication | **29 / 30** | 29 / 29 | 0.2 / 0.2 | 1 | 1 post whose author is suspended: unavailable on every route and from the Mac |
| | yt-dlp | **29 / 30** | 28 / 28 | 4.0 / 4.3 (extraction) | 2, plus CDN | the same post |
| Pinterest | pidgets | **30 / 30** | — | 0.2 / 0.2 | 1 | — |
| | PinResource | **30 / 30** | — | 0.3 / 0.4 | 1 | — |
| | pin page | 28 / 30 | — | 1.2 / 1.4 | 1 (1 MB HTML) | 2 pages carried only Open Graph tags |
| | yt-dlp | **30 / 30** | — | 2.9 / 3.2 (extraction) | 1, plus CDN | — |

- **Pinterest reference:** "—" because it stopped after two pins (see "Blocks"). The 30 pins are public pins found anonymously minutes earlier.
- **yt-dlp timings:** they include its `--sleep-requests 1` waits.
- **Instagram:** every yt-dlp run warned "No CSRF token set by Instagram API" and still succeeded.

Downloaded files (VPS, full downloads):

| Platform | Path | Files | Output | MB p50 / p95 / max | Fetch s p50 / p95 | Resolve + fetch s p50 / p95 |
|---|---|---|---|---|---|---|
| Instagram | direct (`video_versions[0]`) | 30 | H.264 720p ×30 | 4.1 / 21.3 / 28.2 | 1.5 / 3.0 | 4.9 / 6.7 |
| | yt-dlp, plan format | 30 | VP9 1080p ×18, VP9 720p ×3, H.264 720p ×9 | 5.5 / 23.0 / 37.1 | 2.1 / 3.1 | 6.7 / 7.8 |
| X | direct (best MP4 ≤ 1080 px short side) | 29 | H.264 720p ×18, 1080p ×11 | 2.1 / 23.8 / 42.8 | 1.5 / 3.8 | 1.7 / 3.9 |
| | yt-dlp, plan format | 29 | H.264 720p ×16, 1080p ×4, above 1080p ×9 | 3.6 / 49.3 / 130.0 | 1.3 / 4.1 | 5.3 / 8.2 |
| Pinterest | direct (`V_720P`) | 30 | H.264 720p ×28, 1080p ×1, ≤ 480p ×1 | 5.5 / 39.9 / 59.8 | 2.2 / 3.3 | 2.3 / 3.4 |
| | yt-dlp, plan format | 30 | H.264 720p ×27, 1080p ×3 (4 of them over HLS) | 5.9 / 32.4 / 59.8 | 0.4 / 25.3 | 3.3 / 29.4 |

- **`moov` placement:** every file, 178 / 178, has `moov` before `mdat`, so it streams without a remux.
- **Silent videos:** 17 of the 29 X videos and 2 of the 30 Instagram videos have no audio track, on both paths.
- **Totals:** the direct paths fetched 731 MB for 89 videos; yt-dlp with the plan's format string fetched 981 MB for the same posts.
- **Instagram `video_versions`:** on a public test reel its three entries served the same file (H.264 720×1280, same size), so `[0]` loses nothing.

**What the plan's format string picks.** `bv*[ext=mp4]+ba[ext=m4a]/b[ext=mp4]/b`:
- **Instagram:** the 1080p DASH representation. That is VP9 for 21 / 30 posts; VP9 in MP4 does not play everywhere, notably not in older Safari.
- **X:** the largest progressive MP4. That is above 1080p for 9 / 29 posts: double the bytes, 130 MB for one post.
- **Pinterest:** HLS for 4 / 30 posts: up to 26 s and one request per fragment.

**A better format string.** `-S "proto:https,vcodec:h264,res:1080" -f "bv*+ba/b"`, applied offline (`--load-info-json`, no network) to the three public test posts:
- **Instagram:** the progressive H.264 720p file;
- **X:** the 1080p progressive MP4;
- **Pinterest:** `V_720P`.

Each is one request.

### Q3 — Link hydration

| Platform | Endpoint | Answered | Caption (= library) | Author (= library) | Date | Image | Video URL |
|---|---|---|---|---|---|---|---|
| Instagram | post page | 30 / 30 | 29 (28 / 28) | 30 (30 / 30) | 30 | 30 | 30 |
| | GraphQL | 30 / 30 | 29 (28 / 28) | 30 (30 / 30) | 30 | 30 | 30 |
| | embed | 30 / 30 | 29 (28 / 28) | 30 (30 / 30) | 0 | 30 | 21 |
| | yt-dlp | 30 / 30 | 29 (28 / 28) | 30 (30 / 30) | 30 | 30 | 30 |
| X | `tweet-result` | 29 / 30 | 29 (27 / 27) | 29 (29 / 29) | 29 | 29 | 29 |
| | oEmbed | 29 / 30 | 29 (22 / 27) | 29 (29 / 29) | 29 | 0 | 0 |
| | yt-dlp | 29 / 30 | 29 (27 / 27) | 29 (29 / 29) | 29 | 28 | 28 |
| Pinterest | PinResource | 30 / 30 | 30 (reference) | 30 (reference) | 30 | 30 | 30 |
| | pidgets | 30 / 30 | 30 (30 / 30 = PinResource) | 30 (30 / 30) | 0 | 30 | 30 |
| | pin page | 30 / 30 | 30 (28 / 30) | 28 (28 / 28) | 28 | 30 | 28 |
| | oEmbed | 30 / 30 | 30 (19 / 30) | 30 (30 / 30) | 0 | 30 | 0 |
| | yt-dlp | 30 / 30 | 30 (4 / 30) | 30 (19 / 30) | 30 | 30 | 30 |

- **How captions are compared:** on their first 40 letters and digits, after removing URLs and entities. One Instagram post has no caption, and one is too short to compare.
- **X oEmbed:** its text keeps the media link as plain text (`pic.x.com/…`). That is the likely cause of its 5 mismatches, which inside a 40-character prefix only a short caption would show. It returns no media.
- **Pinterest:**
  - oEmbed's caption is the pin title.
  - yt-dlp returns a different description field and the creator's display name, not the pinner the desktop stores.
  - The Instagram embed has no date. The post's date can be recovered from its id: the desktop decodes it from the shortcode.

### Pinterest images (SPIKE-2's missing sample)

`video-pins.mjs --images-out`, then `cdn-probe.mjs` and `cdn-compare.mjs`, 45 pins, 1 request per second:

| Variant | Served to the Mac | …and to the VPS | Size p50 / max | Note |
|---|---|---|---|---|
| widest served (`564x`) | 45 / 45 | **45 / 45** | 64 / 155 KB | byte-identical 45 / 45 |
| `/1200x/` (plan §2.13) | 45 / 45 | **45 / 45** | 155 / 538 KB | byte-identical 45 / 45 |
| `/originals/` (desktop) | 42 / 45 | **42 / 42** | 131 / 517 KB | 3 pins keep no original: 403 from both vantage points |

### Blocks and throttling

- **VPS:** no 429, challenge or login wall in 360 route runs, and no route or platform stopped.
  - **Instagram:** about 180 requests to `www.instagram.com` in 9.5 min, resolvers and yt-dlp together.
  - **X:** 60 resolver requests, plus 60 from yt-dlp.
  - **Pinterest:** 120 requests to `www.pinterest.com` in 6 min.
  - **Trend:** the success share did not fall during any run: Instagram 90 % in the first third vs 93 % in the last, X 100 % vs 100 %, Pinterest 98 % vs 98 %. The misses are the embed's missing URLs and the page-only pins.
- **Mac reference:** the second Pinterest pin page answered 429, so the stop rule ended the Pinterest reference. It was that IP's eighth pin-page load in about an hour, six of them while the probe was being written, with about as many PinResource calls in between. PinResource and pidgets were never refused.

## Decision

### P2 — archiving and hydration

1. **Archive mode: `server` for all three platforms.**
   - **Instagram:** this capture's URLs served 123 / 123 to the VPS, and SPIKE-2 measured 280 / 280.
   - **X:** SPIKE-2 measured 300 / 300.
   - **Pinterest:** 132 / 132 working image URLs, including 45 / 45 `/1200x/`, and 30 / 30 video files.
   - **Breaker:** unchanged.
2. **Parser change (§2.4): keep a direct video URL for every video slide**, plus its own `oe` for Instagram:
   - Instagram `video_versions[0]`;
   - X, the best MP4 variant whose short side is at most 1080 px;
   - Pinterest `V_720P`, else the widest MP4.

   Schedule every URL by its own `oe`: an Instagram video can expire 32 h after capture, its cover after 104 h.
3. **Link hydration (`link.hydrate`):**
   - **X:** `tweet-result` returns text, author, date, media and video variants in one request (0.2 s). Fallback: `publish.x.com/oembed`, which has text, author and date but no media. A tombstone, a 404 or an empty answer means `unavailable`.
   - **Pinterest:** PinResource with `unauth_react_main_pin` returns every field, `created_at` included, in one request. Fallback: pidgets `pins/info`, on its own host, with no date. Not the pin page: 1 MB, 2 / 30 without data, and a 429 on a residential IP's eighth load in an hour.
   - **Instagram:** server-side as well. This changes §2.17 and D18, which give Instagram hydration to the extension because "IG pages are login-walled from datacenters", and that did not hold here.
     - **Order:** post page, then GraphQL, then the extension, which also takes posts the server finds gated (`if_not_gated_logged_out` is null) and every post while the Instagram breaker is open.
     - **Embed:** only as a last resort, since it has no date and no video for 30 %.
   - **Budget:** at most 1 request per 3 s to `www.instagram.com`, at most 1 per second to the other hosts, with the per-host breaker of §2.13.

### P4 — the on-demand video chain (replaces D15's order)

```
cached or kept ─────────────────────────────────────────────► stream /media/<sha>.mp4
stored direct URL (Instagram: oe > now + 10 min) ───────────► media.video: server fetch
server resolver (the P2 hydration call) ────────────────────► fetch the MP4 it returns
  Instagram: post page → GraphQL · X: tweet-result · Pinterest: PinResource → pidgets
yt-dlp, anonymous, all three platforms ─────────────────────► -S "proto:https,vcodec:h264,res:1080" -f "bv*+ba/b"
extension paired ───────────────────────────────────────────► refresh_media / upload_media task
otherwise ──────────────────────────────────────────────────► "Open original"
```

- **When the client path takes over.** The server hands a post to the extension in three cases:
  - a block signal (429, challenge or login wall), which opens the platform breaker for 30 minutes;
  - a gated, private or age-restricted post, for which the anonymous routes answer `gated`, a tombstone or "login required";
  - every server route failing for that post.

  The extension is no longer the step before yt-dlp, because the server routes succeed at 97–100 % without the user's uplink.
- **Instagram behind yt-dlp is no longer opt-in.** It succeeded 30 / 30, and it is the last server step behind two cheaper resolvers. The owner keeps a per-route kill switch.
- **yt-dlp format.** Replace §2.13's `-f "bv*[ext=mp4]+ba[ext=m4a]/b[ext=mp4]/b"` with `-S "proto:https,vcodec:h264,res:1080" -f "bv*+ba/b" --merge-output-format mp4`. The plan's string produced VP9, outputs above 1080p and HLS (see Q2).
- **No faststart remux needed.** All 178 files streamed as fetched. Remux only when `moov` comes after `mdat`.
- **Capacity inputs:**
  - **Size:** videos at 1080p or less are 2–6 MB at p50, 21–40 MB at p95 and 60 MB at most, so the 300 MB cap is never close. The 5 GiB video LRU holds about 1,000 videos at p50.
  - **Latency:** resolve plus fetch takes 1.7 s (X), 2.3 s (Pinterest) and 4.9 s (Instagram) at p50, and yt-dlp 3.3–6.7 s.

## Risks and what to re-check

1. **Instagram's logged-out access is a policy, not a contract.** It was open to this IP at about 19 requests a minute, and Instagram has walled datacenter traffic before. Mitigations: the breaker, the extension fallback, the kill switches, and a daily canary on one public post per route, which feeds the breaker metric.
2. **Endpoint drift.**
   - **Instagram GraphQL:** the doc_id, the friendly name and the `X-ASBD-ID` changed between yt-dlp 2026.02.04 and 2026.08.19.
   - **Instagram embed:** it serves its data only to cross-site iframe loads (`Sec-Fetch-*`).
   - **X:** the syndication token is reverse-engineered.
   - **Pinterest:** PinResource is an internal API.
   - Mitigation: keep the post page first for Instagram and the widget API as Pinterest's fallback, version the resolvers as data, and re-run this probe after every yt-dlp bump.
3. **The client differs.** The probe spoke HTTP/1.1 through Node; production will speak through reqwest and rustls, whose TLS fingerprint differs. Re-run the routes through the Rust client in P4.
4. **Small samples, one window.** 30 / 30 means at least 90 % at 95 % confidence (rule of three). One IP and one half-hour window at the paces above prove nothing about higher rates.
5. **Pinterest pages rate-limit fast.** A residential IP got a 429 on its eighth pin page in an hour: keep page scraping out of every server path.
6. **Unmeasured:** the lifetime of logged-in `video_versions` URLs, and Pinterest posts saved by the owner, since the sample is brand pins.
7. **Hosting third-party media:** R15 is unchanged.

**Re-check:**
- the first P2 extension ingest with the new parser: log `oe − capturedAt` for Instagram video URLs, expecting at least 32 h;
- the O1 exports for X and Pinterest, re-run with `video-sample.mjs --pins`;
- every yt-dlp bump: the yt-dlp route of each platform.

## Deviations and assumptions

- **Q1 without video URLs.** The capture holds no `video_versions`, because the parsers are unchanged since SPIKE-3. Q1 measured its image URLs, and the video lifetime was measured on the URLs the anonymous routes returned.
- **Capture time.** The capture's file name stamp is UTC (17:10 UTC, 19:10 local), not local time.
- **Node in the production image.** The image has no Node, curl or Python, so the official Node 24.20.0 linux-x64 binary was mounted into it read-only after a SHA-256 check, and deleted at the end.
- **The first container.** It kept the image's health check, which reported it unhealthy, harmlessly; the later ones ran with `--no-healthcheck`. `v0.1.0-rc.3` was deployed during the run, and the spike used `rc.2` as specified.
- **yt-dlp flags.** It ran without the desktop's `--user-agent` and without a proxy, with `--sleep-requests 1` for pacing.
- **Stop rule.** A platform stops at the first 429 or challenge; a single route stops after two login walls in a row, so that one private post cannot stop a platform. No login wall occurred.
- **Pinterest sample.** Public brand pins, not owner action O1. O1 is still needed to re-check the owner's own boards.
- **Shared downloads.** Each post's CDN file was downloaded once; the other routes that resolved the same file checked their URL with a 1 KiB range request.
- **Added beyond the brief.** The Pinterest image check, to settle SPIKE-2's open `auto`. The offline yt-dlp format comparison, on public posts only.

## Reproduce

```sh
# Q1, aggregates and a cdn-probe sample (personal data: keep outside the repo)
node scripts/spikes/video-lifetime.mjs --capture <capture.json> --out /tmp/capture.tsv
node scripts/spikes/cdn-probe.mjs --in /tmp/capture.tsv --out /tmp/q1-mac.jsonl --label mac --rate 1

# Samples
node scripts/spikes/video-pins.mjs --out /tmp/pins.txt
node scripts/spikes/video-pins.mjs --from /tmp/pins.txt --images-out /tmp/pin-images.tsv
node scripts/spikes/video-sample.mjs --db <shelfy.sqlite> --pins /tmp/pins.txt --out /tmp/sample.tsv

# On the VPS: Node mounted into the production image, one platform per container
docker run --rm --name shelfy-spike9-ig --no-healthcheck --cpus 0.5 --memory 768m \
  --cap-drop ALL --security-opt no-new-privileges \
  -v /tmp/shelfy-spike9/node:/opt/node:ro -v /tmp/shelfy-spike9/work:/work \
  --entrypoint /opt/node/bin/node ghcr.io/niccolofanton/shelfy-api:v0.1.0-rc.2 \
  /work/video-probe.mjs --in /work/sample.tsv --out /work/vps.jsonl --label vps \
  --platforms instagram --work /work/dl --ytdlp /usr/local/bin/yt-dlp \
  --ffmpeg /usr/bin/ffmpeg --ffprobe /usr/bin/ffprobe
# (same for x and pinterest; cdn-probe.mjs on capture.tsv and pin-images.tsv with --rate 1)

# Residential reference, then the report
node scripts/spikes/video-probe.mjs --in /tmp/sample.tsv --out /tmp/mac.jsonl --label mac --resolve-only
node scripts/spikes/video-report.mjs --test /tmp/vps.jsonl --ref /tmp/mac.jsonl
node scripts/spikes/cdn-compare.mjs --ref /tmp/q1-mac.jsonl --test /tmp/q1-vps.jsonl
```
