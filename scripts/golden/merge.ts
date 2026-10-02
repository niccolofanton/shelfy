// Golden sets for the desktop's `bulkUpsert` (electron/db.ts, DATA-04): the
// merge rules of ingest, ported to crates/core/src/ingest/merge.rs.
//
// Each scenario is one file, shared/golden/merge/<scenario>.jsonl. Its cases are
// the steps of the scenario, in order, on one fresh in-memory desktop library:
//
//   args:   [{ now, overwriteAi, aliases?, posts }]
//   output: { inserted, skipped, aiUpdated, posts: [<view of every post>] }
//
// A step pins the desktop's clocks to `now` (lib.ts `withDesktopClock`), adds
// the accepted or proposed `aliases` rows if any, then calls
// `bulkUpsert(posts, { overwriteAi })`. The output is what bulkUpsert returned
// and the library afterwards, as a view that the web schema can express too:
//
// - `postedAt` is the post date, null while the desktop only has its
//   insert-time fallback ("now" of the step that inserted it); `sortTs` is the
//   date the gallery sorts by, the fallback included (web: `sort_ts`).
// - `cover` is the post's own archived file: `thumbnail_path`, else
//   `image_path`, else `video_path` (web: `cover_object`). `media[].local` is a
//   slide's file (web: `post_media.object_id`). Paths are opaque tokens.
// - `ai.analyzedAt` is in milliseconds (the desktop stores seconds).
// - `ai.tags`, `ai.entities` and `ai.keywords` are the stored JSON arrays,
//   `[]` when NULL; `tagRows` and `entityRows` are the derived rows.
//
// The Rust test feeds the same posts to its port, with each path turned into a
// media object, and must produce the same JSON, byte for byte.
//
// Inputs are restricted to what the web schema can represent (checked below):
// social platforms only, one id per post, dates that are empty or ISO 8601 in
// UTC, string arrays, and analysis times in seconds. Behaviors the port changes
// on purpose are tested in Rust instead (see crates/core/src/ingest/merge.rs).

import type { GoldenCase, GoldenSet } from './lib';
import { openDesktopDb, withDesktopClock } from './lib';
import type BetterSqlite3 from 'better-sqlite3';

// ── Fixture shapes ──────────────────────────────────────────────────────────

interface FixtureMedia {
  type?: string;
  url?: string | null;
  localPath?: string | null;
}

/** A post as the desktop's sync and import paths hand it to bulkUpsert. */
interface FixturePost {
  id: string;
  platform: 'instagram' | 'twitter' | 'pinterest';
  mediaType: string;
  shortcode?: string | null;
  postUrl?: string | null;
  profileUrl?: string | null;
  authorUsername?: string | null;
  authorName?: string | null;
  text?: string | null;
  thumbnailUrl?: string | null;
  timestamp?: string | null;
  thumbnailPath?: string | null;
  imagePath?: string | null;
  videoPath?: string | null;
  media?: FixtureMedia[];
  webUrl?: string | null;
  webDomain?: string | null;
  webFinalUrl?: string | null;
  aiDescription?: string | null;
  aiTags?: string[] | null;
  aiStatus?: string | null;
  aiModel?: string | null;
  aiCategory?: string | null;
  aiContentType?: string | null;
  aiEntities?: string[] | null;
  aiKeywords?: string[] | null;
  aiLanguage?: string | null;
  aiSaveReason?: string | null;
  aiAnalyzedAt?: number | null;
  aiGeneralTags?: string[] | null;
  aiSpecificTags?: string[] | null;
}

/** A `tag_alias` row, added before the step's posts. */
interface FixtureAlias {
  aliasNorm: string;
  canonicalNorm: string;
  canonicalForm: string;
  status: 'accepted' | 'proposed';
}

interface Step {
  now: number;
  overwriteAi: boolean;
  aliases?: FixtureAlias[];
  posts: FixturePost[];
}

interface Scenario {
  name: string;
  steps: [string, Step][];
}

const STRING_KEYS = [
  'shortcode',
  'postUrl',
  'profileUrl',
  'authorUsername',
  'authorName',
  'text',
  'thumbnailUrl',
  'timestamp',
  'thumbnailPath',
  'imagePath',
  'videoPath',
  'webUrl',
  'webDomain',
  'webFinalUrl',
  'aiDescription',
  'aiStatus',
  'aiModel',
  'aiCategory',
  'aiContentType',
  'aiLanguage',
  'aiSaveReason',
] as const;
const ARRAY_KEYS = [
  'aiTags',
  'aiEntities',
  'aiKeywords',
  'aiGeneralTags',
  'aiSpecificTags',
] as const;
const POST_KEYS = new Set<string>([
  'id',
  'platform',
  'mediaType',
  'media',
  'aiAnalyzedAt',
  ...STRING_KEYS,
  ...ARRAY_KEYS,
]);
const MEDIA_KEYS = new Set(['type', 'url', 'localPath']);

// ── Scenarios ───────────────────────────────────────────────────────────────

/** 2026-10-02T00:00:00Z: "now" of the first step of every scenario. */
const DAY0 = Date.UTC(2026, 9, 2);
const DAY = 86_400_000;
const day = (n: number): number => DAY0 + n * DAY;

