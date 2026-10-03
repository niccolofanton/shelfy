# SPIKE-3 — Extension capture parity (Chrome MV3)

> **Note (2026-10-03):** the spike build described here lives on branch `web/t5-extension-spike`. On `web/foundations`, `extension/` is now the P2 extension (P2-06).

> **Status:** extension and comparison tooling built (lane T5, 2026-10-02); **owner run pending.**
> **Plan refs:** [IMPLEMENTATION-PLAN.md](../IMPLEMENTATION-PLAN.md) §0 (D13, D14, D15), §2.16, §9 (SPIKE-3), §10 (T5). Feature refs: [02-social-sync-and-downloads.md](../features/02-social-sync-and-downloads.md) (SYNC-06…16).

## 1. Questions and pass criteria

| # | Question (plan §9) | Pass criterion | How this spike answers it |
|---|---|---|---|
| Q1 | Does a MAIN-world `document_start` hook capture what the desktop captures? | **≥ 99 % item parity** per listing: matched desktop posts / desktop posts ≥ 0.99 on IG saved, one IG folder, X bookmarks and one Pinterest board | `compare.ts` on the extension export vs a desktop sync run of the same listings |
| Q2 | Does the IG replay run from the extension? | the replay pages through the listing (`end_of_feed`, or `page_cap` at 100 pages) without HTTP errors, and its batches arrive tagged `replay` | "Run IG replay" in the side panel (`chrome.scripting`, MAIN world) |
| Q3 | Do background tabs keep lazy-loading? | behavior documented (not a pass/fail gate; the remaining 0.5 d is scheduled at P2 start) | optional observation in step 6 below |

Extra items (extension only) do not count against Q1, but each one must be explainable (for example saved between the two runs). Report parity twice: with every source, and passive only (`--exclude-source replay`).

## 2. What was built

```
 page MAIN world (document_start)              ISOLATED world           service worker          side panel
 electron/webview-injected.ts (unchanged) ─┐
   fetch/XHR patch + desktop parsers       │ window.postMessage
 main/census.ts   (request counts)        ─┼──────────────────► bridge.ts ─runtime──► sw.ts ──► storage ◄── panel.ts
 main/passive.ts  (Pinterest SSR, X DOM)  ─┤   (event.source    (relay.ts:  .sendMessage  sanitize,        live log,
 replay.ts        (via chrome.scripting)  ─┘    === window)      source tag) + ack/retry  key, merge       counters, export
```

| File (`extension/`) | Runs in | Role |
|---|---|---|
| `src/hook.main.ts` → `hook.main.js` | page MAIN world, `document_start`, top frame | bundles `electron/webview-injected.ts` **unchanged**; with no contextBridge its relay takes the existing `postMessage({type: 'SOCIAL_SAVED_INTERCEPT', items, hasNextPage, platform})` fallback. Then installs the census and the passive helpers |
| `src/main/passive.ts` | MAIN | calls the hook's own `__ssReplayPinterest()` at DOMContentLoaded on a board page (Pinterest server-renders page 1 inline) and `__ssScanTwitterBookmarks()` while the owner scrolls X bookmarks (every 750 ms, the desktop's settle time). Both read the DOM only |
| `src/main/census.ts` | MAIN | counts the requests whose responses the hook parses (same URL tests as `matchPlatform`), labelled by endpoint and IG GraphQL friendly name. No body is read, no request is changed |
| `src/bridge.ts`, `src/relay.ts` | ISOLATED, `document_start` | the `webview-preload.ts` rule (`event.source === window`), then `chrome.runtime.sendMessage` with acknowledgement and retry; tags each batch with its source (`passive`, `replay`, `ssr`, `dom`); drops captions and author fields; answers the panel's ping |
| `src/sw.ts`, `src/store.ts` | service worker | checks the sender (top frame, host = declared platform = page URL host), runs the desktop's `sanitizeInterceptedBatch`, derives canonical keys (`src/identity.ts`, plan §2.8) and the listing (`src/listing.ts`), merges into `chrome.storage.local`, pushes state to the panel. No in-memory state, so worker suspension is harmless |
| `src/panel/*` | side panel | active-tab status, IG replay controls, per-platform and per-listing counters, census, live log, **Export JSON**, **Clear data** |
| `src/replay.ts` | MAIN, injected on demand | port of `IG_FEED_REPLAY` as a function (MV3 forbids code strings): same endpoints, app-id discovery and fallback, 700 ms gap, ≤ 100 pages, `__syncStop` |
| `src/manifest.ts`, `build.ts` | build | generates `manifest.json`, bundles with esbuild, runs the sanity check |
| `scripts/compare.ts`, `scripts/compare-lib.ts` | Node | parity report (§6) |
| `scripts/smoke.ts` | Node + Chromium | real-browser smoke test on synthetic pages (§4) |

