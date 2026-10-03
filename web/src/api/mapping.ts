// From the UI's shapes to the HTTP API's and back. The UI keeps the desktop's
// data model (Shelfy.*); these pure functions translate the gallery query into
// `GET /api/v1/posts` parameters and the API's posts, stats and collections into
// that model.
import type { BulkSelector, MediaUrls, PostQuery } from '@ui/api/ShelfyClient';
import { thumbHashToDataURL } from '@ui/lib/thumbhash';
import type { components, operations } from './schema';

type Schemas = components['schemas'];
export type ApiPost = Schemas['Post'];
export type ApiPostDetail = Schemas['PostDetail'];
export type ApiPostMedia = Schemas['PostMedia'];
export type ApiMediaObject = Schemas['MediaObject'];
export type ListPostsParams = NonNullable<operations['listPosts']['parameters']['query']>;
export type CountParams = NonNullable<operations['countPosts']['parameters']['query']>;

// Largest page the API serves (§2.9); a bigger window takes several requests.
export const MAX_PAGE_SIZE = 200;

const PLATFORMS: readonly Schemas['Platform'][] = [
  'instagram',
  'twitter',
  'pinterest',
  'web',
  'manual',
];
const MEDIA_TYPES: readonly Schemas['MediaType'][] = [
  'image',
  'images',
  'carousel',
  'video',
  'text',
  'website',
  'file',
];

function oneOf<T extends string>(values: readonly T[], value: string | undefined): T | undefined {
  return values.find((v) => v === value);
}

// The gallery query as `GET /posts` parameters. The desktop's semantics carry
// over: "downloaded" is "stored" on the server, AI-tagged is yes / no, and a
// search ranks by relevance whatever the date order (the desktop orders by
// score first too), so `sort` is only sent without search text.
export function listPostsParams(
  query: PostQuery,
  page: { limit: number; cursor?: string | null; includeTotal?: boolean },
): ListPostsParams {
  const q = query.search?.trim() || undefined;
  const concepts = (query.concepts ?? []).map((c) => c.trim()).filter(Boolean);
  const mediaType = oneOf(MEDIA_TYPES, query.mediaType);
  const params: ListPostsParams = {
    platform: oneOf(PLATFORMS, query.platform),
    source: query.source === 'web' || query.source === 'social' ? query.source : undefined,
    collection: query.collectionId || undefined,
    mediaType: mediaType ? [mediaType] : undefined,
    stored:
      query.downloadStatus === 'downloaded'
        ? 'yes'
        : query.downloadStatus === 'missing'
          ? 'no'
          : undefined,
    aiTagged:
      query.aiTagged === 'tagged' ? 'yes' : query.aiTagged === 'untagged' ? 'no' : undefined,
    aiStatus: query.aiStatus || undefined,
    tag: query.tag || undefined,
    tags: query.tags?.length ? query.tags : undefined,
    tagMode: query.tags?.length ? query.tagMode : undefined,
    entity: query.entity || undefined,
    category: query.category || undefined,
    contentType: query.contentType || undefined,
    aiLanguage: query.aiLanguage || undefined,
    q,
    concept: concepts.length ? concepts : undefined,
    conceptMode: concepts.length && query.conceptMode === 'and' ? 'and' : undefined,
    sort: !q && !concepts.length && query.sortOrder === 'oldest' ? 'oldest' : undefined,
    trash: query.trash || undefined,
    limit: Math.max(1, Math.min(MAX_PAGE_SIZE, Math.floor(page.limit))),
    cursor: page.cursor || undefined,
    includeTotal: page.includeTotal || undefined,
  };
  return params;
}

// The same filters, as `GET /posts/count` takes them (no paging/order): the
// gallery's count pill, and the authoritative total behind "select all
// matching" (P1-14).
export function countParams(query: PostQuery): CountParams {
  const q = query.search?.trim() || undefined;
  const concepts = (query.concepts ?? []).map((c) => c.trim()).filter(Boolean);
  const mediaType = oneOf(MEDIA_TYPES, query.mediaType);
  return {
    platform: oneOf(PLATFORMS, query.platform),
    source: query.source === 'web' || query.source === 'social' ? query.source : undefined,
    collection: query.collectionId || undefined,
    mediaType: mediaType ? [mediaType] : undefined,
    stored:
      query.downloadStatus === 'downloaded'
        ? 'yes'
        : query.downloadStatus === 'missing'
          ? 'no'
          : undefined,
    aiTagged:
      query.aiTagged === 'tagged' ? 'yes' : query.aiTagged === 'untagged' ? 'no' : undefined,
    aiStatus: query.aiStatus || undefined,
    tag: query.tag || undefined,
    tags: query.tags?.length ? query.tags : undefined,
    tagMode: query.tags?.length ? query.tagMode : undefined,
    entity: query.entity || undefined,
    category: query.category || undefined,
    contentType: query.contentType || undefined,
    aiLanguage: query.aiLanguage || undefined,
    q,
    concept: concepts.length ? concepts : undefined,
    conceptMode: concepts.length && query.conceptMode === 'and' ? 'and' : undefined,
    trash: query.trash || undefined,
  };
}

