/**
 * tw-parser.ts
 * Parse Twitter/X bookmark API response data into normalized Post objects.
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
  platform: 'twitter';
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

// Loose, structurally-typed views of the open-ended Twitter/X GraphQL payload
// shapes. The upstream JSON is untrusted/dynamic, so every field is optional and
// indexing stays type-safe; unknown sub-trees are narrowed where walked.
interface TweetMediaEntity {
  type?: string;
  media_url_https?: string;
}

interface TweetLegacy {
  id_str?: string;
  full_text?: string;
  text?: string;
  created_at?: string;
  extended_entities?: { media?: TweetMediaEntity[] };
  entities?: { media?: TweetMediaEntity[] };
}

interface TweetUserLegacy {
  screen_name?: string;
  name?: string;
  profile_image_url_https?: string;
}

interface TweetUserCore {
  screen_name?: string;
  name?: string;
}

interface TweetUserResult {
  legacy?: TweetUserLegacy;
  core?: TweetUserCore;
}

interface TweetCore {
  rest_id?: string;
  legacy?: TweetLegacy;
  core?: { user_results?: { result?: TweetUserResult } };
  user_results?: { result?: TweetUserResult };
}

// A `tweet_results.result` entry: either a bare tweet or a
// TweetWithVisibilityResults wrapper carrying the tweet under `tweet`.
interface TweetResult extends TweetCore {
  tweet?: TweetCore;
}

interface TimelineEntry {
  entryId?: string;
  content?: {
    cursorType?: string;
    value?: unknown;
    itemContent?: { tweet_results?: { result?: TweetResult } };
  };
}

interface TimelineInstruction {
  type?: string;
  entries?: TimelineEntry[];
}

// A raw API response body (already JSON-decoded). Handles both the
// bookmark_timeline_v2 and bookmarks_timeline GraphQL shapes plus arbitrary
// nested trees, so it's an open record.
interface ResponseBody {
  data?: {
    bookmark_timeline_v2?: { timeline?: { instructions?: TimelineInstruction[] } };
    bookmarks_timeline?: { timeline?: { instructions?: TimelineInstruction[] } };
  };
  [key: string]: unknown;
}

// The return shape of parseBookmarkResponse.
interface ParseResult {
  items: ParsedPost[];
  hasNextPage: boolean | null;
}

// A post record exported from the old Chrome extension JSON format. Loose since
// it's an untrusted on-disk record.
interface ExportedPost {
  id?: string;
  postUrl?: string;
  profileUrl?: string;
  authorUsername?: string;
  authorName?: string;
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
// Core tweet extraction
// ---------------------------------------------------------------------------

// Twitter's `created_at` is a human string ("Wed Oct 10 20:19:24 +0000 2018").
// A malformed/absent value makes `new Date(...).toISOString()` throw a
// RangeError, which would abort the whole import. Guard it and fall back to ''.
function toIso(value: string | null | undefined): string {
  if (!value) return '';
  const d = new Date(value);
  return isNaN(d.getTime()) ? '' : d.toISOString();
}

/**
 * Extract a normalized post object from a single tweet result entry.
 * Port of extractTweet() from extension/tw/injected.js, adapted to the
 * electron app's canonical field shape.
 *
 * @param result - tweet_results.result from the GraphQL response
 * @returns normalized Post or null if the entry is invalid
 */