**Scoping (plan §2.16).** A batch is kept only when the relaying page is a saved listing: IG `/<user>/saved/all-posts/`, IG folders `/<user>/saved/<slug>/<id>/` (and the folder index, kept as its own listing and never compared), X bookmarks (always: the hook only accepts bookmark responses), Pinterest board pages `/<user>/<board>/[<section>/]` (sections count as the board, as on the desktop). Everything else is counted as discarded.

**Data kept.** Per post: canonical key, raw ids, shortcode, post URL, media type and count, post date, listing memberships with their sources and capture times, and per media slot the latest URL with host, capture time and IG/FB `oe` expiry. Captions and author fields never leave the page bridge.

### Deviations from the plan, for this spike

| Plan (§2.16 / §10 T5) | Spike build | Why |
|---|---|---|
| Sync controller out of scope | adds an **owner-triggered IG REST replay** (no scroll automation, no planner, no termination rules) | IG server-renders the first page of every saved listing inline, so no hook sees it (desktop comment in `browserScripts.ts`: a 65-item folder imports as 53 without the replay); the desktop's IG data comes from this replay (aggregate read-only check: every IG id in the reference library has the REST `<pk>_<owner>` form); and Q2 asks for it |
| Manifest permissions | `storage`, `unlimitedStorage`, `scripting`, `sidePanel`; social hosts only | passive capture needs no CDN hosts, Shelfy origin, `externally_connectable` or `alarms`; the sanity check refuses anything else |
| Bridge "over a chrome.runtime port" | one-shot `runtime.sendMessage` with acknowledgement and retry | each delivery is confirmed and wakes a suspended worker; a port can drop a message while the worker is torn down |
| Keep IG `video_versions`, X `video_info.variants`, emit IG `oe` (§2.4) | parsers unchanged (desktop untouched); `oe` is parsed from the URLs in the worker | video URLs stay a P2 parser change (SPIKE-9); IG/X video slides are exported as `poster` URLs, Pinterest keeps direct MP4/HLS URLs |
| `PINTEREST_HOSTS` exported from `src/lib/browserUrls.ts` | `extension/src/hosts.ts`, unit-tested against the desktop's `isAllowedUrl` | the desktop stays untouched |
| — | MAIN-world request census | makes a parity failure diagnosable (requests seen vs items parsed) |

## 3. Build and load

```sh
pnpm install --frozen-lockfile        # once
pnpm exec tsx extension/build.ts      # → extension/dist + sanity check
```

The build prints the file sizes and fails on any sanity-check problem: manifest version and limits, permission and host allowlists, valid match patterns, hook in the MAIN world at `document_start`, bridge ISOLATED at `document_start`, top frame only, every referenced file present, the desktop hook markers present in `hook.main.js`, and no `eval`, `new Function`, runtime imports, `__name()` helpers, inline or remote scripts.

Load it in Chrome (stable is fine for loading unpacked; minimum 120):