// The same filters again, as the `FilterParams` schema nested in a bulk/trash
// selector's `filter` (P1-11): unlike the query-string forms above, every
// field is present (defaults filled in), because this travels as a JSON
// object rather than an optional query parameter.
export function toFilterParams(query: PostQuery): Schemas['FilterParams'] {
  const q = query.search?.trim() || null;
  const concepts = (query.concepts ?? []).map((c) => c.trim()).filter(Boolean);
  const mediaType = oneOf(MEDIA_TYPES, query.mediaType);
  return {
    platform: oneOf(PLATFORMS, query.platform) ?? null,
    source: query.source === 'web' || query.source === 'social' ? query.source : null,
    collection: query.collectionId ?? null,
    mediaType: mediaType ? [mediaType] : [],
    stored:
      query.downloadStatus === 'downloaded'
        ? 'yes'
        : query.downloadStatus === 'missing'
          ? 'no'
          : null,
    aiTagged: query.aiTagged === 'tagged' ? 'yes' : query.aiTagged === 'untagged' ? 'no' : null,
    aiStatus: query.aiStatus || null,
    tag: query.tag || null,
    tagMode: query.tags?.length ? (query.tagMode ?? 'or') : null,
    tags: query.tags ?? [],
    entity: query.entity || null,
    category: query.category || null,
    contentType: query.contentType || null,
    aiLanguage: query.aiLanguage || null,
    q,
    concept: concepts,
    conceptMode: concepts.length && query.conceptMode === 'and' ? 'and' : null,
    trash: query.trash ?? null,
  };
}

// A `BulkSelector` (ShelfyClient's transport-neutral shape) as the API's
// `PostSelector` (P1-03/P1-11): exactly one of `keys` or `filter`.
// A selector's filter carries only the members that are set: the server refuses
// a null, blank or empty member there (F11), so that a misspelled or missing
// field can never widen a destructive action to the whole library.
function compactFilter(filter: Schemas['FilterParams']): Schemas['FilterParams'] {
  const set = Object.entries(filter).filter(
    ([, value]) =>
      value != null &&
      !(typeof value === 'string' && value.trim() === '') &&
      !(Array.isArray(value) && value.length === 0),
  );
  return Object.fromEntries(set) as Schemas['FilterParams'];
}

export function toPostSelector(selector: BulkSelector): Schemas['PostSelector'] {
  if (selector.filter) {
    const out: Schemas['PostSelector'] = { filter: compactFilter(toFilterParams(selector.filter)) };
    if (selector.exceptKeys?.length) out.exceptKeys = selector.exceptKeys;
    return out;
  }
  return { keys: selector.keys ?? [] };
}

// Query-string form of `params`: arrays repeat their key, unset values are left out.
export function toSearchParams(params: object): URLSearchParams {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value == null) continue;
    for (const v of Array.isArray(value) ? value : [value]) search.append(key, String(v));
  }
  return search;
}

// ── Media references ───────────────────────────────────────────────────────────
// A stored object travels in the UI's "local path" slots (thumbnailPath,
// media[].localPath…) as its same-origin URL. When a grid rendition exists, its
// URL rides along after a `#`: `/media/<sha>.jpg#/media/<sha>.g480.webp`. The
// fragment never reaches the server, and webMedia below splits it again.

export function mediaRef(object: ApiMediaObject | null | undefined): string | null {
  if (!object) return null;
  return object.g480Url ? `${object.url}#${object.g480Url}` : object.url;
}

export const webMedia: MediaUrls = {
  file(ref) {
    if (!ref) return null;
    const hash = ref.indexOf('#');
    return hash === -1 ? ref : ref.slice(0, hash);
  },
  tile(ref) {
    if (!ref) return null;
    const hash = ref.indexOf('#');
    return hash === -1 ? ref : ref.slice(hash + 1);
  },
  isStored: (src) => typeof src === 'string' && src.startsWith('/media/'),
};

// ── Posts ───────────────────────────────────────────────────────────────────────

const seconds = (ms: number | null | undefined): number | null =>
  ms == null ? null : Math.floor(ms / 1000);

function list<T>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
}

// One slide, in the desktop's terms. A video the server has not kept (videos
// are fetched on demand, plan D4) shows its stored poster or remote picture.
export function toPostMedia(m: ApiPostMedia): Shelfy.PostMedia {
  if (m.kind === 'video' && m.videoObject) {
    return { position: m.position, type: 'video', url: m.sourceUrl, localPath: m.videoObject.url };
  }
  return {
    position: m.position,
    type: m.kind === 'file' ? 'file' : 'image',
    url: m.sourceUrl,
    localPath: mediaRef(m.object),
  };
}