const IG = 'https://scontent-mxp1-1.cdninstagram.com/v/t51.29350-15';
const PBS = 'https://pbs.twimg.com/media';
const PIN = 'https://i.pinimg.com/736x';

/** One batch that holds each post twice, with different values. */
const REPEATED: FixturePost[] = [
  {
    id: '1123581321345589144',
    platform: 'instagram',
    text: 'First copy',
    mediaType: 'carousel',
    timestamp: '2025-02-01T00:00:00.000Z',
    media: [
      { type: 'image', url: `${IG}/rep-0.jpg?oe=6A000006` },
      { type: 'image', url: `${IG}/rep-1.jpg?oe=6A000006` },
    ],
    aiTags: ['one'],
  },
  {
    id: '1123581321345589144',
    platform: 'instagram',
    text: 'Second copy',
    mediaType: 'carousel',
    timestamp: '',
    media: [{ type: 'image', url: `${IG}/rep-0b.jpg?oe=6A000006` }],
    aiDescription: 'two',
  },
  {
    id: '1790006667778889991',
    platform: 'twitter',
    text: 'Archived first copy',
    mediaType: 'image',
    timestamp: '2025-02-02T00:00:00.000Z',
    thumbnailPath: '/archive/1790006667778889991/cover.jpg',
  },
  {
    id: '1790006667778889991',
    platform: 'twitter',
    text: 'Archived second copy',
    mediaType: 'image',
    timestamp: '2025-02-03T00:00:00.000Z',
  },
  {
    id: '987654321098765438',
    platform: 'pinterest',
    text: 'Undated first',
    mediaType: 'image',
    timestamp: '',
  },
  {
    id: '987654321098765438',
    platform: 'pinterest',
    text: 'Dated second',
    mediaType: 'image',
    timestamp: '2025-02-04T00:00:00.000Z',
    aiTags: ['late'],
    aiStatus: 'done',
  },
];

