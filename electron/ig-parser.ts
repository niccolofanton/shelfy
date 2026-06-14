/**
 * ig-parser.ts
 * Parse Instagram API response data into normalized Post objects.
 * Used for batch import and data normalization (e.g. importing Chrome extension JSON exports).
 *
 * NOTE: Real-time interception during browsing is handled by webview-preload.js.
 * This module is for batch/offline use only (main process, no window/DOM available).
 */

// ---------------------------------------------------------------------------
// File-internal types
// ---------------------------------------------------------------------------

// A normalized media slide, the input shape db.deriveMedia consumes
// (Shelfy.PostMediaInput, minus localPath which the parsers never set here).
interface ParsedMedia {
  type: 'image' | 'video';
  url: string;
}

// The normalized, post-like record produced by the parsers/normalizers below.
// It is a loose intermediate shape (a subset of Shelfy.Post fields) consumed by
// db.postToRow / db.deriveMedia via `??` chains, not a fully-formed Shelfy.Post.
interface ParsedPost {
  id: string;
  platform: 'instagram';
  shortcode: string;
  postUrl: string;
  profileUrl: string;
  authorUsername: string;
  authorName: string;
  text: string;
  thumbnailUrl: string;
  mediaType: string;
  media: ParsedMedia[];
  timestamp: string;
}

// The AI-analysis fields carried over from an exported record. Each field is
// either the exported value or undefined (never coerced to ''/[]).
interface ParsedAiFields {
  aiDescription: string | undefined;
  aiTags: string[] | undefined;
  aiGeneralTags: string[] | undefined;
  aiSpecificTags: string[] | undefined;
  aiCategory: string | undefined;
  aiContentType: string | undefined;
  aiEntities: string[] | undefined;
  aiKeywords: string[] | undefined;
  aiLanguage: string | undefined;
  aiSaveReason: string | undefined;
  aiStatus: string | undefined;
  aiModel: string | undefined;
  aiAnalyzedAt: number | undefined;
}

// A normalized exported post additionally carries the optional AI fields.
type NormalizedExportedPost = ParsedPost & ParsedAiFields;

// Loose, structurally-typed views of the open-ended IG payload shapes. The
// upstream JSON is untrusted/dynamic, so every field is optional and indexing
// stays type-safe; unknown sub-trees are narrowed where walked.
interface GraphMediaSource {
  __typename?: string;
  is_video?: boolean;
  display_url?: string;
  thumbnail_src?: string;
  thumbnail_resources?: Array<{ src?: string }>;
}

interface GraphNode extends GraphMediaSource {
  id?: string;
  shortcode?: string;
  owner?: { username?: string };
  user?: { username?: string };
  edge_media_to_caption?: { edges?: Array<{ node?: { text?: string } }> };
  caption?: { text?: string };
  edge_sidecar_to_children?: { edges?: Array<{ node?: GraphMediaSource }> };
  taken_at_timestamp?: number;
  taken_at?: number;
}

interface GraphEdge {
  node?: GraphNode;
}

interface RestMediaCandidate {
  url?: string;
}

interface RestItem {
  code?: string;
  shortcode?: string;
  id?: string | number;
  pk?: string | number;
  user?: { username?: string };
  caption?: { text?: string } | string;
  media_type?: number;
  image_versions2?: { candidates?: RestMediaCandidate[] };
  carousel_media?: Array<{ image_versions2?: { candidates?: RestMediaCandidate[] } } & RestItem>;
  taken_at?: number;
}

// A raw API response body (already JSON-decoded). Both supported formats plus
// arbitrary nested GraphQL trees, so it's an open record.
interface ResponseBody {
  items?: unknown;
  feed_items?: unknown;
  more_available?: boolean;
  [key: string]: unknown;
}

// The return shape of parseResponseBody.
interface ParseResult {
  items: ParsedPost[];
  hasNextPage: boolean | null;
}

