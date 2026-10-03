// In-page capture scripts exercised in a real headless Chromium on synthetic
// pages. Optional locally when Chromium is absent; required in CI.
import { describe, it, expect, beforeAll, afterAll, afterEach } from 'vitest';
import fs from 'fs';
import { chromium, type Browser, type BrowserContext, type Page } from 'playwright-core';
import {
  INIT_SCRIPT,
  JS_DETECT_BLOCKED,
  JS_PAGE_PROBE,
  JS_REVEAL,
  JS_REMOVE_OVERLAYS,
  JS_LOADER_VISIBLE,
} from '../../electron/webcap/scripts';

let browser: Browser;
let context: BrowserContext | undefined;
const available = fs.existsSync(chromium.executablePath());

async function open(html: string): Promise<Page> {
  context ??= await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  await page.addInitScript({ content: INIT_SCRIPT });
  // Served from a real http(s) origin: init scripts run on navigation (not on
  // setContent) and relative links resolve like on a live site.
  await page.route('https://site.test/**', (route) =>
    route.fulfill({ status: 200, contentType: 'text/html', body: html }),
  );
  await page.goto('https://site.test/', { waitUntil: 'load' });
  return page;
}

describe.skipIf(!available && !process.env.CI)('in-page capture scripts', () => {
  beforeAll(async () => {
    // An installed browser failing to launch is a test failure. Do not turn
    // a hook error into five passing tests that exercised no scripts.
    browser = await chromium.launch({ headless: true });
    // Launching Chromium can take well over the default 10 s on a loaded machine.
  }, 60_000);
  afterAll(async () => {
    await browser?.close();
  }, 30_000);
  afterEach(async () => {
    const finished = context;
    context = undefined;
    await finished?.close();
  });

  it('recognises a Cloudflare interstitial but not a normal page with a captcha form', async () => {
    const cf = await open(
      '<html><head><title>Just a moment...</title><script src="/cdn-cgi/challenge-platform/h/b/orchestrate"></script></head><body><h1>example.com</h1><p>Performing security verification</p></body></html>',
    );
    expect(await cf.evaluate(JS_DETECT_BLOCKED)).toMatchObject({
      blocked: true,
      vendor: 'cloudflare',
    });
    const normal = await open(
      `<html><head><title>Shop</title></head><body>${'<a href="/p">Product</a>'.repeat(30)}<p>${'Lorem ipsum '.repeat(300)}</p><div class="cf-turnstile"></div></body></html>`,
    );
    expect(await normal.evaluate(JS_DETECT_BLOCKED)).toMatchObject({ blocked: false });
  });

  it('measures typography from visible text only and segments sections', async () => {
    const page = await open(`
      <html lang="en"><head><title>Home | Acme</title>
        <meta name="description" content="Acme builds things">
        <meta property="og:site_name" content="Acme">
      </head><body style="margin:0;background:#fafafa;color:#111">
        <header style="height:80px"><nav><a href="/work">Work</a><a href="/about">About</a></nav></header>
        <main>
          <section style="height:900px;background:#0b0b0b;color:#fff"><h1 style="font:700 96px Georgia">Big headline</h1>
            <a href="/start" style="display:inline-block;padding:14px 24px;background:#ff5a1f;color:#fff">Get started</a></section>
          <section style="height:700px"><h2>Features</h2><h3>One</h3><h3>Two</h3><h3>Three</h3><p style="font:16px Arial">${'Readable body copy. '.repeat(80)}</p></section>
          <section style="height:600px"><h2>Pricing</h2><p>Pro €29 / month</p><p>Team €99 / month</p></section>
          <p style="display:none;font:40px Impact">HIDDEN TEXT SHOULD NOT COUNT</p>
        </main>
        <footer style="height:300px"><a href="https://www.instagram.com/acme">Instagram</a></footer>
      </body></html>`);
    const probe = (await page.evaluate(JS_PAGE_PROBE)) as {
      head: { title: string; description: string; ogSiteName: string; lang: string };
      typeStyles: { family: string; size: number }[];
      ctas: { text: string; bg: string }[];
      sections: { kind: string }[];
      social: { platform: string }[];
      links: { href: string; region: string }[];
    };
    expect(probe.head).toMatchObject({
      title: 'Home | Acme',
      description: 'Acme builds things',
      ogSiteName: 'Acme',
      lang: 'en',
    });
    expect(probe.typeStyles.some((s) => /Impact/.test(s.family))).toBe(false);
    expect(probe.typeStyles.some((s) => /Georgia/.test(s.family) && s.size === 96)).toBe(true);
    expect(probe.ctas.find((c) => c.text === 'Get started')?.bg).toBe('#ff5a1f');
    const kinds = probe.sections.map((s) => s.kind);
    expect(kinds).toContain('hero');
    expect(kinds).toContain('features');
    expect(kinds).toContain('pricing');
    expect(kinds).toContain('footer');
    expect(probe.social).toEqual([
      { platform: 'instagram', href: 'https://www.instagram.com/acme' },
    ]);
    expect(probe.links.find((l) => l.href.endsWith('/work'))?.region).toBe('nav');
  });

  it('reveals IntersectionObserver-driven content without scrolling', async () => {
    const page = await open(`
      <body><div style="height:3000px"></div><div id="r" style="opacity:0">revealed</div>
      <script>
        const io = new IntersectionObserver((es) => es.forEach((e) => { if (e.isIntersecting) e.target.style.opacity = 1; }));
        io.observe(document.getElementById('r'));
      </script></body>`);
    expect(await page.evaluate('getComputedStyle(document.getElementById("r")).opacity')).toBe('0');
    const res = (await page.evaluate(JS_REVEAL)) as { io: number };
    expect(res.io).toBeGreaterThanOrEqual(1);
    expect(await page.evaluate('getComputedStyle(document.getElementById("r")).opacity')).toBe('1');
  });

  it('removes newsletter modals but keeps the header', async () => {
    const page = await open(`
      <body><header id="h" style="position:fixed;top:0;left:0;right:0;height:70px"><a href="/">Logo</a></header>
      <div id="m" role="dialog" aria-modal="true" style="position:fixed;inset:20%;background:#fff">Subscribe to our newsletter</div>
      <div style="height:2000px"></div></body>`);
    expect(await page.evaluate(JS_REMOVE_OVERLAYS)).toBeGreaterThanOrEqual(1);
    expect(await page.evaluate('getComputedStyle(document.getElementById("m")).display')).toBe(
      'none',
    );
    expect(await page.evaluate('getComputedStyle(document.getElementById("h")).display')).not.toBe(
      'none',
    );
  });

  it('treats an off-screen curtain loader as gone', async () => {
    const page = await open(`
      <body><div id="l" class="preloader" style="position:fixed;inset:0;background:#000"></div><p>content</p></body>`);
    expect(await page.evaluate(JS_LOADER_VISIBLE)).toBe(true);
    await page.evaluate('document.getElementById("l").style.transform = "translateY(-100%)"');
    expect(await page.evaluate(JS_LOADER_VISIBLE)).toBe(false);
  });
});