// The original post's URL. The desktop rebuilds an X link from the tweet id in
// `id` when the stored one is missing or broken; on the web `id` is the key
// (`x_<tweet id>`, plan §2.8), so the link is rebuilt here instead.
function postUrlOf(p: ApiPost): string | null {
  const broken = !p.postUrl || p.postUrl.includes('//status/');
  if (p.platform === 'twitter' && broken && p.key.startsWith('x_')) {
    return `https://x.com/i/web/status/${p.key.slice(2)}`;
  }
  return p.postUrl;
}

function isDetail(p: ApiPost | ApiPostDetail): p is ApiPostDetail {
  return 'nativeId' in p;
}

// A post of the API as the UI's Shelfy.Post. `id` is the post key; stored
// objects become media references (see mediaRef); the list form leaves the
// detail-only fields (entities, keywords, model, design catalog) empty.
export function toPost(p: ApiPost | ApiPostDetail): Shelfy.Post {
  const detail = isDetail(p) ? p : null;
  const capture = p.webCapture;
  // A kept video plays on hover and in the modal; only single-video posts have one here.
  const keptVideo =
    p.mediaType === 'video' ? p.media.find((m) => m.kind === 'video' && m.videoObject) : undefined;
  const meta =
    capture?.meta && typeof capture.meta === 'object' && !Array.isArray(capture.meta)
      ? (capture.meta as Shelfy.WebMeta)
      : null;
  const webMeta: Shelfy.WebMeta | null =
    capture?.title && !meta?.title ? { ...meta, title: capture.title } : meta;
  return {
    id: p.key,
    platform: p.platform,
    shortcode: p.shortcode,
    postUrl: postUrlOf(p),
    profileUrl: p.profileUrl,
    authorUsername: p.authorUsername,
    authorName: p.authorName,
    text: p.caption,
    thumbnailUrl: p.coverUrl,
    mediaType: p.mediaType,
    timestamp: p.postedAt != null ? new Date(p.postedAt).toISOString() : null,
    thumbnailPath: mediaRef(p.cover),
    previewPath: null,
    imagePath: null,
    videoPath: keptVideo?.videoObject?.url ?? null,
    // Client-decoded ThumbHash (T8 + P1-08): the web's blur-up placeholder,
    // in the same `thumbBlur` slot PostCard already renders.
    thumbBlur: thumbHashToDataURL(p.thumbhash),
    mediaCount: p.mediaCount,
    importedAt: Math.floor(p.importedAt / 1000),
    aiDescription: p.aiDescription,
    aiTags: p.aiTags,
    aiStatus: p.aiStatus as Shelfy.AiStatus | null,
    aiModel: detail?.aiModel ?? null,
    aiAnalyzedAt: seconds(p.aiAnalyzedAt),
    aiCategory: p.aiCategory,
    aiContentType: p.aiContentType,
    aiEntities: detail?.aiEntities ?? [],
    aiKeywords: detail?.aiKeywords ?? [],
    aiLanguage: p.aiLanguage,
    aiSaveReason: p.aiSaveReason,
    aiWeb: (detail?.aiWeb as Shelfy.WebAiCatalog | null | undefined) ?? null,
    userNote: p.userNote,
    userTags: p.userTags,
    webUrl: p.webUrl,
    webDomain: p.webDomain,
    webFinalUrl: p.webFinalUrl,
    webPalette: list(capture?.palette),
    webFonts: list(capture?.fonts),
    webTech: list<unknown>(capture?.tech).filter((t): t is string => typeof t === 'string'),
    webAwards: list(capture?.awards),
    webPages: [],
    webMeta,
    webSinglePage: false,
    webCapturedAt: seconds(capture?.capturedAt),
    // The favicon stored at capture time (plan §1.2 #10): no g480 rendition
    // exists for it, so this is the object's own URL, not `mediaRef`'s
    // tile-fragment form.
    webFaviconPath: capture?.favicon?.url ?? null,
    media: p.media.map(toPostMedia),
    collectionIds: p.collectionIds,
    deletedAt: seconds(p.deletedAt),
  };
}

// ── Stats and collections ───────────────────────────────────────────────────────

// "Downloaded" counters are the server's "stored" ones (cover = thumbnail).
export function toStats(s: Schemas['Stats']): Shelfy.Stats {
  return {
    total: s.total,
    byPlatform: { ...s.byPlatform },
    byMediaType: s.byMediaType,
    downloaded: s.stored,
    downloadedByType: {
      thumbnails: s.storedByKind.covers,
      images: s.storedByKind.images,
      videos: s.storedByKind.videos,
    },
  };
}

export function toCollection(c: Schemas['Collection']): Shelfy.Collection {
  return {
    id: c.id,
    name: c.name,
    color: c.color,
    platform: c.platform,
    externalId: c.externalId,
    igName: c.sourceName,
    count: c.count,
    createdAt: Math.floor(c.createdAt / 1000),
  };
}
