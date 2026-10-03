// Passive capture in the worker (sw/capture.ts): the checks in order, the discard reasons, one
// passive run per listing visit with the C4 listing and folder mapping, and the pre-filter.

import { describe, expect, it } from 'vitest';
import { MSG, type CaptureMessage, type Platform } from '../src/shared/protocol';
import { handleCapture, type CaptureSender } from '../src/sw/capture';
import { T0, harness, igItem, type Harness } from './helpers';

const IG_FOLDER = 'https://www.instagram.com/someone/saved/recipes/17890000000000001/';
const PIN_BOARD = 'https://www.pinterest.com/someone/recipes/';

function message(
  platform: Platform,
  pageUrl: string,
  items: Record<string, unknown>[],
  extra: Partial<CaptureMessage> = {},
): CaptureMessage {
  return {
    kind: MSG.capture,
    platform,
    items,
    hasNextPage: true,
    pageUrl,
    docId: 'doc-1',
    seq: 0,
    capture: 'passive',
    viewer: null,
    sentAt: T0,
    ...extra,
  };
}

const senderFor = (url: string, tabId = 4): CaptureSender => ({
  tabId,
  frameId: 0,
  url,
  tabUrl: url,
});

async function run(h: Harness, m: CaptureMessage, sender = senderFor(m.pageUrl)) {
  return handleCapture(m, sender, {
    queue: h.queue,
    store: h.store,
    config: h.config,
    now: () => h.clock.now,
  });
}

const pin = (n: number) => ({
  id: `90000000000000000${n}`,
  postUrl: `https://www.pinterest.com/pin/90000000000000000${n}/`,
  text: `Synthetic pin ${n}`,
  thumbnailUrl: `https://i.pinimg.com/236x/${n}.jpg`,
  mediaType: 'image',
  media: [{ type: 'image', url: `https://i.pinimg.com/originals/${n}.jpg` }],
});

