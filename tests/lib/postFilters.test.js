import { describe, it, expect } from 'vitest';
import { toApiFilters } from '../../src/lib/postFilters.js';

// toApiFilters is the single source of truth shared by usePosts (what the grid
// shows) and Gallery's select-all → bulk actions (what getPostIds resolves).
// These tests pin the mapping so the two can never diverge silently.
describe('toApiFilters', () => {
  it('maps the "all" sentinels and empty values to undefined', () => {
    const out = toApiFilters({
      platform: 'all',
      source: 'all',
      mediaType: 'all',
      downloadStatus: 'all',
      aiTagged: 'all',
      search: '',
      collectionId: undefined,
      category: undefined,
      contentType: undefined,
      tag: undefined,
      concepts: [],
      conceptMode: 'or',
      sortOrder: 'newest',
    });
    expect(out).toEqual({
      platform: undefined,
      source: undefined,
      mediaType: undefined,
      downloadStatus: undefined,
      search: undefined,
      collectionId: undefined,
      category: undefined,
      contentType: undefined,
      tag: undefined,
      aiTagged: undefined,
      concepts: undefined,
      conceptMode: 'or',
      sortOrder: 'newest',
    });
  });

  it('converts the UI "linkonly" status to the backend "missing"', () => {
    expect(toApiFilters({ downloadStatus: 'linkonly' }).downloadStatus).toBe('missing');
  });

  it('converts any other non-all downloadStatus to "downloaded"', () => {
    expect(toApiFilters({ downloadStatus: 'downloaded' }).downloadStatus).toBe('downloaded');
  });

  it('passes through concrete filter values', () => {
    const out = toApiFilters({
      platform: 'instagram',
      source: 'web',
      mediaType: 'video',
      search: 'pasta',
      collectionId: 7,
      category: 'food',
      contentType: 'recipe',
      tag: 'carbonara',
      aiTagged: 'tagged',
      concepts: ['cucina', 'ricette'],
      conceptMode: 'and',
      sortOrder: 'oldest',
    });
    expect(out).toEqual({
      platform: 'instagram',
      source: 'web',
      mediaType: 'video',
      downloadStatus: undefined,
      search: 'pasta',
      collectionId: 7,
      category: 'food',
      contentType: 'recipe',
      tag: 'carbonara',
      aiTagged: 'tagged',
      concepts: ['cucina', 'ricette'],
      conceptMode: 'and',
      sortOrder: 'oldest',
    });
  });

  it('never includes pagination keys (limit/offset are the callers concern)', () => {
    const out = toApiFilters({ platform: 'all', limit: 250, offset: 50 });
    expect('limit' in out).toBe(false);
    expect('offset' in out).toBe(false);
  });
});
