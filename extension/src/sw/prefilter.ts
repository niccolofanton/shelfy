// The worker's pre-filter (plan §2.16): the desktop's own validation of a page-controlled batch
// (sanitizeInterceptedBatch in src/lib/browserSanitize.ts: batch cap, bounded non-empty id,
// clamped strings, http(s) media only, ≤ 60 media, the batch platform stamped over the item's)
// before anything is queued. The server sanitizes again with the stricter Rust port (P2-02).
//
// One addition: the desktop sanitizer keeps `{type, url}` per slide and drops the direct video
// URL that the parser emits as `videoUrl` (P2-05), which C5 carries. The loop below is
// sanitizeInterceptedBatch's, item by item, so each sanitized slide can be matched with the raw
// slide it came from; extension/tests/prefilter.test.ts checks that, without `videoUrl`, the
// output equals sanitizeInterceptedBatch's.

import {
  MAX_BATCH_ITEMS,
  MAX_MEDIA,
  MAX_URL_LEN,
  sanitizeInterceptedItem,
} from '../../../src/lib/browserSanitize';
import { isRecord, type Platform } from '../shared/protocol';

export interface WireMedia {
  type: 'image' | 'video';
  url: string;
  /** Direct video URL (MP4) of a video slide; `url` keeps its poster image. */
  videoUrl?: string;
}

/** An item of an ingest batch (C5): the desktop InterceptItem, sanitized. */
export interface WireItem {
  id: string;
  shortcode: string;
  postUrl: string;
  profileUrl: string;
  authorUsername: string;
  authorName: string;
  mediaType: string;
  timestamp: string;
  text: string;
  thumbnailUrl: string;
  media: WireMedia[];
}

export interface PrefilterResult {
  items: WireItem[];
  /** Items the sanitizer refused (no usable id, not an object). */
  rejected: number;
}

/** The desktop sanitizer's URL rule (isHttpUrl in browserSanitize.ts). */
function isHttpUrl(value: unknown): value is string {
  if (typeof value !== 'string' || value.length > MAX_URL_LEN) return false;
  try {
    const protocol = new URL(value).protocol;
    return protocol === 'http:' || protocol === 'https:';
  } catch {
    return false;
  }
}

/** The raw slides sanitizeInterceptedItem keeps, in the same order. */
function keptRawMedia(raw: Record<string, unknown>): Record<string, unknown>[] {
  const media = Array.isArray(raw.media) ? raw.media : [];
  return media
    .filter((m): m is Record<string, unknown> => isRecord(m) && isHttpUrl(m.url))
    .slice(0, MAX_MEDIA);
}

export function prefilterBatch(items: readonly unknown[], platform: Platform): PrefilterResult {
  const out: WireItem[] = [];
  let rejected = 0;
  for (const raw of items) {
    if (out.length >= MAX_BATCH_ITEMS) break;
    const clean = sanitizeInterceptedItem(raw, platform);
    if (!clean) {
      rejected += 1;
      continue;
    }
    const rawMedia = isRecord(raw) ? keptRawMedia(raw) : [];
    const aligned = rawMedia.length === clean.media.length;
    out.push({
      id: clean.id,
      shortcode: clean.shortcode,
      postUrl: clean.postUrl,
      profileUrl: clean.profileUrl,
      authorUsername: clean.authorUsername,
      authorName: clean.authorName,
      mediaType: clean.mediaType,
      timestamp: clean.timestamp,
      text: clean.text,
      thumbnailUrl: clean.thumbnailUrl,
      media: clean.media.map((slide, index): WireMedia => {
        const videoUrl = aligned ? rawMedia[index].videoUrl : undefined;
        return slide.type === 'video' && isHttpUrl(videoUrl)
          ? { type: slide.type, url: slide.url, videoUrl }
          : { type: slide.type, url: slide.url };
      }),
    });
  }
  return { items: out, rejected };
}

/** UTF-8 length of a string, without allocating. */
export function utf8Length(text: string): number {
  let bytes = 0;
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    if (code < 0x80) bytes += 1;
    else if (code < 0x800) bytes += 2;
    else if (code >= 0xd800 && code <= 0xdbff && i + 1 < text.length) {
      const next = text.charCodeAt(i + 1);
      if (next >= 0xdc00 && next <= 0xdfff) {
        bytes += 4;
        i += 1;
      } else bytes += 3;
    } else bytes += 3;
  }
  return bytes;
}

/** Bytes an item takes in a batch body (its JSON, plus the separating comma). */
export function itemBytes(item: WireItem): number {
  return utf8Length(JSON.stringify(item)) + 1;
}
