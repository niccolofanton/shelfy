import { describe, it, expect } from 'vitest';
import { buildSyncSteps } from '../../src/hooks/useSourceSync';

// Native folders carry the platform's own folder/board id (externalId); custom
// folders have platform null and must never become sync steps.
const collections = [
  { id: 1, name: 'Motivational', platform: 'instagram', externalId: '17912345' },
  { id: 2, name: 'Ricette', platform: 'instagram', externalId: '17999999' },
  { id: 3, name: 'Custom', platform: null, externalId: null },
  { id: 4, name: 'Travel board', platform: 'pinterest', externalId: 'mario/travel' },
];

describe('buildSyncSteps', () => {
  it('plans Instagram platform sync as all-posts followed by every native folder', () => {
    const steps = buildSyncSteps({ type: 'platform', platform: 'instagram' }, collections);
    expect(steps.map((s) => s.type)).toEqual(['ig-all', 'ig-folder', 'ig-folder']);
    expect(steps[1].collection.id).toBe(1);
    expect(steps[2].collection.id).toBe(2);
  });

  it('plans Twitter as a single bookmarks step', () => {
    const steps = buildSyncSteps({ type: 'platform', platform: 'twitter' }, collections);
    expect(steps).toEqual([{ type: 'tw-bookmarks' }]);
  });

  it('plans Pinterest platform sync as one step per board, none when no boards exist', () => {
    const steps = buildSyncSteps({ type: 'platform', platform: 'pinterest' }, collections);
    expect(steps).toEqual([{ type: 'pin-board', collection: collections[3] }]);
    expect(buildSyncSteps({ type: 'platform', platform: 'pinterest' }, [])).toEqual([]);
  });

  it('plans a single native folder as one targeted step', () => {
    const steps = buildSyncSteps(
      { type: 'collection', platform: 'instagram', collectionId: 2 },
      collections,
    );
    expect(steps).toEqual([{ type: 'ig-folder', collection: collections[1] }]);
  });

  it('refuses custom folders and unknown targets', () => {
    expect(
      buildSyncSteps({ type: 'collection', platform: null, collectionId: 3 }, collections),
    ).toEqual([]);
    expect(buildSyncSteps({ type: 'platform', platform: 'web' }, collections)).toEqual([]);
    expect(buildSyncSteps(null, collections)).toEqual([]);
  });
});
