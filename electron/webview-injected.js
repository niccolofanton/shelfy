'use strict';

// Runs in the webview's MAIN WORLD (injected via executeJavaScript on dom-ready
// / did-finish-load). executeJavaScript bypasses the page CSP, which a DOM
// <script> tag would not. Patches fetch + XMLHttpRequest to capture saved-feed
// / bookmark API responses and relays them to the host via the isolated bridge.
//
// With the webview hardened (nodeIntegration=false, contextIsolation=true) the
// MAIN world has no `require('electron')`. Capture is relayed exclusively
// through `window.__socialSavedBridge`, exposed by webview-preload.js via
// contextBridge, which forwards to the host with ipcRenderer.sendToHost.

(function () {
  if (window.__socialSavedInjected) return;
  window.__socialSavedInjected = true;

  const MSG_TYPE = 'SOCIAL_SAVED_INTERCEPT';

  // Relay via the contextBridge exposed by the isolated preload. As a defensive
  // fallback (e.g. if the bridge is not yet present), use a same-origin
  // postMessage that webview-preload.js also listens for.
  let _relay = null;
  if (window.__socialSavedBridge) {
    _relay = (items, hasNextPage, platform) =>
      window.__socialSavedBridge.send(items, hasNextPage, platform);
  } else {
    _relay = (items, hasNextPage, platform) =>
      window.postMessage({ type: MSG_TYPE, items, hasNextPage, platform }, window.location.origin);
  }

  // Coerce a unix-seconds timestamp (the field IG/Twitter expose under various
  // names) into an ISO string, tolerating missing/garbage values. This is the
  // post date persisted to the DB and used for date display + date sorting.
  function toIsoSeconds(sec) {
    const n = Number(sec);
    if (!Number.isFinite(n) || n <= 0) return '';
    try {
      return new Date(n * 1000).toISOString();
    } catch {
      return '';
    }
  }

  // Derive a post's creation date from its Instagram shortcode. IG media ids are
  // 64-bit values whose top 41 bits are milliseconds since the IG epoch
  // (2011-08-24); the shortcode is that id in base64. Lets us still date posts
  // captured from the LIGHTWEIGHT grid GraphQL nodes (which carry a shortcode but
  // no taken_at / no numeric id), so manually-selected posts aren't left undated.
  const IG_SC_ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
  function igTimestampFromShortcode(shortcode) {
    if (!shortcode || typeof shortcode !== 'string') return '';
    // Real IG shortcodes are <= ~12 chars; cap the length before the BigInt loop
    // below. Decode is super-linear (id = id*64n + i over a growing magnitude), so
    // a hostile GraphQL response with a huge shortcode could otherwise freeze the
    // page main thread for tens of seconds. 64 is generous.
    if (shortcode.length > 64) return '';
    try {
      let id = 0n;
      for (const ch of shortcode) {
        const i = IG_SC_ALPHABET.indexOf(ch);
        if (i < 0) return '';
        id = id * 64n + BigInt(i);
      }
      const ms = Number((id >> 23n) + 1314220021721n);
      if (!Number.isFinite(ms) || ms <= 0) return '';
      const d = new Date(ms);
      const y = d.getUTCFullYear();
      if (y < 2010 || y > 2100) return ''; // reject implausible decodes
      return d.toISOString();
    } catch {
      return '';
    }
  }

  // ── Instagram parser ──────────────────────────────────────────────────────

  function parseInstagramResponse(data) {
    const items = [];
    let hasNextPage = null;

    const restItems = data.items || data.feed_items;
    if (Array.isArray(restItems) && restItems.length > 0) {
      for (const rawItem of restItems) {
        // Saved / collection feeds wrap each post in { media: {...} }; older
        // feeds expose the media object directly.
        const item = rawItem.media || rawItem;
        const shortcode = item.code || item.shortcode;
        if (!shortcode) continue;
        const user = item.user || {};
        const igUser = user.username || '';
        const caption =
          typeof item.caption?.text === 'string' ? item.caption.text : item.caption || '';
        let mediaType = 'image';
        if (item.media_type === 2) mediaType = 'video';
        else if (item.media_type === 8) mediaType = 'carousel';
        const oneMedia = (m) => ({
          type: m.media_type === 2 ? 'video' : 'image',
          url: m.image_versions2?.candidates?.[0]?.url || '',
        });
        const media = (
          Array.isArray(item.carousel_media) && item.carousel_media.length > 0
            ? item.carousel_media.map(oneMedia)
            : [oneMedia(item)]
        ).filter((m) => m.url);
        const thumb =
          media[0]?.url ||
          item.image_versions2?.candidates?.[0]?.url ||
          item.carousel_media?.[0]?.image_versions2?.candidates?.[0]?.url ||
          '';
        items.push({
          id: String(item.id || item.pk || shortcode),
          platform: 'instagram',
          shortcode,
          postUrl: `https://www.instagram.com/p/${shortcode}/`,
          profileUrl: igUser ? `https://www.instagram.com/${igUser}/` : '',
          authorUsername: igUser,
          authorName: '',
          text: typeof caption === 'string' ? caption : '',
          thumbnailUrl: thumb,
          mediaType,
          media,
          timestamp:
            toIsoSeconds(item.taken_at ?? item.taken_at_timestamp) ||
            igTimestampFromShortcode(shortcode),
        });
      }
      if (typeof data.more_available === 'boolean') hasNextPage = data.more_available;
      return { items, hasNextPage };
    }

    // Total-node budget for the generic GraphQL walkers below: a hostile origin
    // could craft a wide-but-shallow blob to make every intercept an expensive
    // O(total-nodes) main-thread walk. Cap how many nodes we visit; the real IG
    // saved/feed responses fit comfortably under this.
    const MAX_WALK_NODES = 200000;
    let walkBudget = MAX_WALK_NODES;

    function findEdges(obj, acc = [], depth = 0) {
      if (!obj || typeof obj !== 'object' || depth > 50 || walkBudget <= 0) return acc;
      walkBudget--;
      if (Array.isArray(obj)) {
        obj.forEach((i) => findEdges(i, acc, depth + 1));
        return acc;
      }
      for (const key of Object.keys(obj)) {
        const val = obj[key];
        if (key === 'edges' && Array.isArray(val) && val.length > 0 && val[0]?.node?.shortcode)
          acc.push(val);
        else findEdges(val, acc, depth + 1);
      }
      return acc;
    }

    function findPageInfo(obj, depth = 0) {
      if (!obj || typeof obj !== 'object' || depth > 50 || walkBudget <= 0) return null;
      walkBudget--;
      if (Array.isArray(obj)) {
        for (const i of obj) {
          const r = findPageInfo(i, depth + 1);
          if (r !== null) return r;
        }
        return null;
      }
      // page_info usually sits next to edges, so prefer it directly to avoid a
      // full-tree fallback walk.
      if (obj.page_info && typeof obj.page_info.has_next_page === 'boolean')
        return obj.page_info.has_next_page;
      if ('has_next_page' in obj) return obj.has_next_page;
      for (const key of Object.keys(obj)) {
        const r = findPageInfo(obj[key], depth + 1);
        if (r !== null) return r;
      }
      return null;
    }

    for (const edges of findEdges(data)) {
      for (const edge of edges) {
        const node = edge?.node;
        if (!node?.shortcode) continue;
        const owner = node.owner || {};
        const igUser = owner.username || node.user?.username || '';
        const caption =
          node.edge_media_to_caption?.edges?.[0]?.node?.text || node.caption?.text || '';
        let mediaType = 'image';
        if (node.__typename === 'GraphVideo' || node.is_video) mediaType = 'video';
        else if (node.__typename === 'GraphSidecar' || node.edge_sidecar_to_children)
          mediaType = 'carousel';
        const childMedia = (c) => ({
          type: c.__typename === 'GraphVideo' || c.is_video ? 'video' : 'image',
          url: c.display_url || c.thumbnail_src || c.thumbnail_resources?.[0]?.src || '',
        });
        const children = node.edge_sidecar_to_children?.edges;
        const media = (
          Array.isArray(children) && children.length > 0
            ? children.map((e) => childMedia(e.node || {}))
            : [childMedia(node)]
        ).filter((m) => m.url);
        items.push({
          id: node.id || node.shortcode,
          platform: 'instagram',
          shortcode: node.shortcode,
          postUrl: `https://www.instagram.com/p/${node.shortcode}/`,
          profileUrl: igUser ? `https://www.instagram.com/${igUser}/` : '',
          authorUsername: igUser,
          authorName: '',
          text: caption,
          thumbnailUrl:
            media[0]?.url ||
            node.thumbnail_src ||
            node.display_url ||
            node.thumbnail_resources?.[0]?.src ||
            '',
          mediaType,
          media,
          timestamp:
            toIsoSeconds(node.taken_at_timestamp ?? node.taken_at) ||
            igTimestampFromShortcode(node.shortcode),
        });
      }
    }

    walkBudget = MAX_WALK_NODES; // fresh budget for the page_info walk
    const pi = findPageInfo(data);
    if (pi !== null) hasNextPage = pi;
    return { items, hasNextPage };
  }

  // ── Twitter parser ────────────────────────────────────────────────────────

  function parseTwitterResponse(data) {
    const items = [];
    let hasNextPage = null;
    const instructions =
      data?.data?.bookmark_timeline_v2?.timeline?.instructions ||
      data?.data?.bookmarks_timeline?.timeline?.instructions ||
      [];
    let tweetCount = 0;
    for (const instruction of instructions) {
      if (instruction.type !== 'TimelineAddEntries') continue;
      for (const entry of instruction.entries || []) {
        const content = entry.content || {};
        if (content.cursorType === 'Bottom' || (entry.entryId || '').startsWith('cursor-bottom')) {
          hasNextPage = !!content.value;
          continue;
        }
        const tweetResult = content.itemContent?.tweet_results?.result;
        if (!tweetResult) continue;
        const tweet = tweetResult.tweet || tweetResult;
        const legacy = tweet.legacy || {};
        const tweetId = tweet.rest_id || legacy.id_str || '';
        if (!tweetId) continue;
        const userResult = tweet.core?.user_results?.result || tweet.user_results?.result || {};
        // Twitter moved screen_name/name from user.legacy to a new user.core
        // object in 2024; read the new location first, fall back to legacy.
        const userLegacy = userResult.legacy || {};
        const userCore = userResult.core || {};
        const authorUsername = userCore.screen_name || userLegacy.screen_name || '';
        const mediaEntities = legacy.extended_entities?.media || legacy.entities?.media || [];
        let mediaType = 'text';
        if (mediaEntities.length > 0) {
          const t = mediaEntities[0].type;
          if (t === 'video' || t === 'animated_gif') mediaType = 'video';
          else if (mediaEntities.length > 1) mediaType = 'images';
          else mediaType = 'image';
        }
        const media = mediaEntities
          .map((m) => ({
            type: m.type === 'video' || m.type === 'animated_gif' ? 'video' : 'image',
            url: m.media_url_https || '',
          }))
          .filter((m) => m.url);
        items.push({
          id: tweetId,
          platform: 'twitter',
          shortcode: '',
          postUrl: `https://x.com/${authorUsername}/status/${tweetId}`,
          profileUrl: authorUsername ? `https://x.com/${authorUsername}` : '',
          authorUsername,
          authorName: userCore.name || userLegacy.name || '',
          text: legacy.full_text || legacy.text || '',
          thumbnailUrl: media[0]?.url || userLegacy.profile_image_url_https || '',
          mediaType,
          media,
          // Guarded conversion: a malformed created_at must yield '' (like the IG
          // and Pinterest paths) and not throw an Invalid Date RangeError, which
          // would abort parsing of the WHOLE timeline page and drop every tweet
          // already accumulated. Twitter's created_at is the same RFC-2822 format
          // toIsoHttpDate handles.
          timestamp: toIsoHttpDate(legacy.created_at),
        });
        tweetCount++;
      }
    }
    // NB: do NOT force hasNextPage=false merely because tweetCount===0. A
    // transient/intermediate response with no extractable tweets but a still
    // valid cursor-bottom must keep paging. End-of-timeline is decided
    // authoritatively by the cursor branch above (empty/absent value → false),
    // or by the host-side stall detector. Only collapse to false when there is
    // no cursor at all to keep going on.
    if (instructions.length > 0 && tweetCount === 0 && hasNextPage === null) hasNextPage = false;
    return { items, hasNextPage };
  }

  // ── Pinterest parser ──────────────────────────────────────────────────────
  //
  // Pinterest serves board/saved data via "resource RPC" calls
  // (GET /resource/<Name>Resource/get/). The response body is
  //   { resource_response: { data }, resource: { options: { bookmarks } } }
  // where `data` is the pin array directly (or `{ results: [...] }` on some
  // resources). Pagination is a string cursor in resource.options.bookmarks[0]
  // (also mirrored at resource_response.bookmark); an empty/absent cursor, the
  // literal '-end-', or a 'Y2JOb25lO…' base64 sentinel ("cbNone;") marks the end
  // of the feed — analogous to IG's more_available and TW's cursor-bottom.

  // Coerce Pinterest's RFC-2822 created_at ("Fri, 01 Aug 2025 19:57:38 +0000") —
  // the same human-readable format Twitter uses — into an ISO string.
  function toIsoHttpDate(s) {
    if (!s || typeof s !== 'string') return '';
    const t = Date.parse(s);
    if (!Number.isFinite(t)) return '';
    try {
      return new Date(t).toISOString();
    } catch {
      return '';
    }
  }

  // Highest-resolution image URL from a pin's `images` map (keyed by size).
  const PIN_IMG_SIZES = ['orig', '736x', '564x', '474x', '236x', '170x', '136x136'];
  function pinImageUrl(images) {
    if (!images || typeof images !== 'object') return '';
    for (const k of PIN_IMG_SIZES) if (images[k] && images[k].url) return images[k].url;
    for (const k of Object.keys(images)) if (images[k] && images[k].url) return images[k].url;
    return '';
  }

  // Best video URL from a pin's videos.video_list. Prefer a progressive MP4
  // (directly downloadable) over an HLS playlist; yt-dlp can still pull the pin
  // by its postUrl regardless — this is just the stored media url / cover.
  const PIN_VID_FORMATS = [
    'V_720P',
    'V_480P',
    'V_360P',
    'V_240P',
    'V_EXP7',
    'V_HLSV4',
    'V_HLSV3_WEB',
    'V_HLSV3_MOBILE',
  ];
  function pinVideoUrl(videos) {
    const list = videos && videos.video_list;
    if (!list || typeof list !== 'object') return '';
    for (const k of PIN_VID_FORMATS) if (list[k] && list[k].url) return list[k].url;
    for (const k of Object.keys(list)) if (list[k] && list[k].url) return list[k].url;
    return '';
  }

  // Map one raw pin object to the canonical item shape (same shape IG/TW emit).
  // Returns null for non-pin rows (board objects from BoardsResource, ad
  // separators, story carousels with no media) so a mixed feed yields no junk.
  function mapPin(pin) {
    if (!pin || typeof pin !== 'object' || !pin.id) return null;
    const isPinLike =
      pin.images || pin.videos || pin.carousel_data || pin.story_pin_data || pin.type === 'pin';
    if (!isPinLike) return null;

    const media = [];
    const pages =
      pin.story_pin_data && Array.isArray(pin.story_pin_data.pages)
        ? pin.story_pin_data.pages
        : null;
    if (pages && pages.length) {
      // Idea/story pin: one media entry per page (its first image or video block).
      for (const page of pages) {
        const blocks = Array.isArray(page.blocks) ? page.blocks : [];
        let added = false;
        for (const b of blocks) {
          if (b && b.video) {
            const u = pinVideoUrl(b.video);
            if (u) {
              media.push({ type: 'video', url: u });
              added = true;
              break;
            }
          }
          if (b && b.image && b.image.images) {
            const u = pinImageUrl(b.image.images);
            if (u) {
              media.push({ type: 'image', url: u });
              added = true;
              break;
            }
          }
        }
        if (!added && page.image && page.image.images) {
          const u = pinImageUrl(page.image.images);
          if (u) media.push({ type: 'image', url: u });
        }
      }
    } else if (pin.carousel_data && Array.isArray(pin.carousel_data.carousel_slots)) {
      for (const slot of pin.carousel_data.carousel_slots) {
        const u = pinImageUrl(slot.images);
        if (u) media.push({ type: 'image', url: u });
      }
    } else if (pin.videos && pin.videos.video_list) {
      const u = pinVideoUrl(pin.videos);
      if (u) media.push({ type: 'video', url: u });
    }
    const cover = pinImageUrl(pin.images);
    if (media.length === 0 && cover) media.push({ type: 'image', url: cover });
    const cleanMedia = media.filter((m) => m.url);

    let mediaType = 'image';
    if (
      (pages && pages.length) ||
      (pin.carousel_data &&
        Array.isArray(pin.carousel_data.carousel_slots) &&
        pin.carousel_data.carousel_slots.length > 1) ||
      cleanMedia.length > 1
    )
      mediaType = 'carousel';
    else if (pin.videos && pin.videos.video_list) mediaType = 'video';

    const pinner = pin.pinner || pin.native_creator || {};
    const title = pin.title || pin.grid_title || '';
    const description =
      pin.description || pin.closeup_unified_description || pin.closeup_description || '';
    let text = [title, description].filter(Boolean).join(' — ');
    // Pins often link to an external page (recipe, article, product). There's no
    // dedicated column, so keep the link inline in the text rather than lose it.
    const link = pin.link || pin.tracked_link || '';
    if (link) text = text ? `${text}\n${link}` : link;

    return {
      id: String(pin.id),
      platform: 'pinterest',
      shortcode: '',
      postUrl: `https://www.pinterest.com/pin/${pin.id}/`,
      profileUrl: pinner.username ? `https://www.pinterest.com/${pinner.username}/` : '',
      authorUsername: pinner.username || '',
      authorName: pinner.full_name || '',
      text,
      thumbnailUrl: cover || cleanMedia[0]?.url || '',
      mediaType,
      media: cleanMedia,
      timestamp: toIsoHttpDate(pin.created_at),
    };
  }

  function parsePinterestResponse(data) {
    const items = [];
    let hasNextPage = null;
    const rr = data && data.resource_response;
    let rows = rr && rr.data;
    if (rows && !Array.isArray(rows) && Array.isArray(rows.results)) rows = rows.results;
    if (Array.isArray(rows)) {
      for (const pin of rows) {
        const it = mapPin(pin);
        if (it) items.push(it);
      }
    }
    // Pagination cursor: resource.options.bookmarks[0], else resource_response.bookmark.
    let cursor = null;
    const opts = data && data.resource && data.resource.options;
    if (opts && Array.isArray(opts.bookmarks)) cursor = opts.bookmarks[0];
    else if (rr && typeof rr.bookmark === 'string') cursor = rr.bookmark;
    if (cursor != null) {
      const c = String(cursor);
      hasNextPage = !(c === '' || c === '-end-' || c.indexOf('Y2JOb25lO') === 0);
      // Tolerant end detection: Pinterest could introduce a new end sentinel we
      // don't recognise. If the cursor stops advancing (same value as last time),
      // treat it as end-of-feed so the sync can't loop forever on a stuck cursor.
      if (hasNextPage) {
        if (c === window.__ssPinLastCursor) hasNextPage = false;
        else window.__ssPinLastCursor = c;
      }
    }
    return { items, hasNextPage };
  }

  // ── Shared response handler ─────────────────────────────────────────────────

  function matchPlatform(url) {
    const isIG = ['/graphql/query', '/api/v1/feed/saved/', '/api/v1/feed/collection/'].some((p) =>
      url.includes(p),
    );
    const isTW = url.includes('/i/api/graphql/') && url.toLowerCase().includes('bookmark');
    // Pinterest "resource RPC" endpoints that carry PINS (board feeds, section
    // feeds, a user's pins). The board/section LIST resources are intentionally
    // excluded: their responses carry no pins, and a stray hasNextPage from one
    // could prematurely end a board sync.
    const isPIN =
      /\/resource\/(?:BoardFeed|BoardSectionPins|UserPins|UserActivityPins|UserActivityFeed)Resource\/get\//.test(
        url,
      );
    return isIG ? 'instagram' : isTW ? 'twitter' : isPIN ? 'pinterest' : null;
  }

  // Stash a parsed batch in the MAIN world and relay it to the host. Shared by
  // the live fetch/XHR path and the Pinterest SSR replay (which has no
  // originating URL). No-op unless there are items or an authoritative
  // end-of-feed (hasNextPage === false).
  function emit(items, hasNextPage, platform) {
    if (!(items.length > 0 || hasNextPage === false)) return;
    window.__lastInterceptAt = Date.now();
    // Stash full parsed items keyed by both shortcode and id so the selection
    // overlay (webview-select.js) can resolve a DOM post tile back to its
    // complete record (media URLs etc.) for download/import.
    if (items.length > 0) {
      const store = (window.__ssCapturedItems = window.__ssCapturedItems || {});
      // FIFO eviction so the store can't grow unbounded over the page's lifetime
      // during large feed syncs (it only needs to resolve currently visible/
      // selected tiles, so dropping the oldest keys is safe). Track insertion
      // order in a PRIVATE eviction queue and cap total keys (both shortcode + id).
      const evictQueue = (window.__ssEvictQueue = window.__ssEvictQueue || []);
      // __ssCapturedOrder is the MONOTONIC growth signal the renderer scroll loops
      // poll (src/lib/browserScripts.js): they break after NO_GROWTH_LIMIT iters
      // with no new keys. It must only ever grow — it is NOT the eviction queue, so
      // capping the store below can't pin its length and prematurely end the sync.
      const order = (window.__ssCapturedOrder = window.__ssCapturedOrder || []);
      const MAX_KEYS = 5000;
      const put = (key, val) => {
        if (!(key in store)) {
          evictQueue.push(key);
          order.push(key);
        }
        store[key] = val;
      };
      for (const it of items) {
        if (it.shortcode) put(it.shortcode, it);
        if (it.id) put(String(it.id), it);
      }
      while (evictQueue.length > MAX_KEYS) {
        const oldest = evictQueue.shift();
        delete store[oldest];
      }
    }
    _relay(items, hasNextPage, platform);
  }

  function handleResponse(url, rawText) {
    const platform = matchPlatform(url);
    if (!platform) return;
    let data;
    try {
      data = JSON.parse(rawText);
    } catch {
      return;
    }
    const { items, hasNextPage } =
      platform === 'instagram'
        ? parseInstagramResponse(data)
        : platform === 'pinterest'
          ? parsePinterestResponse(data)
          : parseTwitterResponse(data);
    emit(items, hasNextPage, platform);
  }

  // Pinterest server-renders the FIRST page of a board's pins inline in the page
  // HTML (in a JSON <script>), so the passive fetch/XHR hook never sees it — only
  // scroll-triggered pages 2..N. At sync start the host calls this to scan those
  // inline blobs for the embedded BoardFeedResource response and relay its pins
  // through the same pipeline. No network call is made (Pinterest's resource RPC
  // is bot-guarded and would 403 on replay), so this is safe and best-effort —
  // the Pinterest analogue of IG_FEED_REPLAY.
  function replayPinterestSSR() {
    try {
      const pins = [];
      const pushFrom = (resp) => {
        let d = resp && resp.data;
        if (d && !Array.isArray(d) && Array.isArray(d.results)) d = d.results;
        if (Array.isArray(d)) for (const p of d) pins.push(p);
      };
      // Total-node budget so a large inline redux/PWS blob can't stall the page
      // main thread at sync start (Pinterest's blobs are big).
      let budget = 200000;
      const walk = (obj, depth) => {
        if (!obj || typeof obj !== 'object' || depth > 60 || budget <= 0) return;
        budget--;
        if (Array.isArray(obj)) {
          for (const x of obj) walk(x, depth + 1);
          return;
        }
        // Shape A: resourceResponses: [{ name:'BoardFeedResource', response:{data} }].
        if (obj.name === 'BoardFeedResource' && obj.response) {
          pushFrom(obj.response);
          return; // consumed — don't re-walk this node's children
        }
        // Shape B: redux cache resources: { BoardFeedResource: { '<args>': { data } } }.
        const bf = obj.BoardFeedResource;
        if (bf && typeof bf === 'object') {
          for (const k of Object.keys(bf)) if (bf[k] && bf[k].data) pushFrom(bf[k]);
          return; // consumed — don't re-walk this node's children
        }
        for (const k of Object.keys(obj)) walk(obj[k], depth + 1);
      };
      const scripts = document.querySelectorAll(
        'script[type="application/json"], script[id^="__PWS"]',
      );
      for (const s of scripts) {
        const txt = s.textContent || '';
        if (txt.indexOf('BoardFeedResource') === -1) continue;
        let json;
        try {
          json = JSON.parse(txt);
        } catch {
          continue;
        }
        walk(json, 0);
        // First script that yields pins is the SSR first page — stop scanning the
        // rest of the (potentially many, large) inline blobs.
        if (pins.length) break;
      }
      if (pins.length) {
        const { items } = parsePinterestResponse({ resource_response: { data: pins } });
        // hasNextPage=null on purpose: this is only page 1, so it must NOT signal
        // end-of-feed. The scroll loop + later BoardFeedResource responses decide
        // the end via their cursor.
        if (items.length) emit(items, null, 'pinterest');
      }
    } catch (_) {}
  }
  window.__ssReplayPinterest = replayPinterestSSR;

  // ── Fetch hook ────────────────────────────────────────────────────────────

  const originalFetch = window.fetch;
  window.fetch = async function (...args) {
    const url = typeof args[0] === 'string' ? args[0] : args[0]?.url || '';
    const response = await originalFetch.apply(this, args);
    try {
      if (!matchPlatform(url)) return response;
      const cloned = response.clone();
      const text = await cloned.text();
      handleResponse(url, text);
    } catch (_) {}
    return response;
  };

  const XHR = window.XMLHttpRequest;
  if (XHR && XHR.prototype) {
    const origOpen = XHR.prototype.open;
    const origSend = XHR.prototype.send;

    XHR.prototype.open = function (method, url, ...rest) {
      this.__swUrl = typeof url === 'string' ? url : url?.toString?.() || '';
      // Reset the per-request capture guard so a reused XHR instance (some SPA
      // frameworks open()+send() the same object repeatedly) gets exactly one
      // 'load' handler per request lifecycle instead of stacking one per send().
      this.__swHooked = false;
      return origOpen.call(this, method, url, ...rest);
    };

    XHR.prototype.send = function (...args) {
      const url = this.__swUrl || '';
      if (matchPlatform(url) && !this.__swHooked) {
        this.__swHooked = true;
        // { once: true } removes the listener after it fires so handleResponse
        // can't run more than once for the same final response.
        this.addEventListener(
          'load',
          () => {
            try {
              const text =
                this.responseType === '' || this.responseType === 'text'
                  ? this.responseText
                  : this.responseType === 'json'
                    ? JSON.stringify(this.response)
                    : '';
              if (text) handleResponse(url, text);
            } catch (_) {}
          },
          { once: true },
        );
      }
      return origSend.apply(this, args);
    };
  }
})();