const SCENARIOS: Scenario[] = [
  {
    // New posts: every field, the slides derived from `media` or from the
    // cover, and the insert-time date fallback.
    name: 'insert',
    steps: [
      [
        'first-sync',
        {
          now: day(0),
          overwriteAi: false,
          posts: [
            {
              id: '3141592653589793238',
              platform: 'instagram',
              shortcode: 'CuR8Zk1N2pA',
              postUrl: 'https://www.instagram.com/p/CuR8Zk1N2pA/',
              profileUrl: 'https://www.instagram.com/studio.lumen/',
              authorUsername: 'studio.lumen',
              authorName: 'Studio Lumen',
              text: 'Lampada da tavolo in vetro soffiato #design #lighting',
              thumbnailUrl: `${IG}/lamp-cover.jpg?stp=dst-jpg&oe=6A1B2C3D`,
              mediaType: 'carousel',
              timestamp: '2024-03-14T09:26:53.589Z',
              // The url-less slide is dropped and the next one moves up;
              // any type but `video` is an image.
              media: [
                { type: 'image', url: `${IG}/lamp-1.jpg?oe=6A1B2C3D` },
                { type: 'video', url: `${IG}/lamp-2.jpg?oe=6A1B2C3D` },
                { type: 'image', url: '' },
                { type: 'carousel_item', url: `${IG}/lamp-4.jpg?oe=6A1B2C3D` },
              ],
            },
            {
              // No slides from the parser: one image slide from the cover. No
              // date: the import time.
              id: '3141592653589793239',
              platform: 'instagram',
              shortcode: 'CuR8Zk1N2pB',
              authorUsername: 'casa.ortica',
              text: 'Cucina in rovere',
              thumbnailUrl: `${IG}/kitchen.jpg?oe=6A1B2C3D`,
              mediaType: 'image',
              timestamp: '',
            },
            {
              // A video cover becomes a video slide; the date is absent.
              id: '3141592653589793240',
              platform: 'instagram',
              shortcode: 'CuR8Zk1N2pC',
              thumbnailUrl: `${IG}/reel-poster.jpg?oe=6A1B2C3D`,
              mediaType: 'video',
            },
            {
              // A text post gets no slide, even with a cover URL.
              id: '1790001112223334441',
              platform: 'twitter',
              postUrl: 'https://x.com/typeforms/status/1790001112223334441',
              authorUsername: 'typeforms',
              authorName: 'Type Forms',
              text: 'Grotesk revival, thread ↓',
              thumbnailUrl: `${PBS}/GrotA.jpg?name=small`,
              mediaType: 'text',
              timestamp: null,
            },
            {
              // Slides without a URL are dropped, and the cover is not used
              // instead because the parser did send slides.
              id: '1790001112223334442',
              platform: 'twitter',
              postUrl: 'https://x.com/i/status/1790001112223334442',
              thumbnailUrl: `${PBS}/Fallback.jpg`,
              mediaType: 'images',
              timestamp: '2023-11-05T18:00:00.000Z',
              media: [{ type: 'image' }, { type: 'image', url: null }],
            },
            {
              // What the renderer's sanitizer sends for a pin with no data:
              // empty strings, which are stored as such.
              id: '987654321098765432',
              platform: 'pinterest',
              shortcode: '',
              postUrl: '',
              profileUrl: '',
              authorUsername: '',
              authorName: '',
              text: '',
              thumbnailUrl: '',
              mediaType: 'image',
              timestamp: '',
              media: [],
            },
            {
              id: '1790001112223334443',
              platform: 'twitter',
              postUrl: 'https://x.com/atelier_ö/status/1790001112223334443',
              authorUsername: 'atelier_ö',
              authorName: 'Atelier Ö — Zoë Ünal',
              text: 'Città, café, 東京 デザイン 🔥 “quotes” \\ back\nslash\ttab',
              thumbnailUrl: `${PBS}/Multi0.jpg`,
              mediaType: 'images',
              timestamp: '2019-01-01T00:00:00.000Z',
              media: [
                { type: 'image', url: `${PBS}/Multi0.jpg?format=jpg&name=large` },
                { type: 'image', url: `${PBS}/Multi1.jpg?format=jpg&name=large` },
              ],
            },
          ],
        },
      ],
    ],
  },
  {
    // Re-imports of posts with no archived file: the metadata follows the
    // newest import (empty values included), a known date survives an import
    // without one, and slides are refreshed or added, never removed.
    name: 'refresh',
    steps: [
      [
        'first-sync',
        {
          now: day(0),
          overwriteAi: false,
          posts: [
            {
              id: '2718281828459045235',
              platform: 'instagram',
              shortcode: 'CwE1aB2cD3e',
              postUrl: 'https://www.instagram.com/p/CwE1aB2cD3e/',
              profileUrl: 'https://www.instagram.com/nora.k/',
              authorUsername: 'nora.k',
              authorName: 'Nora K',
              text: 'First caption',
              thumbnailUrl: `${IG}/nora-cover.jpg?oe=6A000001`,
              mediaType: 'carousel',
              timestamp: '2022-06-01T12:00:00.000Z',
              webUrl: 'https://nora.example/',
              webDomain: 'nora.example',
              webFinalUrl: 'https://nora.example/',
              media: [
                { type: 'image', url: `${IG}/nora-0.jpg?oe=6A000001` },
                { type: 'image', url: `${IG}/nora-1.jpg?oe=6A000001` },
              ],
            },
            {
              id: '1790002223334445551',
              platform: 'twitter',
              authorUsername: 'undated',
              text: 'No date on first sight',
              mediaType: 'text',
              timestamp: '',
            },
            {
              id: '987654321098765433',
              platform: 'pinterest',
              postUrl: 'https://www.pinterest.com/pin/987654321098765433/',
              text: 'Board pin',
              thumbnailUrl: `${PIN}/aa/bb/pin.jpg`,
              mediaType: 'image',
              timestamp: '2021-02-03T04:05:06.007Z',
              media: [{ type: 'image', url: `${PIN}/aa/bb/pin.jpg` }],
            },
            {
              id: '2718281828459045236',
              platform: 'instagram',
              shortcode: 'CwE1aB2cD3f',
              mediaType: 'video',
              timestamp: '2022-06-02T12:00:00.000Z',
              media: [{ type: 'video', url: `${IG}/reel-a.jpg?oe=6A000001` }],
            },
          ],
        },
      ],
      [
        'second-sync',
        {
          now: day(1),
          overwriteAi: false,
          posts: [
            {
              // Fewer slides: slide 1 stays. Empty and missing values replace
              // the old ones; the empty date keeps the old date.
              id: '2718281828459045235',
              platform: 'instagram',
              shortcode: 'CwE1aB2cD3e',
              postUrl: 'https://www.instagram.com/p/CwE1aB2cD3e/',
              profileUrl: '',
              authorUsername: 'nora.k',
              authorName: null,
              text: 'Edited caption',
              thumbnailUrl: `${IG}/nora-cover.jpg?oe=6B000002`,
              mediaType: 'image',
              timestamp: '',
              webUrl: null,
              media: [{ type: 'image', url: `${IG}/nora-0.jpg?oe=6B000002` }],
            },
            {
              // A date at last: it replaces the fallback.
              id: '1790002223334445551',
              platform: 'twitter',
              authorUsername: 'undated',
              text: 'No date on first sight',
              mediaType: 'text',
              timestamp: '2020-07-08T09:10:11.120Z',
            },
            {
              // A known date replaces a known date; new slides are added.
              id: '987654321098765433',
              platform: 'pinterest',
              shortcode: 'pin-sc',
              postUrl: 'https://www.pinterest.com/pin/987654321098765433/',
              text: 'Board pin',
              thumbnailUrl: `${PIN}/aa/bb/pin.jpg`,
              mediaType: 'carousel',
              timestamp: '2021-02-04T00:00:00.000Z',
              media: [
                { type: 'image', url: `${PIN}/aa/bb/pin.jpg` },
                { type: 'image', url: `${PIN}/cc/dd/pin-2.jpg` },
                { type: 'video', url: `${PIN}/ee/ff/pin-3.jpg` },
              ],
            },
            {
              // A slide's type follows its URL.
              id: '2718281828459045236',
              platform: 'instagram',
              shortcode: 'CwE1aB2cD3f',
              mediaType: 'image',
              media: [{ type: 'image', url: `${IG}/reel-a-still.jpg?oe=6B000002` }],
            },
          ],
        },
      ],
      [
        'third-sync',
        {
          now: day(2),
          overwriteAi: false,
          posts: [
            {
              // No slides and no cover: slides untouched, media count 1.
              id: '2718281828459045235',
              platform: 'instagram',
              mediaType: 'text',
              timestamp: null,
            },
            {
              id: '1790002223334445551',
              platform: 'twitter',
              text: 'Same post, date missing again',
              mediaType: 'text',
            },
          ],
        },
      ],
    ],
  },
  {
    // Posts with archived files: their metadata and date are frozen, slides
    // with a file keep their URL, and an import never adds files to a post
    // that already exists.
    name: 'archived',
    steps: [
      [
        'sync-with-files',
        {
          now: day(0),
          overwriteAi: false,
          posts: [
            {
              id: '1618033988749894848',
              platform: 'instagram',
              shortcode: 'Cx1fA2gB3hC',
              authorUsername: 'keeper',
              text: 'Downloaded cover',
              thumbnailUrl: `${IG}/keep-cover.jpg?oe=6A000003`,
              mediaType: 'carousel',
              timestamp: '2023-01-01T00:00:00.000Z',
              thumbnailPath: '/archive/1618033988749894848/cover.jpg',
              media: [
                {
                  type: 'image',
                  url: `${IG}/keep-0.jpg?oe=6A000003`,
                  localPath: '/archive/1618033988749894848/0.jpg',
                },
                { type: 'image', url: `${IG}/keep-1.jpg?oe=6A000003` },
              ],
            },
            {
              id: '1618033988749894849',
              platform: 'instagram',
              text: 'Downloaded video',
              thumbnailUrl: `${IG}/kept-reel.jpg?oe=6A000003`,
              mediaType: 'video',
              timestamp: '2023-01-02T00:00:00.000Z',
              videoPath: '/archive/1618033988749894849/video.mp4',
            },
            {
              id: '1790003334445556661',
              platform: 'twitter',
              text: 'Downloaded image',
              mediaType: 'image',
              timestamp: '2023-01-03T00:00:00.000Z',
              imagePath: '/archive/1790003334445556661/image.jpg',
              media: [{ type: 'image', url: `${PBS}/Kept.jpg?name=large` }],
            },
            {
              // Only a slide has a file: the post itself is not archived.
              id: '1618033988749894850',
              platform: 'instagram',
              text: 'One slide downloaded',
              mediaType: 'carousel',
              timestamp: '2023-01-04T00:00:00.000Z',
              media: [
                {
                  type: 'image',
                  url: `${IG}/half-0.jpg?oe=6A000003`,
                  localPath: '/archive/1618033988749894850/0.jpg',
                },
                { type: 'video', url: `${IG}/half-1.jpg?oe=6A000003` },
              ],
            },
            {
              id: '987654321098765434',
              platform: 'pinterest',
              text: 'Not downloaded',
              thumbnailUrl: `${PIN}/11/22/plain.jpg`,
              mediaType: 'image',
              timestamp: '2023-01-05T00:00:00.000Z',
            },
          ],
        },
      ],
      [
        'resync',
        {
          now: day(1),
          overwriteAi: false,
          posts: [
            {
              id: '1618033988749894848',
              platform: 'instagram',
              shortcode: 'Cx1fA2gB3hD',
              authorUsername: 'keeper.renamed',
              text: 'New caption, ignored',
              thumbnailUrl: `${IG}/keep-cover.jpg?oe=6B000004`,
              mediaType: 'images',
              timestamp: '2024-05-05T05:05:05.005Z',
              media: [
                {
                  type: 'video',
                  url: `${IG}/keep-0-new.jpg?oe=6B000004`,
                  localPath: '/elsewhere/0.jpg',
                },
                { type: 'video', url: `${IG}/keep-1-new.jpg?oe=6B000004` },
                {
                  type: 'image',
                  url: `${IG}/keep-2.jpg?oe=6B000004`,
                  localPath: '/archive/1618033988749894848/2.jpg',
                },
              ],
            },
            {
              id: '1618033988749894849',
              platform: 'instagram',
              text: 'Video caption edited',
              thumbnailUrl: `${IG}/kept-reel-2.jpg?oe=6B000004`,
              mediaType: 'video',
              timestamp: '',
            },
            {
              id: '1790003334445556661',
              platform: 'twitter',
              text: 'Image caption edited',
              mediaType: 'image',
              timestamp: '2024-01-03T00:00:00.000Z',
              media: [{ type: 'image', url: `${PBS}/Kept2.jpg?name=large` }],
            },
            {
              id: '1618033988749894850',
              platform: 'instagram',
              text: 'Half-archived caption edited',
              mediaType: 'carousel',
              timestamp: '2024-01-04T00:00:00.000Z',
              media: [
                { type: 'image', url: `${IG}/half-0-new.jpg?oe=6B000004` },
                { type: 'image', url: `${IG}/half-1-new.jpg?oe=6B000004` },
              ],
            },
            {
              // Files sent with an existing post are not recorded.
              id: '987654321098765434',
              platform: 'pinterest',
              text: 'Files arrive late',
              thumbnailUrl: `${PIN}/11/22/plain.jpg`,
              mediaType: 'image',
              timestamp: '2024-01-05T00:00:00.000Z',
              thumbnailPath: '/archive/987654321098765434/cover.jpg',
              imagePath: '/archive/987654321098765434/image.jpg',
              videoPath: '/archive/987654321098765434/video.mp4',
              media: [
                {
                  type: 'image',
                  url: `${PIN}/11/22/plain.jpg`,
                  localPath: '/archive/987654321098765434/0.jpg',
                },
              ],
            },
          ],
        },
      ],
      [
        'resync-again',
        {
          now: day(2),
          overwriteAi: false,
          posts: [
            {
              id: '987654321098765434',
              platform: 'pinterest',
              text: 'Still not archived, so still refreshed',
              mediaType: 'image',
              timestamp: '',
            },
            {
              id: '1618033988749894848',
              platform: 'instagram',
              mediaType: 'carousel',
              media: [{ type: 'image', url: `${IG}/keep-0-newer.jpg?oe=6C000005` }],
            },
          ],
        },
      ],
    ],
  },
  {
    // AI fields on new posts: whatever is present is written; tags and
    // entities become rows, with tiers when the tier lists are present.
    name: 'ai-insert',
    steps: [
      [
        'import-with-ai',
        {
          now: day(0),
          overwriteAi: false,
          posts: [
            {
              id: '1414213562373095048',
              platform: 'instagram',
              text: 'Tizio by Artemide',
              mediaType: 'image',
              timestamp: '2024-09-27T13:35:38.519Z',
              aiDescription: 'Una lampada da scrivania nera con braccio snodato.',
              aiTags: [' Design ', 'design', 'Lighting', '', 'Mid-Century', 'lampada'],
              aiGeneralTags: ['design', 'LIGHTING', 'Mid-Century'],
              aiSpecificTags: ['mid-century', 'Lampada'],
              aiEntities: ['Artemide', 'artemide', ' Tizio ', ''],
              aiKeywords: ['lamp', 'desk lamp', 'lamp'],
              aiStatus: 'done',
              aiAnalyzedAt: 1727444138,
              aiModel: 'qwen3vl-4b',
              aiCategory: 'design',
              aiContentType: 'product',
              aiLanguage: 'it',
              aiSaveReason: 'inspiration',
            },
            {
              // Done without a time: stamped with the step's now.
              id: '1414213562373095049',
              platform: 'instagram',
              mediaType: 'image',
              timestamp: '2024-09-28T00:00:00.000Z',
              aiTags: ['poster'],
              aiStatus: 'done',
            },
            {
              // Tags without tier lists: no tiers. No status: still unanalyzed.
              id: '1790004445556667771',
              platform: 'twitter',
              mediaType: 'text',
              timestamp: '2024-09-29T00:00:00.000Z',
              aiTags: ['A', 'b', 'B '],
            },
            {
              // Tier lists without tags write nothing, but count as applied.
              id: '1790004445556667772',
              platform: 'twitter',
              mediaType: 'text',
              timestamp: '2024-09-30T00:00:00.000Z',
              aiGeneralTags: ['ignored'],
            },
            {
              id: '987654321098765435',
              platform: 'pinterest',
              mediaType: 'image',
              timestamp: '2024-10-01T00:00:00.000Z',
              aiDescription: null,
              aiTags: null,
              aiStatus: null,
              aiEntities: null,
            },
            {
              id: '987654321098765436',
              platform: 'pinterest',
              mediaType: 'image',
              timestamp: '2024-10-02T00:00:00.000Z',
              aiStatus: 'pending',
            },
            {
              // With tier lists, a tag in neither list has no tier.
              id: '1414213562373095050',
              platform: 'instagram',
              mediaType: 'image',
              timestamp: '2024-10-03T00:00:00.000Z',
              aiTags: ['Bauhaus', 'chair', 'steel'],
              aiGeneralTags: ['chair'],
              aiSpecificTags: [],
              aiKeywords: [],
              aiEntities: ['Marcel Breuer', 'MARCEL BREUER'],
            },
            {
              id: '1414213562373095051',
              platform: 'instagram',
              text: 'No AI at all',
              mediaType: 'image',
              timestamp: '2024-10-04T00:00:00.000Z',
            },
          ],
        },
      ],
    ],
  },
  {
    // AI fields on existing posts: applied only while the post is
    // unanalyzed (ai_status NULL), or with overwriteAi when the import carries
    // an analysis (tags, description, tier lists, keywords or entities).
    name: 'ai-merge',
    steps: [
      [
        'seed',
        {
          now: day(0),
          overwriteAi: false,
          posts: [
            {
              id: '1732050807568877293',
              platform: 'instagram',
              text: 'Not analyzed',
              mediaType: 'image',
              timestamp: '2025-01-01T00:00:00.000Z',
            },
            {
              id: '1732050807568877294',
              platform: 'instagram',
              text: 'Analyzed',
              mediaType: 'image',
              timestamp: '2025-01-02T00:00:00.000Z',
              aiTags: ['vintage'],
              aiDescription: 'An old radio.',
              aiStatus: 'done',
              aiAnalyzedAt: 1735776000,
              aiEntities: ['Braun'],
            },
            {
              id: '1790005556667778881',
              platform: 'twitter',
              text: 'Tags but no status',
              mediaType: 'text',
              timestamp: '2025-01-03T00:00:00.000Z',
              aiTags: ['draft'],
            },
            {
              id: '1790005556667778882',
              platform: 'twitter',
              text: 'Failed analysis',
              mediaType: 'text',
              timestamp: '2025-01-04T00:00:00.000Z',
              aiStatus: 'error',
            },
            {
              id: '987654321098765437',
              platform: 'pinterest',
              text: 'Queued',
              mediaType: 'image',
              timestamp: '2025-01-05T00:00:00.000Z',
              aiStatus: 'pending',
            },
            {
              id: '1732050807568877295',
              platform: 'instagram',
              text: 'Analyzed with tiers',
              mediaType: 'image',
              timestamp: '2025-01-06T00:00:00.000Z',
              aiTags: ['Brutalism', 'concrete'],
              aiGeneralTags: ['concrete'],
              aiSpecificTags: ['Brutalism'],
              aiStatus: 'done',
              aiAnalyzedAt: 1736121600,
            },
          ],
        },
      ],
      [
        'resync',
        {
          now: day(1),
          overwriteAi: false,
          posts: [
            {
              id: '1732050807568877293',
              platform: 'instagram',
              text: 'Not analyzed',
              mediaType: 'image',
              aiTags: ['radio', 'Radio'],
              aiDescription: 'A radio.',
              aiStatus: 'done',
            },
            {
              id: '1732050807568877294',
              platform: 'instagram',
              text: 'Analyzed',
              mediaType: 'image',
              aiTags: ['replaced?'],
              aiDescription: 'No.',
              aiStatus: 'done',
            },
            {
              // Unanalyzed: the description is added, the tags stay.
              id: '1790005556667778881',
              platform: 'twitter',
              text: 'Tags but no status',
              mediaType: 'text',
              aiDescription: 'Now described',
            },
            {
              id: '1790005556667778882',
              platform: 'twitter',
              text: 'Failed analysis',
              mediaType: 'text',
              aiTags: ['nope'],
            },
            {
              id: '987654321098765437',
              platform: 'pinterest',
              text: 'Queued',
              mediaType: 'image',
              aiTags: ['nope'],
            },
            {
              id: '1732050807568877295',
              platform: 'instagram',
              text: 'Analyzed with tiers',
              mediaType: 'image',
              aiTags: ['nope'],
            },
          ],
        },
      ],
      [
        'import-overwrite',
        {
          now: day(2),
          overwriteAi: true,
          posts: [
            {
              // Carries tags: replaces them; the description stays.
              id: '1732050807568877294',
              platform: 'instagram',
              text: 'Analyzed',
              mediaType: 'image',
              aiTags: ['vintage', 'Braun SK4'],
            },
            {
              // Status and model only: no analysis to overwrite with.
              id: '1790005556667778882',
              platform: 'twitter',
              text: 'Failed analysis',
              mediaType: 'text',
              aiStatus: 'done',
              aiModel: 'other-model',
            },
            {
              // An empty description carries nothing.
              id: '987654321098765437',
              platform: 'pinterest',
              text: 'Queued',
              mediaType: 'image',
              aiDescription: '',
            },
            {
              // An empty keyword list does carry an analysis; tiers stay.
              id: '1732050807568877295',
              platform: 'instagram',
              text: 'Analyzed with tiers',
              mediaType: 'image',
              aiKeywords: [],
            },
            {
              // Still unanalyzed, so applied: null tags clear the rows.
              id: '1790005556667778881',
              platform: 'twitter',
              text: 'Tags but no status',
              mediaType: 'text',
              aiTags: null,
              aiStatus: 'done',
            },
            {
              id: '1732050807568877293',
              platform: 'instagram',
              text: 'Not analyzed',
              mediaType: 'image',
              aiEntities: ['Vitra', 'Eames'],
            },
          ],
        },
      ],
      [
        'resync-after-import',
        {
          now: day(3),
          overwriteAi: false,
          posts: [
            {
              id: '1790005556667778881',
              platform: 'twitter',
              text: 'Tags but no status',
              mediaType: 'text',
              aiTags: ['too late'],
              aiStatus: 'done',
            },
          ],
        },
      ],
    ],
  },
  {
    // The same post twice in one batch, then the whole batch again: the
    // second run leaves the library as the first one did.
    name: 'batch-repeats',
    steps: [
      ['twice-in-one-batch', { now: day(0), overwriteAi: false, posts: REPEATED }],
      ['same-batch-again', { now: day(1), overwriteAi: false, posts: REPEATED }],
    ],
  },
  {
    // AI tags are canonicalized through accepted aliases (followed to the
    // root), proposed aliases are ignored, and tiers follow the canonical tag.
    // Lowercasing is Unicode-aware.
    name: 'aliases',
    steps: [
      [
        'aliases-then-sync',
        {
          now: day(0),
          overwriteAi: false,
          aliases: [
            {
              aliasNorm: 'mid century',
              canonicalNorm: 'mid-century',
              canonicalForm: 'Mid-Century',
              status: 'accepted',
            },
            {
              aliasNorm: 'mcm',
              canonicalNorm: 'mid-century',
              canonicalForm: 'Mid-Century',
              status: 'accepted',
            },
            {
              aliasNorm: 'lamp',
              canonicalNorm: 'lighting',
              canonicalForm: 'Lighting',
              status: 'proposed',
            },
            {
              aliasNorm: 'lampe',
              canonicalNorm: 'lamp',
              canonicalForm: 'Lamp',
              status: 'accepted',
            },
            {
              aliasNorm: 'città',
              canonicalNorm: 'city',
              canonicalForm: 'City',
              status: 'accepted',
            },
            { aliasNorm: 'a', canonicalNorm: 'b', canonicalForm: 'B', status: 'accepted' },
            { aliasNorm: 'b', canonicalNorm: 'c', canonicalForm: 'C', status: 'accepted' },
          ],
          posts: [
            {
              id: '1259921049894873164',
              platform: 'instagram',
              text: 'Aliases',
              mediaType: 'image',
              timestamp: '2025-03-01T00:00:00.000Z',
              aiTags: ['Mid Century', 'MCM', 'lamp', 'Lampe', 'Mid-Century Modern'],
              aiGeneralTags: ['mcm'],
              aiSpecificTags: ['LAMPE'],
              aiStatus: 'done',
              aiAnalyzedAt: 1740787200,
            },
            {
              id: '1259921049894873165',
              platform: 'instagram',
              text: 'Unicode',
              mediaType: 'image',
              timestamp: '2025-03-02T00:00:00.000Z',
              aiTags: ['Città', 'CITTÀ', 'ΟΔΟΣ', 'İstanbul', 'Straße', 'STRASSE', 'A', 'Ⅻ'],
              aiEntities: ['Zürich', 'ZÜRICH', 'ΣΟΦΙΑ'],
            },
          ],
        },
      ],
      [
        'merge-applies-aliases',
        {
          now: day(1),
          overwriteAi: true,
          posts: [
            {
              id: '1259921049894873164',
              platform: 'instagram',
              text: 'Aliases',
              mediaType: 'image',
              aiTags: ['mcm', 'b'],
              aiSpecificTags: ['B'],
            },
          ],
        },
      ],
    ],
  },
];

