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

// File-internal types ───────────────────────────────────────────────────────

// A normalized media slide (matches the { type, url } shape IG/TW/Pinterest
// emit here). 'image' | 'video' is the per-slide type the host persists.
interface InterceptMedia {
  type: 'image' | 'video';
  url: string;
}

// The normalized, post-like record produced by the parsers below and relayed to
// the host. A loose intermediate shape (a subset of Shelfy.Post fields) — id may
// be a string (IG/TW/Pinterest stringify it) or, on the lightweight IG GraphQL
// path, whatever `node.id` is; keep it broad and let the host normalize.
interface InterceptItem {
  id: string;
  platform: 'instagram' | 'twitter' | 'pinterest';
  shortcode: string;
  postUrl: string;
  profileUrl: string;
  authorUsername: string;
  authorName: string;
  text: string;
  thumbnailUrl: string;
  mediaType: string;
  media: InterceptMedia[];
  timestamp: string;
}

// Result of every per-platform parser: the captured items plus a tri-state
// pagination signal (true = more, false = authoritative end, null = unknown).
interface ParseResult {
  items: InterceptItem[];
  hasNextPage: boolean | null;
}

// Loose, structurally-typed views of the open-ended remote payloads. The walk
// helpers and parsers treat every nested object as an index map of unknown.
type JsonObject = { [key: string]: unknown };

// The contextBridge surface exposed by webview-preload.js into the MAIN world.
interface SocialSavedBridge {
  send: (items: InterceptItem[], hasNextPage: boolean | null, platform: string) => void;
  sendSelect?: (payload: unknown) => void;
}

// Custom MAIN-world properties this script reads/writes on `window` and on each
// XMLHttpRequest instance. Declared here (not in a shared .d.ts) because they
// are private to this injected capture pipeline.
declare global {
  interface Window {
    __socialSavedInjected?: boolean;
    __socialSavedBridge?: SocialSavedBridge;
    __lastInterceptAt?: number;
    __ssCapturedItems?: { [key: string]: InterceptItem };
    __ssEvictQueue?: string[];
    __ssCapturedOrder?: string[];
    __ssPinLastCursor?: string;
    __ssReplayPinterest?: () => void;
  }
  interface XMLHttpRequest {
    __swUrl?: string;
    __swHooked?: boolean;
  }
}