// A post record exported from the old Chrome extension JSON format. Stores the
// caption under `caption`; loose since it's an untrusted on-disk record.
interface ExportedPost {
  id?: string;
  shortcode?: string;
  postUrl?: string;
  profileUrl?: string;
  authorUsername?: string;
  authorName?: string;
  caption?: string;
  text?: string;
  thumbnailUrl?: string;
  mediaType?: string;
  media?: Array<{ type?: string; url?: string; thumbnailUrl?: string }>;
  timestamp?: string;
  aiDescription?: unknown;
  aiTags?: unknown;
  aiGeneralTags?: unknown;
  aiSpecificTags?: unknown;
  aiCategory?: unknown;
  aiContentType?: unknown;
  aiEntities?: unknown;
  aiKeywords?: unknown;
  aiLanguage?: unknown;
  aiSaveReason?: unknown;
  aiStatus?: unknown;
  aiModel?: unknown;
  aiAnalyzedAt?: unknown;
}

// ---------------------------------------------------------------------------
// GraphQL helpers
// ---------------------------------------------------------------------------

// Recursion depth cap for the tree walkers below. IG payloads nest only a
// handful of levels; a much deeper (or self-referential) object indicates a
// pathological/malicious input, so we stop rather than blow the stack.
const MAX_DEPTH = 50;

// Convert a unix-seconds timestamp to ISO, guarding against values that make
// `Date#toISOString()` throw a RangeError (which would abort the whole import).
function unixToIso(seconds: number | null | undefined): string {
  if (seconds == null) return '';
  const d = new Date(seconds * 1000);
  return isNaN(d.getTime()) ? '' : d.toISOString();
}