function extractTweet(result: TweetResult | null | undefined): ParsedPost | null {
  if (!result) return null;

  // Unwrap TweetWithVisibilityResults wrapper when present
  const tweet = result.tweet || result;
  const legacy = tweet.legacy || {};
  const tweetId = tweet.rest_id || legacy.id_str || '';
  if (!tweetId) return null;

  const userResult: TweetUserResult =
    tweet.core?.user_results?.result || tweet.user_results?.result || {};
  // Twitter moved screen_name/name from user.legacy to a new user.core object in
  // 2024; read the new location first, fall back to legacy for older payloads.
  const userLegacy = userResult.legacy || {};
  const userCore = userResult.core || {};

  const authorUsername = userCore.screen_name || userLegacy.screen_name || '';
  const authorName = userCore.name || userLegacy.name || '';
  const fullText = legacy.full_text || legacy.text || '';
  const createdAt = legacy.created_at || '';

  const mediaEntities: TweetMediaEntity[] =
    legacy.extended_entities?.media || legacy.entities?.media || [];

  let mediaType = 'text';
  if (mediaEntities.length > 0) {
    const type = mediaEntities[0].type;
    if (type === 'video' || type === 'animated_gif') mediaType = 'video';
    else if (mediaEntities.length > 1) mediaType = 'images';
    else mediaType = 'image';
  }

  const media = mediaEntities
    .map(
      (m): ParsedMedia => ({
        type: m.type === 'video' || m.type === 'animated_gif' ? 'video' : 'image',
        url: m.media_url_https || '',
      }),
    )
    .filter((m) => m.url);

  const thumbnailUrl = media[0]?.url || userLegacy.profile_image_url_https || '';

  return {
    id: tweetId,
    platform: 'twitter',
    shortcode: '',
    postUrl: `https://x.com/${authorUsername || 'i'}/status/${tweetId}`,
    profileUrl: authorUsername ? `https://x.com/${authorUsername}` : '',
    authorUsername,
    authorName,
    text: fullText,
    thumbnailUrl,
    mediaType,
    media,
    timestamp: toIso(createdAt),
  };
}

// ---------------------------------------------------------------------------
// Public: parse a raw API response body
// ---------------------------------------------------------------------------

/**
 * Parse a JSON bookmark response body (already decoded) and extract tweet items.
 * Handles both bookmark_timeline_v2 and bookmarks_timeline GraphQL shapes.
 *
 * Returns { items: Post[], hasNextPage: boolean|null }
 */
function parseBookmarkResponse(data: ResponseBody): ParseResult {
  const items: ParsedPost[] = [];
  let hasNextPage: boolean | null = null;

  const instructions: TimelineInstruction[] =
    data?.data?.bookmark_timeline_v2?.timeline?.instructions ||
    data?.data?.bookmarks_timeline?.timeline?.instructions ||
    [];

  let tweetCount = 0;

  for (const instruction of instructions) {
    if (instruction.type !== 'TimelineAddEntries') continue;

    for (const entry of instruction.entries || []) {
      const entryId = entry.entryId || '';
      const content = entry.content || {};

      if (content.cursorType === 'Bottom' || entryId.startsWith('cursor-bottom')) {
        hasNextPage = !!content.value;
        continue;
      }

      const tweetResult = content.itemContent?.tweet_results?.result;
      if (!tweetResult) continue;

      const item = extractTweet(tweetResult);
      if (item) {
        items.push(item);
        tweetCount++;
      }
    }
  }

  // No tweets in a non-empty instruction list means we've reached the end
  if (instructions.length > 0 && tweetCount === 0) {
    hasNextPage = false;
  }

  return { items, hasNextPage };
}

// ---------------------------------------------------------------------------
// Public: normalize a Chrome extension export record
// ---------------------------------------------------------------------------

/**
 * Normalize a post object exported from the old Chrome extension JSON format
 * into the canonical shape expected by db.bulkUpsert / db.postToRow.
 *
 * Twitter extension exports already use `text`, `authorUsername`, and `authorName`,
 * so this is mostly a shape guarantee with safe fallbacks.
 */
function normalizeExportedPost(post: ExportedPost): NormalizedExportedPost {
  return {
    id: post.id || '',
    platform: 'twitter',
    shortcode: '',
    postUrl:
      post.postUrl ||
      (post.id ? `https://x.com/${post.authorUsername || 'i'}/status/${post.id}` : ''),
    profileUrl:
      post.profileUrl || (post.authorUsername ? `https://x.com/${post.authorUsername}` : ''),
    authorUsername: post.authorUsername || '',
    authorName: post.authorName || '',
    text: post.text || '',
    thumbnailUrl: post.thumbnailUrl || '',
    mediaType: post.mediaType || 'text',
    media: normalizeMediaList(post),
    timestamp: post.timestamp || '',
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
  if (post.thumbnailUrl && post.mediaType !== 'text') {
    return [{ type: post.mediaType === 'video' ? 'video' : 'image', url: post.thumbnailUrl }];
  }
  return [];
}

export { extractTweet, parseBookmarkResponse, normalizeExportedPost };