(function () {
  if (window.__socialSavedInjected) return;
  window.__socialSavedInjected = true;

  const MSG_TYPE = 'SOCIAL_SAVED_INTERCEPT';

  // Relay via the contextBridge exposed by the isolated preload. As a defensive
  // fallback (e.g. if the bridge is not yet present), use a same-origin
  // postMessage that webview-preload.js also listens for.
  let _relay: (items: InterceptItem[], hasNextPage: boolean | null, platform: string) => void;
  if (window.__socialSavedBridge) {
    _relay = (items, hasNextPage, platform) =>
      window.__socialSavedBridge!.send(items, hasNextPage, platform);
  } else {
    _relay = (items, hasNextPage, platform) =>
      window.postMessage({ type: MSG_TYPE, items, hasNextPage, platform }, window.location.origin);
  }

  // Coerce a unix-seconds timestamp (the field IG/Twitter expose under various
  // names) into an ISO string, tolerating missing/garbage values. This is the
  // post date persisted to the DB and used for date display + date sorting.
  function toIsoSeconds(sec: unknown): string {
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
  function igTimestampFromShortcode(shortcode: unknown): string {
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

  function parseInstagramResponse(data: JsonObject): ParseResult {
    const items: InterceptItem[] = [];
    let hasNextPage: boolean | null = null;

    const restItems = (data.items || data.feed_items) as JsonObject[] | undefined;
    if (Array.isArray(restItems) && restItems.length > 0) {
      for (const rawItem of restItems) {
        // Saved / collection feeds wrap each post in { media: {...} }; older
        // feeds expose the media object directly.
        const item = (rawItem.media || rawItem) as JsonObject;
        const shortcode = (item.code || item.shortcode) as string | undefined;
        if (!shortcode) continue;
        const user = (item.user || {}) as JsonObject;
        const igUser = (user.username as string | undefined) || '';
        const itemCaption = item.caption as JsonObject | string | undefined;
        const caption =
          typeof (itemCaption as JsonObject)?.text === 'string'
            ? ((itemCaption as JsonObject).text as string)
            : (itemCaption as string | undefined) || '';
        let mediaType = 'image';
        if (item.media_type === 2) mediaType = 'video';
        else if (item.media_type === 8) mediaType = 'carousel';
        const oneMedia = (m: JsonObject): InterceptMedia => ({
          type: m.media_type === 2 ? 'video' : 'image',
          url:
            (((m.image_versions2 as JsonObject)?.candidates as JsonObject[])?.[0]?.url as
              | string
              | undefined) || '',
        });
        const carouselMedia = item.carousel_media as JsonObject[] | undefined;
        const media = (
          Array.isArray(carouselMedia) && carouselMedia.length > 0
            ? carouselMedia.map(oneMedia)
            : [oneMedia(item)]
        ).filter((m) => m.url);
        const thumb =
          media[0]?.url ||
          (((item.image_versions2 as JsonObject)?.candidates as JsonObject[])?.[0]?.url as
            | string
            | undefined) ||
          (((carouselMedia?.[0]?.image_versions2 as JsonObject)?.candidates as JsonObject[])?.[0]
            ?.url as string | undefined) ||
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

    function findEdges(obj: unknown, acc: JsonObject[][] = [], depth = 0): JsonObject[][] {
      if (!obj || typeof obj !== 'object' || depth > 50 || walkBudget <= 0) return acc;
      walkBudget--;
      if (Array.isArray(obj)) {
        obj.forEach((i) => findEdges(i, acc, depth + 1));
        return acc;
      }
      const rec = obj as JsonObject;
      for (const key of Object.keys(rec)) {
        const val = rec[key];
        if (
          key === 'edges' &&
          Array.isArray(val) &&
          val.length > 0 &&
          (val[0] as JsonObject)?.node &&
          ((val[0] as JsonObject).node as JsonObject)?.shortcode
        )
          acc.push(val as JsonObject[]);
        else findEdges(val, acc, depth + 1);
      }
      return acc;
    }

    function findPageInfo(obj: unknown, depth = 0): boolean | null {
      if (!obj || typeof obj !== 'object' || depth > 50 || walkBudget <= 0) return null;
      walkBudget--;
      if (Array.isArray(obj)) {
        for (const i of obj) {
          const r = findPageInfo(i, depth + 1);
          if (r !== null) return r;
        }
        return null;
      }
      const rec = obj as JsonObject;
      // page_info usually sits next to edges, so prefer it directly to avoid a
      // full-tree fallback walk.
      if (rec.page_info && typeof (rec.page_info as JsonObject).has_next_page === 'boolean')
        return (rec.page_info as JsonObject).has_next_page as boolean;
      if ('has_next_page' in rec) return rec.has_next_page as boolean;
      for (const key of Object.keys(rec)) {
        const r = findPageInfo(rec[key], depth + 1);
        if (r !== null) return r;
      }
      return null;
    }

    for (const edges of findEdges(data)) {
      for (const edge of edges) {
        const node = edge?.node as JsonObject | undefined;
        if (!node?.shortcode) continue;
        const owner = (node.owner || {}) as JsonObject;
        const igUser =
          (owner.username as string | undefined) ||
          ((node.user as JsonObject)?.username as string | undefined) ||
          '';
        const caption =
          (
            ((node.edge_media_to_caption as JsonObject)?.edges as JsonObject[])?.[0]?.node as
              | JsonObject
              | undefined
          )?.text ||
          (node.caption as JsonObject)?.text ||
          '';
        let mediaType = 'image';
        if (node.__typename === 'GraphVideo' || node.is_video) mediaType = 'video';
        else if (node.__typename === 'GraphSidecar' || node.edge_sidecar_to_children)
          mediaType = 'carousel';
        const childMedia = (c: JsonObject): InterceptMedia => ({
          type: c.__typename === 'GraphVideo' || c.is_video ? 'video' : 'image',
          url:
            (c.display_url as string | undefined) ||
            (c.thumbnail_src as string | undefined) ||
            ((c.thumbnail_resources as JsonObject[])?.[0]?.src as string | undefined) ||
            '',
        });
        const children = (node.edge_sidecar_to_children as JsonObject)?.edges as
          | JsonObject[]
          | undefined;
        const media = (
          Array.isArray(children) && children.length > 0
            ? children.map((e) => childMedia((e.node || {}) as JsonObject))
            : [childMedia(node)]
        ).filter((m) => m.url);
        items.push({
          id: (node.id as string | undefined) || (node.shortcode as string),
          platform: 'instagram',
          shortcode: node.shortcode as string,
          postUrl: `https://www.instagram.com/p/${node.shortcode}/`,
          profileUrl: igUser ? `https://www.instagram.com/${igUser}/` : '',
          authorUsername: igUser,
          authorName: '',
          text: caption as string,
          thumbnailUrl:
            media[0]?.url ||
            (node.thumbnail_src as string | undefined) ||
            (node.display_url as string | undefined) ||
            ((node.thumbnail_resources as JsonObject[])?.[0]?.src as string | undefined) ||
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

  function parseTwitterResponse(data: JsonObject): ParseResult {
    const items: InterceptItem[] = [];
    let hasNextPage: boolean | null = null;
    const dataData = data?.data as JsonObject | undefined;
    const instructions =
      (((dataData?.bookmark_timeline_v2 as JsonObject)?.timeline as JsonObject)?.instructions as
        | JsonObject[]
        | undefined) ||
      (((dataData?.bookmarks_timeline as JsonObject)?.timeline as JsonObject)?.instructions as
        | JsonObject[]
        | undefined) ||
      [];
    let tweetCount = 0;
    for (const instruction of instructions) {
      if (instruction.type !== 'TimelineAddEntries') continue;
      for (const entry of (instruction.entries as JsonObject[] | undefined) || []) {
        const content = (entry.content || {}) as JsonObject;
        if (
          content.cursorType === 'Bottom' ||
          ((entry.entryId as string | undefined) || '').startsWith('cursor-bottom')
        ) {
          hasNextPage = !!content.value;
          continue;
        }
        const tweetResult = ((content.itemContent as JsonObject)?.tweet_results as JsonObject)
          ?.result as JsonObject | undefined;
        if (!tweetResult) continue;
        const tweet = (tweetResult.tweet || tweetResult) as JsonObject;
        const legacy = (tweet.legacy || {}) as JsonObject;
        const tweetId =
          (tweet.rest_id as string | undefined) || (legacy.id_str as string | undefined) || '';
        if (!tweetId) continue;
        const userResult =
          (((tweet.core as JsonObject)?.user_results as JsonObject)?.result as
            | JsonObject
            | undefined) ||
          ((tweet.user_results as JsonObject)?.result as JsonObject | undefined) ||
          {};
        // Twitter moved screen_name/name from user.legacy to a new user.core
        // object in 2024; read the new location first, fall back to legacy.
        const userLegacy = (userResult.legacy || {}) as JsonObject;
        const userCore = (userResult.core || {}) as JsonObject;
        const authorUsername =
          (userCore.screen_name as string | undefined) ||
          (userLegacy.screen_name as string | undefined) ||
          '';
        const mediaEntities = (((legacy.extended_entities as JsonObject)?.media as
          | JsonObject[]
          | undefined) ||
          ((legacy.entities as JsonObject)?.media as JsonObject[] | undefined) ||
          []) as JsonObject[];
        let mediaType = 'text';
        if (mediaEntities.length > 0) {
          const t = mediaEntities[0].type;
          if (t === 'video' || t === 'animated_gif') mediaType = 'video';
          else if (mediaEntities.length > 1) mediaType = 'images';
          else mediaType = 'image';
        }
        const media = mediaEntities
          .map(
            (m): InterceptMedia => ({
              type: m.type === 'video' || m.type === 'animated_gif' ? 'video' : 'image',
              url: (m.media_url_https as string | undefined) || '',
            }),
          )
          .filter((m) => m.url);
        items.push({
          id: tweetId,
          platform: 'twitter',
          shortcode: '',
          postUrl: `https://x.com/${authorUsername}/status/${tweetId}`,
          profileUrl: authorUsername ? `https://x.com/${authorUsername}` : '',
          authorUsername,
          authorName:
            (userCore.name as string | undefined) || (userLegacy.name as string | undefined) || '',
          text:
            (legacy.full_text as string | undefined) || (legacy.text as string | undefined) || '',
          thumbnailUrl:
            media[0]?.url || (userLegacy.profile_image_url_https as string | undefined) || '',
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
  function toIsoHttpDate(s: unknown): string {
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
  function pinImageUrl(images: unknown): string {
    if (!images || typeof images !== 'object') return '';
    const map = images as JsonObject;
    for (const k of PIN_IMG_SIZES)
      if (map[k] && (map[k] as JsonObject).url) return (map[k] as JsonObject).url as string;
    for (const k of Object.keys(map))
      if (map[k] && (map[k] as JsonObject).url) return (map[k] as JsonObject).url as string;
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
  function pinVideoUrl(videos: unknown): string {
    const list = videos && (videos as JsonObject).video_list;
    if (!list || typeof list !== 'object') return '';
    const map = list as JsonObject;
    for (const k of PIN_VID_FORMATS)
      if (map[k] && (map[k] as JsonObject).url) return (map[k] as JsonObject).url as string;
    for (const k of Object.keys(map))
      if (map[k] && (map[k] as JsonObject).url) return (map[k] as JsonObject).url as string;
    return '';
  }

  // Map one raw pin object to the canonical item shape (same shape IG/TW emit).
  // Returns null for non-pin rows (board objects from BoardsResource, ad
  // separators, story carousels with no media) so a mixed feed yields no junk.
  function mapPin(pin: unknown): InterceptItem | null {
    if (!pin || typeof pin !== 'object' || !(pin as JsonObject).id) return null;
    const p = pin as JsonObject;
    const isPinLike =
      p.images || p.videos || p.carousel_data || p.story_pin_data || p.type === 'pin';
    if (!isPinLike) return null;

    const media: InterceptMedia[] = [];
    const pages =
      p.story_pin_data && Array.isArray((p.story_pin_data as JsonObject).pages)
        ? ((p.story_pin_data as JsonObject).pages as JsonObject[])
        : null;
    if (pages && pages.length) {
      // Idea/story pin: one media entry per page (its first image or video block).
      for (const page of pages) {
        const blocks = Array.isArray(page.blocks) ? (page.blocks as JsonObject[]) : [];
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
          if (b && b.image && (b.image as JsonObject).images) {
            const u = pinImageUrl((b.image as JsonObject).images);
            if (u) {
              media.push({ type: 'image', url: u });
              added = true;
              break;
            }
          }
        }
        if (!added && page.image && (page.image as JsonObject).images) {
          const u = pinImageUrl((page.image as JsonObject).images);
          if (u) media.push({ type: 'image', url: u });
        }
      }
    } else if (p.carousel_data && Array.isArray((p.carousel_data as JsonObject).carousel_slots)) {
      for (const slot of (p.carousel_data as JsonObject).carousel_slots as JsonObject[]) {
        const u = pinImageUrl(slot.images);
        if (u) media.push({ type: 'image', url: u });
      }
    } else if (p.videos && (p.videos as JsonObject).video_list) {
      const u = pinVideoUrl(p.videos);
      if (u) media.push({ type: 'video', url: u });
    }
    const cover = pinImageUrl(p.images);
    if (media.length === 0 && cover) media.push({ type: 'image', url: cover });
    const cleanMedia = media.filter((m) => m.url);

    let mediaType = 'image';
    if (
      (pages && pages.length) ||
      (p.carousel_data &&
        Array.isArray((p.carousel_data as JsonObject).carousel_slots) &&
        ((p.carousel_data as JsonObject).carousel_slots as JsonObject[]).length > 1) ||
      cleanMedia.length > 1
    )
      mediaType = 'carousel';
    else if (p.videos && (p.videos as JsonObject).video_list) mediaType = 'video';

    const pinner = (p.pinner || p.native_creator || {}) as JsonObject;
    const title = (p.title as string | undefined) || (p.grid_title as string | undefined) || '';
    const description =
      (p.description as string | undefined) ||
      (p.closeup_unified_description as string | undefined) ||
      (p.closeup_description as string | undefined) ||
      '';
    let text = [title, description].filter(Boolean).join(' — ');
    // Pins often link to an external page (recipe, article, product). There's no
    // dedicated column, so keep the link inline in the text rather than lose it.
    const link = (p.link as string | undefined) || (p.tracked_link as string | undefined) || '';
    if (link) text = text ? `${text}\n${link}` : link;

    return {
      id: String(p.id),
      platform: 'pinterest',
      shortcode: '',
      postUrl: `https://www.pinterest.com/pin/${p.id}/`,
      profileUrl: pinner.username ? `https://www.pinterest.com/${pinner.username}/` : '',
      authorUsername: (pinner.username as string | undefined) || '',
      authorName: (pinner.full_name as string | undefined) || '',
      text,
      thumbnailUrl: cover || cleanMedia[0]?.url || '',
      mediaType,
      media: cleanMedia,
      timestamp: toIsoHttpDate(p.created_at),
    };
  }

  function parsePinterestResponse(data: JsonObject): ParseResult {
    const items: InterceptItem[] = [];
    let hasNextPage: boolean | null = null;
    const rr = data && (data.resource_response as JsonObject | undefined);
    let rows = rr && (rr.data as unknown);
    if (rows && !Array.isArray(rows) && Array.isArray((rows as JsonObject).results))
      rows = (rows as JsonObject).results;
    if (Array.isArray(rows)) {
      for (const pin of rows) {
        const it = mapPin(pin);
        if (it) items.push(it);
      }
    }
    // Pagination cursor: resource.options.bookmarks[0], else resource_response.bookmark.
    let cursor: unknown = null;
    const opts = data && (data.resource as JsonObject)?.options;
    if (opts && Array.isArray((opts as JsonObject).bookmarks))
      cursor = ((opts as JsonObject).bookmarks as unknown[])[0];
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

  function matchPlatform(url: string): 'instagram' | 'twitter' | 'pinterest' | null {
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
  function emit(items: InterceptItem[], hasNextPage: boolean | null, platform: string): void {
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
      const put = (key: string, val: InterceptItem): void => {
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
        const oldest = evictQueue.shift()!;
        delete store[oldest];
      }
    }
    _relay(items, hasNextPage, platform);
  }

  function handleResponse(url: string, rawText: string): void {
    const platform = matchPlatform(url);
    if (!platform) return;
    let data: JsonObject;
    try {
      data = JSON.parse(rawText) as JsonObject;
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
  function replayPinterestSSR(): void {
    try {
      const pins: JsonObject[] = [];
      const pushFrom = (resp: unknown): void => {
        let d = resp && (resp as JsonObject).data;
        if (d && !Array.isArray(d) && Array.isArray((d as JsonObject).results))
          d = (d as JsonObject).results;
        if (Array.isArray(d)) for (const p of d) pins.push(p as JsonObject);
      };
      // Total-node budget so a large inline redux/PWS blob can't stall the page
      // main thread at sync start (Pinterest's blobs are big).
      let budget = 200000;
      const walk = (obj: unknown, depth: number): void => {
        if (!obj || typeof obj !== 'object' || depth > 60 || budget <= 0) return;
        budget--;
        if (Array.isArray(obj)) {
          for (const x of obj) walk(x, depth + 1);
          return;
        }
        const rec = obj as JsonObject;
        // Shape A: resourceResponses: [{ name:'BoardFeedResource', response:{data} }].
        if (rec.name === 'BoardFeedResource' && rec.response) {
          pushFrom(rec.response);
          return; // consumed — don't re-walk this node's children
        }
        // Shape B: redux cache resources: { BoardFeedResource: { '<args>': { data } } }.
        const bf = rec.BoardFeedResource;
        if (bf && typeof bf === 'object') {
          const bfMap = bf as JsonObject;
          for (const k of Object.keys(bfMap))
            if (bfMap[k] && (bfMap[k] as JsonObject).data) pushFrom(bfMap[k]);
          return; // consumed — don't re-walk this node's children
        }
        for (const k of Object.keys(rec)) walk(rec[k], depth + 1);
      };
      const scripts = document.querySelectorAll(
        'script[type="application/json"], script[id^="__PWS"]',
      );
      for (const s of scripts) {
        const txt = s.textContent || '';
        if (txt.indexOf('BoardFeedResource') === -1) continue;
        let json: unknown;
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
  window.fetch = async function (...args: Parameters<typeof window.fetch>): Promise<Response> {
    const first = args[0];
    const url = typeof first === 'string' ? first : (first as Request)?.url || '';
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

    XHR.prototype.open = function (
      this: XMLHttpRequest,
      method: string,
      url: string | URL,
      ...rest: unknown[]
    ): void {
      this.__swUrl = typeof url === 'string' ? url : url?.toString?.() || '';
      // Reset the per-request capture guard so a reused XHR instance (some SPA
      // frameworks open()+send() the same object repeatedly) gets exactly one
      // 'load' handler per request lifecycle instead of stacking one per send().
      this.__swHooked = false;
      return (origOpen as (...a: unknown[]) => void).call(this, method, url, ...rest);
    };

    XHR.prototype.send = function (
      this: XMLHttpRequest,
      ...args: Parameters<XMLHttpRequest['send']>
    ): void {
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
