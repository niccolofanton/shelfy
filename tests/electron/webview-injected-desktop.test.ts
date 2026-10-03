// The desktop's own build of the capture hook (electron/webview-injected.ts), web port P2-05.
// build/esbuild-electron.ts transpiles electron/ file by file to CommonJS, and the desktop
// injects dist-electron/webview-injected.js into the webview page with executeJavaScript, where
// there is no `module`, `exports` or `require`. This suite builds the hook the same way, runs
// it in a bare page global, and checks that the desktop still gets what it got before the
// video URLs: same globals, and, after its sanitizer, the same items. Node environment, because
// esbuild refuses jsdom's Uint8Array. Synthetic payloads only.

import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runInNewContext } from 'node:vm';
import * as esbuild from 'esbuild';
import { beforeAll, describe, expect, it } from 'vitest';
import { sanitizeInterceptedBatch } from '../../src/lib/browserSanitize';

const HOOK = join(
  dirname(fileURLToPath(import.meta.url)),
  '..',
  '..',
  'electron',
  'webview-injected.ts',
);

type Relay = [items: unknown[], hasNextPage: boolean | null, platform: string];
type Page = Record<string, unknown> & { fetch: (url: string) => Promise<Response> };

let code = '';
const relayed: Relay[] = [];
let served: unknown = {};
let page: Page;
const network = async (): Promise<Response> => new Response(JSON.stringify(served));

/** A JSON round trip: compares values made in the page's realm with values made here. */
const plain = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;

beforeAll(async () => {
  // The options of build/esbuild-electron.ts that shape the output (it writes to disk).
  const result = await esbuild.build({
    entryPoints: [HOOK],
    bundle: false,
    platform: 'node',
    format: 'cjs',
    target: 'node22',
    write: false,
    logLevel: 'silent',
  });
  code = result.outputFiles[0].text;

  // The webview's MAIN world: a page global, the preload's bridge, the page's own network.
  page = {
    __socialSavedBridge: { send: (...args: unknown[]) => void relayed.push(args as Relay) },
    fetch: network,
    XMLHttpRequest: function XMLHttpRequest() {},
    location: { origin: 'https://www.instagram.com', pathname: '/' },
  };
  page.window = page;
  runInNewContext(code, page);
});

describe('the desktop build of the hook', () => {
  it('is a plain script: no CommonJS module scaffolding', () => {
    expect(code).toContain('SOCIAL_SAVED_INTERCEPT');
    expect(code).not.toMatch(/\bmodule\.exports\b|\bexports\.|\brequire\s*\(/);
  });

  it('installs in a bare page global, with the same globals plus the extension entry', () => {
    expect(page.__socialSavedInjected).toBe(true);
    for (const name of ['__ssReplayPinterest', '__ssScanTwitterBookmarks', '__ssEmitInstagramRest'])
      expect(typeof page[name]).toBe('function');
    // The hook wrapped the page's fetch.
    expect(page.fetch).not.toBe(network);
  });

  it('after the desktop sanitizer, a video post is what the desktop stored before', async () => {
    const cdn = 'https://scontent-synth1-1.cdninstagram.com';
    const poster = `${cdn}/v/t51.2885-15/reel.jpg?oe=68F00000`;
    const video = `${cdn}/o1/v/t16/f2/m69/reel.mp4?oe=68F00000`;
    served = {
      items: [
        {
          media: {
            id: '3400000000000000002_9000000002',
            code: 'C8vOfxsVAAC',
            media_type: 2,
            taken_at: 1758100000,
            user: { username: 'synthetic_author' },
            caption: { text: 'Synthetic caption' },
            image_versions2: { candidates: [{ url: poster }] },
            video_versions: [{ width: 720, height: 1280, url: video }],
          },
        },
      ],
      more_available: false,
    };
    await page.fetch('https://www.instagram.com/api/v1/feed/saved/posts/?max_id=');
    expect(relayed).toHaveLength(1);
    const [items, hasNextPage, platform] = relayed[0];
    expect([hasNextPage, platform]).toEqual([false, 'instagram']);
    expect(plain(items)).toMatchObject([
      { media: [{ type: 'video', url: poster, videoUrl: video }] },
    ]);

    const stored = plain(sanitizeInterceptedBatch(items, platform));
    expect(stored).toEqual([
      {
        id: '3400000000000000002_9000000002',
        platform: 'instagram',
        shortcode: 'C8vOfxsVAAC',
        postUrl: 'https://www.instagram.com/p/C8vOfxsVAAC/',
        profileUrl: 'https://www.instagram.com/synthetic_author/',
        authorUsername: 'synthetic_author',
        authorName: '',
        mediaType: 'video',
        timestamp: '2025-09-17T09:06:40.000Z',
        text: 'Synthetic caption',
        thumbnailUrl: poster,
        media: [{ type: 'video', url: poster }],
      },
    ]);
  });
});