// Instagram media ids embed the creation time (top 41 bits = ms since the IG
// epoch 2011-08-24); the shortcode is that id in base64. Returns an ISO string,
// or '' when the shortcode can't be decoded into a plausible date. Fallback for
// payloads that carry a shortcode but no taken_at, so those posts aren't left
// undated (undated posts sort as the oldest in the gallery). Mirrors the same
// helper in webview-injected.js (kept in sync; that file runs in the page
// context and can't require this module).
const IG_SC_ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
function igDateFromShortcode(shortcode: string | null | undefined): string {
  if (!shortcode || typeof shortcode !== 'string') return '';
  // Real IG shortcodes are <= ~12 chars; cap the input (generous margin) so an
  // arbitrarily long string can't trigger the quadratic BigInt accumulation in
  // the loop below and freeze the main process during an untrusted import.
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

/**
 * Recursively search an object for arrays named "edges" whose items
 * have a "node" with a "shortcode" field — the canonical IG post identifier.
 */
function findEdgeArrays(obj: unknown, results: GraphEdge[][] = [], depth = 0): GraphEdge[][] {
  if (!obj || typeof obj !== 'object' || depth > MAX_DEPTH) return results;
  if (Array.isArray(obj)) {
    obj.forEach((item) => findEdgeArrays(item, results, depth + 1));
    return results;
  }
  const rec = obj as Record<string, unknown>;
  for (const key of Object.keys(rec)) {
    const val = rec[key];
    if (
      key === 'edges' &&
      Array.isArray(val) &&
      val.length > 0 &&
      (val[0] as GraphEdge | undefined)?.node?.shortcode
    ) {
      results.push(val as GraphEdge[]);
    } else {
      findEdgeArrays(val, results, depth + 1);
    }
  }
  return results;
}

/**
 * Extract a normalized post object from a GraphQL edge node.
 * Port of nodeToItem() from extension/ig/injected.js, adapted to the
 * electron app's canonical field shape (uses `text`, not `caption`).
 */
// Build the full ordered media list for a GraphQL node, expanding sidecar
// children so carousels yield one entry per slide.
function nodeMedia(node: GraphNode): ParsedMedia[] {
  const childToMedia = (child: GraphMediaSource): ParsedMedia => {
    const isVideo = child.__typename === 'GraphVideo' || child.is_video;
    return {
      type: isVideo ? 'video' : 'image',
      url: child.display_url || child.thumbnail_src || child.thumbnail_resources?.[0]?.src || '',
    };
  };

  const children = node.edge_sidecar_to_children?.edges;
  if (Array.isArray(children) && children.length > 0) {
    return children.map((edge) => childToMedia(edge.node || {})).filter((m) => m.url);
  }
  const single = childToMedia(node);
  return single.url ? [single] : [];
}

function nodeToItem(node: GraphNode): ParsedPost {
  const owner = node.owner || {};
  const igUser = owner.username || node.user?.username || '';
  const caption = node.edge_media_to_caption?.edges?.[0]?.node?.text || node.caption?.text || '';

  let mediaType = 'image';
  if (node.__typename === 'GraphVideo' || node.is_video) {
    mediaType = 'video';
  } else if (node.__typename === 'GraphSidecar' || node.edge_sidecar_to_children) {
    mediaType = 'carousel';
  }

  const media = nodeMedia(node);

  return {
    id: node.id || node.shortcode || '',
    platform: 'instagram',
    shortcode: node.shortcode || '',
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
      (node.taken_at_timestamp
        ? unixToIso(node.taken_at_timestamp)
        : node.taken_at
          ? unixToIso(node.taken_at)
          : '') || igDateFromShortcode(node.shortcode),
  };
}

// ---------------------------------------------------------------------------
// REST (v1 API) helpers
// ---------------------------------------------------------------------------

/**
 * Extract normalized post objects from a REST API v1 items array.
 * Port of parseRestItems() from extension/ig/injected.js.
 */
// Build the full ordered media list for a REST v1 item, expanding
// carousel_media so carousels yield one entry per slide.
function restItemMedia(item: RestItem): ParsedMedia[] {
  const oneMedia = (m: RestItem): ParsedMedia => ({
    type: m.media_type === 2 ? 'video' : 'image',
    url: m.image_versions2?.candidates?.[0]?.url || '',
  });

  if (Array.isArray(item.carousel_media) && item.carousel_media.length > 0) {
    return item.carousel_media.map(oneMedia).filter((m) => m.url);
  }
  const single = oneMedia(item);
  return single.url ? [single] : [];
}

function parseRestItems(items: unknown): ParsedPost[] {
  if (!Array.isArray(items)) return [];
  return items
    .map((item: RestItem): ParsedPost | null => {
      const shortcode = item.code || item.shortcode;
      if (!shortcode) return null;

      const user = item.user || {};
      const igUser = user.username || '';
      const captionRaw =
        (typeof item.caption === 'object' ? item.caption?.text : undefined) ?? item.caption ?? '';
      const caption = typeof captionRaw === 'string' ? captionRaw : '';

      let mediaType = 'image';
      if (item.media_type === 2) mediaType = 'video';
      else if (item.media_type === 8) mediaType = 'carousel';

      const media = restItemMedia(item);
      const thumb =
        media[0]?.url ||
        item.image_versions2?.candidates?.[0]?.url ||
        item.carousel_media?.[0]?.image_versions2?.candidates?.[0]?.url ||
        '';

      return {
        id: String(item.id || item.pk || shortcode),
        platform: 'instagram',
        shortcode,
        postUrl: `https://www.instagram.com/p/${shortcode}/`,
        profileUrl: igUser ? `https://www.instagram.com/${igUser}/` : '',
        authorUsername: igUser,
        authorName: '',
        text: caption,
        thumbnailUrl: thumb,
        mediaType,
        media,
        timestamp:
          (item.taken_at ? unixToIso(item.taken_at) : '') || igDateFromShortcode(shortcode),
      };
    })
    .filter((p): p is ParsedPost => Boolean(p));
}

// ---------------------------------------------------------------------------
// Public: parse a raw API response body
// ---------------------------------------------------------------------------

/**
 * Parse a JSON response body (already decoded) and extract post items.
 * Handles both the REST v1 format (data.items / data.feed_items) and
 * the GraphQL edge format.
 *
 * Returns { items: Post[], hasNextPage: boolean|null }
 */
function parseResponseBody(data: ResponseBody): ParseResult {
  const items: ParsedPost[] = [];
  let hasNextPage: boolean | null = null;

  // --- REST format: data.items or data.feed_items ---
  const restItems = data.items || data.feed_items;
  if (Array.isArray(restItems) && restItems.length > 0) {
    items.push(...parseRestItems(restItems));
    if (typeof data.more_available === 'boolean') {
      hasNextPage = data.more_available;
    }
    return { items, hasNextPage };
  }

  // --- GraphQL format: recursive edge search ---
  const edgeArrays = findEdgeArrays(data);
  for (const edges of edgeArrays) {
    for (const edge of edges) {
      if (edge?.node?.shortcode) {
        items.push(nodeToItem(edge.node));
      }
    }
  }

  // Try to find page_info.has_next_page anywhere in the tree
  function findPageInfo(obj: unknown, depth = 0): boolean | null {
    if (!obj || typeof obj !== 'object' || depth > MAX_DEPTH) return null;
    if (Array.isArray(obj)) {
      for (const item of obj) {
        const r = findPageInfo(item, depth + 1);
        if (r !== null) return r;
      }
      return null;
    }
    const rec = obj as Record<string, unknown>;
    if ('has_next_page' in rec) return rec.has_next_page as boolean;
    for (const key of Object.keys(rec)) {
      const r = findPageInfo(rec[key], depth + 1);
      if (r !== null) return r;
    }
    return null;
  }

  const pageInfo = findPageInfo(data);
  if (pageInfo !== null) hasNextPage = pageInfo;

  return { items, hasNextPage };
}

// ---------------------------------------------------------------------------
// Public: normalize a Chrome extension export record
// ---------------------------------------------------------------------------

/**
 * Normalize a post object exported from the old Chrome extension JSON format
 * into the canonical shape expected by db.bulkUpsert / db.postToRow.
 *
 * Key difference: the extension stores the post text under `caption`,
 * while the electron app uses `text` throughout.
 */
function normalizeExportedPost(post: ExportedPost): NormalizedExportedPost {
  return {
    id: post.id || post.shortcode || '',
    platform: 'instagram',
    shortcode: post.shortcode || '',
    postUrl:
      post.postUrl || (post.shortcode ? `https://www.instagram.com/p/${post.shortcode}/` : ''),
    profileUrl:
      post.profileUrl ||
      (post.authorUsername ? `https://www.instagram.com/${post.authorUsername}/` : ''),
    authorUsername: post.authorUsername || '',
    authorName: post.authorName || '',
    // Extension exports use `caption`; electron app uses `text`
    text: post.caption || post.text || '',
    thumbnailUrl: post.thumbnailUrl || '',
    mediaType: post.mediaType || 'image',
    media: normalizeMediaList(post),
    timestamp: post.timestamp || igDateFromShortcode(post.shortcode),
    // Pass AI analysis fields through when present in the import record.
    // Absent → undefined (never '' / []), so the importer can tell "not
    // provided" from "explicitly empty" and skip writing untouched columns.
    ...normalizeAiFields(post),
  };
}

// Carry over AI analysis fields from an exported record. Strings stay undefined
// when absent; arrays stay undefined when not arrays — never coerced to ''/[].
function normalizeAiFields(post: ExportedPost): ParsedAiFields {
  const str = (v: unknown): string | undefined => (typeof v === 'string' ? v : undefined);
  const arr = (v: unknown): string[] | undefined => (Array.isArray(v) ? v : undefined);
  return {
    aiDescription: str(post.aiDescription),
    aiTags: arr(post.aiTags),
    aiGeneralTags: arr(post.aiGeneralTags),
    aiSpecificTags: arr(post.aiSpecificTags),
    aiCategory: str(post.aiCategory),
    aiContentType: str(post.aiContentType),
    aiEntities: arr(post.aiEntities),
    aiKeywords: arr(post.aiKeywords),
    aiLanguage: str(post.aiLanguage),
    aiSaveReason: str(post.aiSaveReason),
    aiStatus: str(post.aiStatus),
    aiModel: str(post.aiModel),
    aiAnalyzedAt: typeof post.aiAnalyzedAt === 'number' ? post.aiAnalyzedAt : undefined,
  };
}

// Normalize a media list from an exported record: keep an explicit `media`
// array if present, otherwise synthesize a single entry from the thumbnail.
function normalizeMediaList(post: ExportedPost): ParsedMedia[] {
  if (Array.isArray(post.media) && post.media.length > 0) {
    return post.media
      .map(
        (m): ParsedMedia => ({
          type: m.type === 'video' ? 'video' : 'image',
          url: m.url || m.thumbnailUrl || '',
        }),
      )
      .filter((m) => m.url);
  }
  if (post.thumbnailUrl) {
    return [{ type: post.mediaType === 'video' ? 'video' : 'image', url: post.thumbnailUrl }];
  }
  return [];
}

export { parseResponseBody, normalizeExportedPost, igDateFromShortcode };
