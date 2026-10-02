// Synthetic API shapes for the web app's suites.
import type { components } from '../src/api/schema';

type Schemas = components['schemas'];

export function apiObject(sha: string, ext = 'jpg', g480 = true): Schemas['MediaObject'] {
  return {
    url: `/media/${sha}.${ext}`,
    g480Url: g480 ? `/media/${sha}.g480.webp` : null,
    sha256: sha,
    mime: ext === 'mp4' ? 'video/mp4' : 'image/jpeg',
    bytes: 1000,
    width: 1080,
    height: 1350,
    durationMs: ext === 'mp4' ? 12_000 : null,
  };
}

export function apiPost(overrides: Partial<Schemas['Post']> = {}): Schemas['Post'] {
  return {
    key: 'ig_1001',
    platform: 'instagram',
    shortcode: 'C0ffee',
    postUrl: 'https://www.instagram.com/p/C0ffee/',
    profileUrl: null,
    authorUsername: 'studio.example',
    authorName: 'Studio',
    caption: 'Lampada in vetro soffiato',
    mediaType: 'image',
    mediaCount: 1,
    postedAt: Date.UTC(2026, 8, 1),
    importedAt: Date.UTC(2026, 9, 1),
    sortTs: Date.UTC(2026, 8, 1),
    cover: null,
    coverUrl: 'https://cdn.example.test/cover.jpg',
    thumbhash: null,
    archiveState: 'pending',
    aiStatus: null,
    aiDescription: null,
    aiCategory: null,
    aiContentType: null,
    aiLanguage: null,
    aiSaveReason: null,
    aiTags: [],
    aiAnalyzedAt: null,
    userNote: null,
    userTags: [],
    webUrl: null,
    webDomain: null,
    webFinalUrl: null,
    updatedAt: Date.UTC(2026, 9, 1),
    deletedAt: null,
    media: [],
    collectionIds: [],
    webCapture: null,
    ...overrides,
  };
}

export function apiSlide(overrides: Partial<Schemas['PostMedia']> = {}): Schemas['PostMedia'] {
  return {
    position: 0,
    kind: 'image',
    sourceUrl: 'https://cdn.example.test/slide.jpg',
    width: 1080,
    height: 1350,
    durationMs: null,
    label: null,
    object: null,
    videoObject: null,
    ...overrides,
  };
}