describe('handleCapture', () => {
  it('queues an IG folder batch into a passive run filed under the folder', async () => {
    const h = harness();
    await h.pairNow();
    const outcome = await run(h, message('instagram', IG_FOLDER, [igItem(1), igItem(2)]));
    expect(outcome).toMatchObject({ ok: true, queued: 2, duplicate: false });
    const [r] = await h.queue.runs();
    expect(r).toMatchObject({
      platform: 'instagram',
      trigger: 'passive',
      listing: { kind: 'ig_collection', externalId: '17890000000000001', name: 'Recipes' },
      collection: { mode: 'auto' },
      tabId: 4,
      docId: 'doc-1',
      listingKey: 'instagram:ig_collection:17890000000000001',
    });
  });

  it('files nothing into collections when folder mapping is off, or for all-posts', async () => {
    const h = harness();
    await h.pairNow();
    await h.store.patchSettings({ passiveFolders: false });
    await run(h, message('instagram', IG_FOLDER, [igItem(1)]));
    await run(
      h,
      message('instagram', 'https://www.instagram.com/someone/saved/all-posts/', [igItem(2)], {
        seq: 1,
      }),
      senderFor(IG_FOLDER, 5),
    );
    expect((await h.queue.runs()).map((r) => r.collection)).toEqual([
      { mode: 'none' },
      { mode: 'none' },
    ]);
  });

  it('keeps captions, authors and direct video URLs (P2-G15, P2-05)', async () => {
    const h = harness();
    await h.pairNow();
    await run(
      h,
      message('instagram', IG_FOLDER, [
        igItem(1, {
          mediaType: 'video',
          media: [
            {
              type: 'video',
              url: 'https://scontent.cdninstagram.com/v/poster.jpg',
              videoUrl: 'https://scontent.cdninstagram.com/v/clip.mp4',
            },
          ],
        }),
      ]),
    );
    h.clock.now += 2_000;
    await h.queue.sealDue(h.clock.now);
    const batch = await h.queue.nextBatch();
    expect(batch?.items[0]).toMatchObject({
      text: 'Synthetic caption 1',
      authorUsername: 'synthetic_author',
      media: [
        {
          type: 'video',
          url: 'https://scontent.cdninstagram.com/v/poster.jpg',
          videoUrl: 'https://scontent.cdninstagram.com/v/clip.mp4',
        },
      ],
    });
    expect(batch?.items[0]).not.toHaveProperty('platform');
  });

  it.each([
    ['unpaired', async () => undefined, IG_FOLDER, 'instagram' as Platform],
    [
      'outdated',
      async (h: Harness) => void (await h.store.patchStatus({ outdated: true })),
      IG_FOLDER,
      'instagram' as Platform,
    ],
    [
      'disabled',
      async (h: Harness) => void (await h.store.patchSettings({ passive: { instagram: false } })),
      IG_FOLDER,
      'instagram' as Platform,
    ],
    [
      'out_of_scope',
      async () => undefined,
      'https://www.instagram.com/explore/',
      'instagram' as Platform,
    ],
  ])('discards when %s', async (reason, prepare, pageUrl, platform) => {
    const h = harness();
    if (reason !== 'unpaired') await h.pairNow();
    await prepare(h);
    expect(await run(h, message(platform, pageUrl, [igItem(1), igItem(2)]))).toEqual({
      ok: true,
      queued: 0,
      discarded: reason,
      items: 2,
    });
    const { counters, runs } = await h.queue.snapshot();
    expect(counters.discarded[reason as keyof typeof counters.discarded]).toBe(2);
    expect(counters.queuedItems).toBe(0);
    expect(runs).toEqual([]);
  });

  it('applies the server kill switch from the config (within one refresh)', async () => {
    const h = harness();
    await h.pairNow();
    expect((await run(h, message('instagram', IG_FOLDER, [igItem(1)]))).queued).toBe(1);
    h.api.killed.add('instagram');
    h.api.bumpConfig();
    await h.config.refresh(true);
    expect(await run(h, message('instagram', IG_FOLDER, [igItem(2)], { seq: 1 }))).toMatchObject({
      discarded: 'killed',
    });
    // X is not killed.
    expect(
      (
        await run(
          h,
          message('twitter', 'https://x.com/i/bookmarks', [{ id: '1800000000000000001' }], {
            seq: 2,
          }),
        )
      ).queued,
    ).toBe(1);
  });

  it("Pinterest: the user's own board only", async () => {
    const h = harness();
    await h.pairNow();
    expect(
      await run(h, message('pinterest', PIN_BOARD, [pin(1)], { viewer: 'someone' })),
    ).toMatchObject({
      queued: 1,
    });
    expect(
      await run(h, message('pinterest', PIN_BOARD, [pin(2)], { viewer: 'someone_else', seq: 1 })),
    ).toMatchObject({ discarded: 'not_own_board' });
    expect(
      await run(h, message('pinterest', PIN_BOARD, [pin(3)], { viewer: null, seq: 2 })),
    ).toMatchObject({
      discarded: 'viewer_unknown',
    });
  });

  it('refuses a batch whose sender is not the top frame of a matching tab', async () => {
    const h = harness();
    await h.pairNow();
    expect(
      await run(h, message('instagram', IG_FOLDER, [igItem(1)]), {
        tabId: 4,
        frameId: 3,
        url: IG_FOLDER,
        tabUrl: IG_FOLDER,
      }),
    ).toMatchObject({ discarded: 'sender' });
    expect(
      await run(
        h,
        message('instagram', IG_FOLDER, [igItem(1)]),
        senderFor('https://x.com/i/bookmarks'),
      ),
    ).toMatchObject({ discarded: 'sender' });
  });

  it('counts items the pre-filter refuses, and ignores a repeated delivery', async () => {
    const h = harness();
    await h.pairNow();
    expect(
      await run(h, message('instagram', IG_FOLDER, [igItem(1), { id: '' }, { noId: true }])),
    ).toMatchObject({ queued: 1 });
    expect(await run(h, message('instagram', IG_FOLDER, [{ id: '' }], { seq: 1 }))).toEqual({
      ok: true,
      queued: 0,
      discarded: 'invalid',
      items: 1,
    });
    expect(await run(h, message('instagram', IG_FOLDER, [igItem(1)]))).toMatchObject({
      queued: 0,
      duplicate: true,
    });
    const { counters } = await h.queue.snapshot();
    expect(counters.discarded.invalid).toBe(3);
    expect(counters.queuedItems).toBe(1);
  });
});
