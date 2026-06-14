'use strict';

/**
 * tw-parser.js
 * Parse Twitter/X bookmark API response data into normalized Post objects.
 * Used for batch import and data normalization (e.g. importing Chrome extension JSON exports).
 *
 * NOTE: Real-time interception during browsing is handled by webview-preload.js.
 * This module is for batch/offline use only (main process, no window/DOM available).
 */

// ---------------------------------------------------------------------------
// Core tweet extraction
// ---------------------------------------------------------------------------

// Twitter's `created_at` is a human string ("Wed Oct 10 20:19:24 +0000 2018").
// A malformed/absent value makes `new Date(...).toISOString()` throw a
// RangeError, which would abort the whole import. Guard it and fall back to ''.
function toIso(value) {
  if (!value) return '';
  const d = new Date(value);
  return isNaN(d.getTime()) ? '' : d.toISOString();
}

/**
 * Extract a normalized post object from a single tweet result entry.
 * Port of extractTweet() from extension/tw/injected.js, adapted to the
 * electron app's canonical field shape.
 *
 * @param {object} result - tweet_results.result from the GraphQL response
 * @returns {object|null} normalized Post or null if the entry is invalid
 */
function extractTweet(result) {
  if (!result) return null;

  // Unwrap TweetWithVisibilityResults wrapper when present
  const tweet = result.tweet || result;
  const legacy = tweet.legacy || {};
  const tweetId = tweet.rest_id || legacy.id_str || '';
  if (!tweetId) return null;

  const userResult =
    tweet.core?.user_results?.result ||
    tweet.user_results?.result ||
    {};
  // Twitter moved screen_name/name from user.legacy to a new user.core object in
  // 2024; read the new location first, fall back to legacy for older payloads.
  const userLegacy = userResult.legacy || {};
  const userCore = userResult.core || {};

  const authorUsername = userCore.screen_name || userLegacy.screen_name || '';
  const authorName = userCore.name || userLegacy.name || '';
  const fullText = legacy.full_text || legacy.text || '';
  const createdAt = legacy.created_at || '';

  const mediaEntities =
    legacy.extended_entities?.media ||
    legacy.entities?.media ||
    [];

  let mediaType = 'text';
  if (mediaEntities.length > 0) {
    const type = mediaEntities[0].type;
    if (type === 'video' || type === 'animated_gif') mediaType = 'video';
    else if (mediaEntities.length > 1) mediaType = 'images';
    else mediaType = 'image';
  }

  const media = mediaEntities
    .map((m) => ({
      type: m.type === 'video' || m.type === 'animated_gif' ? 'video' : 'image',
      url: m.media_url_https || '',
    }))
    .filter((m) => m.url);

  const thumbnailUrl =
    media[0]?.url ||
    userLegacy.profile_image_url_https ||
    '';

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
function parseBookmarkResponse(data) {
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
function normalizeExportedPost(post) {
  return {
    id: post.id,
    platform: 'twitter',
    shortcode: '',
    postUrl:
      post.postUrl ||
      (post.id
        ? `https://x.com/${post.authorUsername || 'i'}/status/${post.id}`
        : ''),
    profileUrl:
      post.profileUrl ||
      (post.authorUsername ? `https://x.com/${post.authorUsername}` : ''),
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
function normalizeAiFields(post) {
  const str = (v) => (typeof v === 'string' ? v : undefined);
  const arr = (v) => (Array.isArray(v) ? v : undefined);
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
function normalizeMediaList(post) {
  if (Array.isArray(post.media) && post.media.length > 0) {
    return post.media
      .map((m) => ({
        type: m.type === 'video' ? 'video' : 'image',
        url: m.url || m.thumbnailUrl || '',
      }))
      .filter((m) => m.url);
  }
  if (post.thumbnailUrl && post.mediaType !== 'text') {
    return [{ type: post.mediaType === 'video' ? 'video' : 'image', url: post.thumbnailUrl }];
  }
  return [];
}

module.exports = { extractTweet, parseBookmarkResponse, normalizeExportedPost };