1. `chrome://extensions` → enable **Developer mode** → **Load unpacked** → select `extension/dist`.
2. Pin "Shelfy capture spike (SPIKE-3)" in the toolbar; clicking it opens the side panel.
3. After each rebuild: press the reload icon on the extension card, then **reload every open Instagram, X and Pinterest tab** (content scripts are injected on page load only). The panel shows "Capture: not loaded in this tab" until you do.

## 4. Smoke test (optional, no network)

```sh
pnpm exec tsx extension/scripts/smoke.ts [--chrome <Chromium or Chrome for Testing binary>] [--headed]
```

Loads `extension/dist` in Chromium and serves synthetic Instagram, X and Pinterest pages from `extension/tests/fixtures` through Playwright route interception; every other request is aborted. It checks the service worker, the bridge ping, the IG replay through `chrome.scripting.executeScript({world: 'MAIN'})` with the bundled function, storage contents with listing and source, discarding outside saved listings, the side panel, the exported file, console errors and that no request left the browser. Branded Google Chrome ignores `--load-extension` since Chrome 137: use Playwright's Chromium (`pnpm exec playwright install chromium`) or pass a Chrome for Testing binary. Passed on Chrome for Testing build 1243 on 2026-10-02.

## 5. Owner run

Do the desktop run and the extension run on the same day, without saving or unsaving anything in between. Keep every file under `/Users/fant/work/experiments/shelfy-web-local/spike-data/` (outside the repo).

### Step 0 — prepare

```sh
SPIKE=/Users/fant/work/experiments/shelfy-web-local/spike-data
mkdir -p "$SPIKE"
```

Pick the IG folder and the Pinterest board to test: one of each, ideally with 50–300 items.

### Step 1 — desktop reference run (fresh profile)

A fresh profile makes the desktop DB contain exactly this run. The existing library would also hold every post ever imported, including posts unsaved since, which would show up as "missing".

1. From the repo root: `SHELFY_TEST_USER_DATA="$SPIKE/desktop-profile" pnpm run dev` (honored by unpackaged runs only; see the top of `electron/main.ts`). Accept the disclaimer.
2. In the desktop **Browser** view, log in to Instagram, X and Pinterest.
3. **IG saved:** Instagram tab → your profile → Saved → All posts → **Auto-import**. Wait until the sync stops on its own. Note the counters (scanned / new).
4. **IG folder:** Saved → the chosen folder → **Auto-import** → in "Import Instagram folder" choose **Add to a folder tag** → **Create new tag…** (keep the proposed name) → confirm. Wait, note the counters. Without the tag the desktop records no folder membership and `compare.ts` cannot isolate the folder.
5. **X bookmarks:** X tab (opens Bookmarks) → **Auto-import** → wait → note the counters.
6. **Pinterest board:** Pinterest tab → the chosen board → **Auto-import** → **Add to a folder tag** → **Create new tag…** → confirm → wait → note the counters.
7. Quit the desktop app (the DB is checkpointed on exit). The DB is `$SPIKE/desktop-profile/shelfy.sqlite`.

Fallback (not recommended): `--desktop-db` on a copy of the installed library (`~/Library/Application Support/SHELFY/shelfy.sqlite`, app quit first) or `--desktop-export` with a JSON export from Settings. Then "missing" may list posts that are no longer saved; check those links by hand.

### Step 2 — extension: IG saved

1. Chrome, logged in to Instagram, extension loaded (§3). Open the side panel, click **Clear data**, then click it again within 4 s to confirm. Do this once, before the first listing.
2. Type `https://www.instagram.com/<username>/saved/all-posts/` in the address bar (a hard load). The panel shows *Listing: Instagram · saved (all posts)* and *Capture: active*.
3. **Passive pass:** scroll to the bottom of the grid until nothing more loads (End key, or in DevTools: `t = setInterval(() => scrollBy(0, innerHeight * 0.55), 650)`, stop with `clearInterval(t)`). Watch the *Listings* row (`passive` column) and the census table.
   - If after a few screens the census shows in-scope `graphql …` requests but the `passive` count stays at 0, stop scrolling and write it down (that is a Q1 finding: the page returns saved items in a shape the parsers do not read). Go on with the replay.