// ── Running a scenario ──────────────────────────────────────────────────────

function checkScenario(s: Scenario): void {
  const platformOf = new Map<string, string>();
  const nows = new Set<string>();
  let previous = -Infinity;
  for (const [stepId, step] of s.steps) {
    const where = `${s.name}/${stepId}`;
    if (!Number.isInteger(step.now) || step.now % 1000 !== 0 || step.now <= previous) {
      throw new Error(`${where}: now must be whole seconds, increasing`);
    }
    previous = step.now;
    nows.add(new Date(step.now).toISOString());
    for (const p of step.posts) checkPost(where, p, platformOf);
  }
  for (const [stepId, step] of s.steps) {
    for (const p of step.posts) {
      if (p.timestamp && nows.has(p.timestamp)) {
        throw new Error(`${s.name}/${stepId}: ${p.id} has a date equal to a step's now`);
      }
    }
  }
}

function checkPost(where: string, p: FixturePost, platformOf: Map<string, string>): void {
  const fail = (why: string): never => {
    throw new Error(`${where}: post ${p.id}: ${why}`);
  };
  for (const key of Object.keys(p)) if (!POST_KEYS.has(key)) fail(`unsupported key ${key}`);
  if (!/^[1-9][0-9]{0,19}$/.test(p.id)) fail('ids are canonical decimal ids');
  if (!['instagram', 'twitter', 'pinterest'].includes(p.platform)) fail('social platforms only');
  if ((platformOf.get(p.id) ?? p.platform) !== p.platform) fail('one platform per id');
  platformOf.set(p.id, p.platform);
  if (typeof p.mediaType !== 'string' || p.mediaType === '') fail('mediaType is required');
  const record = p as unknown as Record<string, unknown>;
  for (const key of STRING_KEYS) {
    const v = record[key];
    if (v !== undefined && v !== null && typeof v !== 'string') fail(`${key} is not a string`);
  }
  for (const key of ARRAY_KEYS) {
    const v = record[key];
    if (v === undefined || v === null) continue;
    if (!Array.isArray(v) || v.some((t) => typeof t !== 'string')) fail(`${key}: strings only`);
  }
  const ts = p.timestamp;
  if (ts && new Date(ts).toISOString() !== ts) fail('dates are empty or ISO 8601 in UTC');
  const at = p.aiAnalyzedAt;
  if (at != null && !(Number.isInteger(at) && at >= 946_684_800 && at < 4_102_444_800)) {
    fail('aiAnalyzedAt is in seconds');
  }
  for (const m of p.media ?? []) {
    for (const key of Object.keys(m))
      if (!MEDIA_KEYS.has(key)) fail(`unsupported media key ${key}`);
  }
}

