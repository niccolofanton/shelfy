import { beforeAll, describe, expect, it } from 'vitest';
import { build } from 'esbuild';
import { createRequire } from 'node:module';
const { JSDOM } = createRequire(import.meta.url)('jsdom') as {
  JSDOM: new (
    html: string,
    options: { url: string; runScripts: string; pretendToBeVisual: boolean },
  ) => { window: Window & { eval(code: string): unknown; close(): void } };
};
import type {} from '../../electron/webview-select';
let code: string;
beforeAll(async () => {
  code = (
    await build({
      entryPoints: ['electron/webview-select.ts'],
      bundle: false,
      platform: 'node',
      format: 'cjs',
      target: 'node22',
      write: false,
    })
  ).outputFiles[0].text;
});

describe('desktop selection contract', () => {
  it('is a plain script and keeps Italian labels and the existing host relay by default', () => {
    expect(code).not.toMatch(/\bmodule\.exports\b|\bexports\.|\brequire\s*\(/);
    const dom = new JSDOM('<main><a href="/p/ABC/">tile</a></main>', {
      url: 'https://www.instagram.com/me/saved/all-posts/',
      runScripts: 'outside-only',
      pretendToBeVisual: true,
    });
    const messages: unknown[] = [];
    const window = dom.window as unknown as Window;
    window.__socialSavedBridge = {
      sendSelect: (message: unknown) => {
        messages.push(message);
      },
    } as typeof window.__socialSavedBridge;
    dom.window.eval(code);
    window.__ssSelect!.enable();
    const box = dom.window.document.querySelector('[data-ss-check]') as HTMLElement;
    box.click();
    expect(JSON.parse(window.__ssSelect!.collectJSON())).toMatchObject([
      { id: 'ABC', shortcode: 'ABC' },
    ]);
    window.__ssSelect!.markSaved([{ key: 'ABC', id: 'desktop-id' }]);
    const badge = dom.window.document.querySelector('[data-ss-open]') as HTMLElement;
    expect(badge.textContent).toBe('Già in database');
    expect(badge.title).toBe('Premi per vedere il post');
    expect(box.title).toBe('Già presente nel database');
    expect(window.__ssSelect!.status().count).toBe(0);
    badge.click();
    expect(messages).toContainEqual({ type: 'open', id: 'desktop-id', platform: 'instagram' });
    window.__ssSelect!.disable();
    dom.window.close();
  });

  it('resets saved badges and selections when the extension switches library binding', () => {
    const dom = new JSDOM('<a href="/p/ABC/">tile</a><a href="/p/DEF/">other</a>', {
      url: 'https://www.instagram.com/me/saved/all-posts/',
      runScripts: 'outside-only',
      pretendToBeVisual: true,
    });
    const window = dom.window as unknown as Window;
    dom.window.eval(code);
    window.__ssSelect!.bindLibrary('account-a');
    window.__ssSelect!.enable();
    window.__ssSelect!.markSaved([{ key: 'ABC', id: 'ig_old' }]);
    (dom.window.document.querySelectorAll('[data-ss-check]')[1] as HTMLElement).click();
    expect(window.__ssSelect!.status().count).toBe(1);
    window.__ssSelect!.bindLibrary('account-b');
    expect(window.__ssSelect!.status()).toEqual({ enabled: true, count: 0 });
    expect((dom.window.document.querySelector('[data-ss-open]') as HTMLElement).style.display).toBe(
      'none',
    );
    expect(
      dom.window.document.querySelector('[data-ss-open]')!.getAttribute('data-ss-id'),
    ).toBeNull();
    (dom.window.document.querySelector('[data-ss-check]') as HTMLElement).click();
    expect(window.__ssSelect!.status().count).toBe(1);
    window.__ssSelect!.disable();
    dom.window.close();
  });

  it('injects labels as text and updates existing saved cards', () => {
    const dom = new JSDOM('<a href="/p/ABC/">tile</a>', {
      url: 'https://www.instagram.com/me/saved/all-posts/',
      runScripts: 'outside-only',
      pretendToBeVisual: true,
    });
    const window = dom.window as unknown as Window;
    dom.window.eval(code);
    window.__ssSelect!.enable();
    window.__ssSelect!.markSaved([{ key: 'ABC', id: 'ig_123' }]);
    window.__ssSelect!.setLabels({
      saved: '<img onerror=bad()>',
      open: 'Open in Shelfy',
      disabled: 'Already saved',
    });
    expect(dom.window.document.querySelector('[data-ss-open]')!.textContent).toBe(
      '<img onerror=bad()>',
    );
    expect(dom.window.document.querySelector('[data-ss-open] img')).toBeNull();
    window.__ssSelect!.disable();
    dom.window.close();
  });
});