4. **Replay pass:** in the panel's *Instagram replay* box keep *Pages* at 100 and click **Run IG replay**. Do not scroll. Wait for *Replay finished: N pages, end_of_feed* (or `page_cap`). Note N and the reason.
5. If the reason is `page_cap`, the listing is longer than 100 pages: scroll to the bottom again, as the desktop's scroll loop does after its replay.

### Step 3 — extension: IG folder

Type `https://www.instagram.com/<username>/saved/<slug>/<folderId>/` (copy it from the address bar after opening the folder, then reload the tab). The panel shows *Listing: Instagram · folder …*. Repeat the passive pass and the replay pass of step 2.

### Step 4 — extension: X bookmarks

Open `https://x.com/i/bookmarks` (a hard load; `/i/history` with the Bookmarks tab selected works too). Scroll to the end at a pace that lets the cards render (the DOM scan runs at most every 750 ms). The *End* column turns `yes` when X returns its last cursor.

### Step 5 — extension: Pinterest board

Open `https://www.pinterest.<tld>/<username>/<board>/` (a hard load: page 1 is read from the page's inline JSON and appears in the `ssr` column). Scroll to the end; *End* turns `yes` on Pinterest's end-of-feed cursor.

Then click **Export JSON** and move the downloaded `shelfy-spike3-capture-<UTC stamp>.json` to `$SPIKE`.

### Step 6 — optional: background tabs (Q3)

1. On the IG saved listing set *Pages* to 20, click **Run IG replay**, switch to another tab for 2 minutes, come back. Compare the log timestamps between replay pages before and after the switch (foreground ≈ 0.7 s plus the request time; Chrome throttles timers in hidden tabs, more aggressively after 5 hidden minutes).
2. Open the X bookmarks listing in a background tab and do not scroll it: note whether anything is captured (passive capture depends on the page loading content, which needs scrolling).
3. Record pages per minute in foreground and background in the results.

### Step 7 — compare

```sh
pnpm exec tsx extension/scripts/compare.ts \
  --extension "$SPIKE/shelfy-spike3-capture-<stamp>.json" \
  --desktop-db "$SPIKE/desktop-profile/shelfy.sqlite" \
  --json "$SPIKE/spike3-report-all.json"

pnpm exec tsx extension/scripts/compare.ts \
  --extension "$SPIKE/shelfy-spike3-capture-<stamp>.json" \
  --desktop-db "$SPIKE/desktop-profile/shelfy.sqlite" \
  --exclude-source replay \
  --json "$SPIKE/spike3-report-passive.json"
```

On Node 22 `node:sqlite` prints an ExperimentalWarning; it is harmless. Copy the two tables and the replay outcome into §8. Keep the JSON reports (they list every missing and extra id) in `$SPIKE`, not in the repo.

## 6. Comparison tool (`extension/scripts/compare.ts`)

**Inputs.** `--extension` (the panel export) and exactly one of `--desktop-db` (opened read-only through `node:sqlite`, Node ≥ 22.13, with `PRAGMA query_only`) or `--desktop-export` (the desktop's JSON export). Options: `--listing <key>` (repeatable; default: every comparable listing in the export), `--exclude-source <source>`, `--threshold` (default 0.99; `99%` also accepted), `--json <file>`, `--show <n>`. Exit codes: 0 all pass, 1 a listing fails, 2 usage or input error.

**Desktop data read**, and nothing else:

| Source | Fields |
|---|---|
| DB `posts` | `id`, `platform`, `shortcode`, `post_url`, for `platform IN ('instagram', 'twitter', 'pinterest')` |
| DB `collections` | `id`, `platform`, `external_id`, `name`, for `platform IN ('instagram', 'pinterest')` with an `external_id` (IG folders: numeric id; Pinterest boards: `user/slug`) |
| DB `post_collections` | `post_id`, `collection_id` |
| JSON export | `posts[].id / platform / shortcode / postUrl / collections` (`x:<externalId>` keys) and `collections[].platform / externalId / name` |

No captions, media, file paths or AI fields are read.

**Listing mapping.**

| Listing key | Desktop set | Extension set |
|---|---|---|
| `instagram:ig_saved` | every Instagram post | every IG item on `ig_saved` or any `ig_collection` (the desktop has no per-listing record either) |
| `instagram:ig_collection:<folderId>` | posts in the collection `(instagram, <folderId>)` | items on that listing |
| `twitter:x_bookmarks` | every X post | every X item |
| `pinterest:pin_board:<user>/<board>` | posts in the collection `(pinterest, <user>/<board>)`; without it, every Pinterest post (noted in the report) | items on that listing; in the fallback, every Pinterest item |

**Matching.** Both sides go through `canonicalIdentity` (§2.8): IG `<pk>_<owner>`, `<pk>` and shortcodes collapse onto `ig_<pk>` (plus a verbatim-shortcode alias), X and Pinterest use their numeric ids with the post URL as fallback. Desktop rows sharing any alias count as one post (the duplicate variants of 02 risk 4). Parity = matched desktop posts / desktop posts. The text report lists the first `--show` missing (desktop only) and extra (extension only) ids with their post URLs; the JSON report lists all of them.

## 7. Export format (`shelfy-spike3-capture`, version 1)

Unix-ms timestamps throughout. SPIKE-2 samples its URLs from `items[].media`.

```jsonc
{
  "format": "shelfy-spike3-capture", "version": 1,
  "exportedAt": "2026-10-02T12:00:00.000Z",
  "extension": { "version": "0.1.0", "userAgent": "…" },
  "storeCreatedAt": 1759400000000,
  "platforms": { "instagram": { "uniqueItems": 0, "batches": 0, "itemsReceived": 0, "discardedBatches": 0,
                 "discardedItems": 0, "rejectedItems": 0, "lastCaptureAt": null,
                 "census": { "graphql <FriendlyName>": { "requests": 0, "inScope": 0 } } }, "twitter": {…}, "pinterest": {…} },
  "diagnostics": { "igShortcodeMismatch": 0, "refusedBatches": {} },
  "listings": [ { "key": "instagram:ig_collection:<folderId>", "platform": "instagram", "kind": "ig_collection",
                  "externalId": "<folderId>", "name": "<slug>", "account": "<user>", "uniqueItems": 0,
                  "bySource": { "passive": 0, "replay": 0, "ssr": 0, "dom": 0 }, "batches": 0,
                  "itemsReceived": 0, "endOfFeedSeen": true, "lastHasNextPage": false, "firstSeenAt": 0, "lastSeenAt": 0 } ],
  "items": [ { "key": "ig_<pk>", "platform": "instagram", "nativeId": "<pk>", "rawIds": ["<pk>_<owner>"],
               "shortcode": "…", "postUrl": "https://www.instagram.com/p/…/", "mediaType": "carousel", "mediaCount": 3,
               "postedAt": "…", "firstCapturedAt": 0, "lastCapturedAt": 0, "captureCount": 2,
               "listings": [ { "key": "instagram:ig_saved", "sources": ["passive", "replay"], "firstCapturedAt": 0, "lastCapturedAt": 0 } ],
               "media": [ { "slot": "cover", "position": null, "type": "image", "urlKind": "image",
                            "url": "https://…cdninstagram.com/…&oe=…", "host": "…", "capturedAt": 0, "expiresAt": 0 } ] } ]
}
```

`media[].slot` is `cover` (the item's thumbnail) or `slide` (`position` 0…n); `urlKind` is `image`, `poster` (the image the parsers keep for an IG/X video) or `video` (a direct Pinterest MP4/HLS URL); `expiresAt` is set only for signed IG/FB CDN URLs; each slot keeps its latest URL.

## 8. Results — partial owner run (2026-10-02)

| Listing | Desktop posts | Extension items | Matched | Missing | Extra | Parity (all sources) | Parity (passive only) | Pass |
|---|---|---|---|---|---|---|---|---|
| IG saved | | | | | | | | skipped: same replay as the folder, about 4,000 posts |
| IG folder (123 posts) | 123 | 123 | 123 | 0 | 0 | 100.00 % | 0.00 % | PASS with the replay |
| X bookmarks | — | 20 (first page only) | — | — | — | n/a | n/a | pending: no desktop X import, list not scrolled to the end |
| Pinterest board `<user>/<board>` | | | | | | | | pending |

| Item | Value |
|---|---|
| Desktop counters per listing (scanned / new) | pending |
| IG replay (Q2): pages, reason, last HTTP status, app id source, per listing | IG folder: 11 pages of `/api/v1/feed/collection/:id/posts/`, `end_of_feed`, 0 refused batches, 0 shortcode mismatches |
| Census: IG GraphQL friendly names seen in scope, and whether they produced passive items | `PolarisProfilePostsTabContentQuery_connection` was in scope (1 request) and produced 0 items, which is risk 3. Out of scope: `PolarisProfilePostsQuery`, `PolarisStoriesV3AdsPoolQuery`. X: `Bookmarks` gave 20 items; `BookmarkFoldersSlice` was in scope. |
| Background tabs (Q3): replay pages per minute foreground vs background; X capture in a background tab | pending |
| Explanation of every extra and missing item | pending |
| Decision: passive capture sufficient, or replay required per platform (input to the P2 sync controller) | Instagram: replay required. X and Pinterest: pending the rest of the run. |

## 9. Known risks

1. **Background-tab lazy loading (R16).** Hidden tabs throttle timers (harder after 5 hidden minutes) and nobody scrolls them, so nothing lazy-loads without an active controller. Unattended syncs stay opt-in until Q3 is measured; the P2 controller lives in the tab and may need a visible, minimized window.
2. **IG replay from the extension.** Verified only on synthetic pages (unit tests and the real-Chrome smoke test). On the live site it can meet HTTP 429, checkpoints, a changed endpoint or app-id scheme; it sends the same requests as the desktop at the desktop's pace (700 ms, ≤ 100 pages). It runs in the page's MAIN world, so the requests carry the page's own cookies, origin and headers.
3. **IG first page and GraphQL shapes.** The first page of a saved listing is server-rendered inline, and the passive walker only reads the legacy `edges[].node.shortcode` shape. If current IG pages deliver saved items in another shape, passive-only parity on IG will be low and the replay remains required, as on the desktop. The census shows which queries the page made.
4. **Detectability.** The hook patches `fetch`/`XMLHttpRequest`, sets `window.__socialSaved*`/`__ss*` globals and relays through page-visible `postMessage`, exactly as the desktop's fallback path; a platform can detect it. Requests from workers or other frames bypass the top-frame hook (also true on the desktop).
5. **Listing attribution.** A batch is attributed to the page URL at relay time; a response that lands after a client-side navigation to another listing is filed under the new one. Do not navigate while a listing is loading.
6. **Pinterest ownership.** The plan limits passive Pinterest capture to the user's own boards; the spike accepts any board page (the owner visits their own).
7. **Video URLs.** The parsers keep only the poster for IG and X videos; direct video URLs wait for the parser change of §2.4 and SPIKE-9.
8. **Desktop baseline.** The desktop records no per-run membership, so only a fresh profile gives an exact baseline; IG saved and X compare whole platforms on both sides.

## 10. Privacy

The extension stores data only in `chrome.storage.local` of the owner's browser, never sends it anywhere, and exports only on click. Captions and author fields are dropped in the bridge. Exports, desktop profiles and reports stay in `shelfy-web-local/spike-data/`, outside the repo. All fixtures under `extension/tests/fixtures` are synthetic.