function runScenario(s: Scenario): GoldenCase[] {
  checkScenario(s);
  const { db, sql } = openDesktopDb();
  const fallbacks = new Set<string>();
  try {
    return s.steps.map(([id, step]) => {
      fallbacks.add(new Date(step.now).toISOString());
      const result = withDesktopClock(sql, step.now, () => {
        if (step.aliases?.length) addAliases(sql, step.aliases);
        db.invalidateGlobalCaches();
        // bulkUpsert's input type has no explicit nulls for the AI fields, but
        // its code reads them (`!== undefined`), and imports send them.
        const posts = step.posts as unknown as Parameters<typeof db.bulkUpsert>[0];
        return db.bulkUpsert(posts, { overwriteAi: step.overwriteAi });
      });
      const output = {
        inserted: result.inserted,
        skipped: result.skipped,
        aiUpdated: result.aiUpdated,
        posts: postsView(sql, fallbacks),
      };
      return { id, args: [step], output };
    });
  } finally {
    db.close();
  }
}

function addAliases(sql: BetterSqlite3.Database, aliases: FixtureAlias[]): void {
  const insert = sql.prepare(
    'INSERT INTO tag_alias (alias_norm, canonical_norm, canonical_form, status) VALUES (?, ?, ?, ?)',
  );
  for (const a of aliases) insert.run(a.aliasNorm, a.canonicalNorm, a.canonicalForm, a.status);
}

// ── The view ────────────────────────────────────────────────────────────────

interface PostRow {
  id: string;
  platform: string;
  shortcode: string | null;
  post_url: string | null;
  profile_url: string | null;
  author_username: string | null;
  author_name: string | null;
  text: string | null;
  thumbnail_url: string | null;
  media_type: string | null;
  media_count: number;
  web_url: string | null;
  web_domain: string | null;
  web_final_url: string | null;
  timestamp: string | null;
  thumbnail_path: string | null;
  image_path: string | null;
  video_path: string | null;
  ai_status: string | null;
  ai_model: string | null;
  ai_description: string | null;
  ai_category: string | null;
  ai_content_type: string | null;
  ai_language: string | null;
  ai_save_reason: string | null;
  ai_analyzed_at: number | null;
  ai_tags: string | null;
  ai_entities: string | null;
  ai_keywords: string | null;
}

/** Every post of the library, by id, in the shape described at the top. */
function postsView(sql: BetterSqlite3.Database, fallbacks: Set<string>): unknown[] {
  const slides = sql.prepare(
    'SELECT media_type, source_url, local_path FROM post_media WHERE post_id = ? ORDER BY position',
  );
  const tags = sql.prepare(
    'SELECT tag_norm, tag_form, tier FROM post_tags WHERE post_id = ? ORDER BY tag_norm',
  );
  const entities = sql.prepare(
    'SELECT ent_norm, ent_form FROM post_entities WHERE post_id = ? ORDER BY ent_norm',
  );
  const rows = sql.prepare('SELECT * FROM posts ORDER BY id').all() as PostRow[];
  return rows.map((r) => {
    if (r.timestamp == null || Number.isNaN(Date.parse(r.timestamp))) {
      throw new Error(`${r.id}: stored date ${String(r.timestamp)} is not a date`);
    }
    const sortTs = Date.parse(r.timestamp);
    return {
      id: r.id,
      platform: r.platform,
      shortcode: r.shortcode,
      postUrl: r.post_url,
      profileUrl: r.profile_url,
      authorUsername: r.author_username,
      authorName: r.author_name,
      text: r.text,
      thumbnailUrl: r.thumbnail_url,
      mediaType: r.media_type,
      mediaCount: r.media_count,
      webUrl: r.web_url,
      webDomain: r.web_domain,
      webFinalUrl: r.web_final_url,
      postedAt: fallbacks.has(r.timestamp) ? null : sortTs,
      sortTs,
      cover: r.thumbnail_path ?? r.image_path ?? r.video_path,
      media: (
        slides.all(r.id) as {
          media_type: string;
          source_url: string | null;
          local_path: string | null;
        }[]
      ).map((m) => ({ type: m.media_type, url: m.source_url, local: m.local_path })),
      ai: {
        status: r.ai_status,
        model: r.ai_model,
        description: r.ai_description,
        category: r.ai_category,
        contentType: r.ai_content_type,
        language: r.ai_language,
        saveReason: r.ai_save_reason,
        analyzedAt: r.ai_analyzed_at == null ? null : r.ai_analyzed_at * 1000,
        tags: jsonArray(r.ai_tags),
        entities: jsonArray(r.ai_entities),
        keywords: jsonArray(r.ai_keywords),
      },
      tagRows: (
        tags.all(r.id) as { tag_norm: string; tag_form: string; tier: string | null }[]
      ).map((t) => ({ norm: t.tag_norm, form: t.tag_form, tier: t.tier })),
      entityRows: (entities.all(r.id) as { ent_norm: string; ent_form: string }[]).map((e) => ({
        norm: e.ent_norm,
        form: e.ent_form,
      })),
    };
  });
}

/** A stored JSON array column; NULL reads as `[]`, as the desktop's parseTags does. */
function jsonArray(raw: string | null): unknown[] {
  if (raw == null) return [];
  const v: unknown = JSON.parse(raw);
  if (!Array.isArray(v)) throw new Error(`not a JSON array: ${raw}`);
  return v;
}

// ── The sets ────────────────────────────────────────────────────────────────

const mergeSets: GoldenSet[] = SCENARIOS.map((s) => ({
  name: `merge/${s.name}`,
  source: 'electron/db.ts#bulkUpsert',
  generator: 'scripts/golden/merge.ts',
  build: () => runScenario(s),
}));

export default mergeSets;
