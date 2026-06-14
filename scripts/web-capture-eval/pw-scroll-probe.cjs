'use strict';

// Deep-probe a site's scroll mechanism. For scroll-DRIVEN WebGL experiences
// (lusion.co et al.) documentElement.scrollHeight stays ≈ viewport because the
// page is a fixed canvas and scrolling fires wheel/scroll listeners that advance
// an animation timeline — there is NO tall static DOM to tile. This probe figures
// out which mechanism a page uses and whether dispatching wheel/scroll actually
// moves a virtual progress, so we can decide how to capture the journey.
//
// Run: electron scripts/web-capture-eval/pw-scroll-probe.cjs [url]

const { app } = require('electron');
const { chromium } = require('playwright-core');

const TARGET = process.argv.find((a) => /^https?:\/\//.test(a)) || 'https://lusion.co/';
const SLEEP = (ms) => new Promise((r) => setTimeout(r, ms));

const INSPECT = `(() => {
  const d = document.documentElement, b = document.body;
  const cs = getComputedStyle.bind(window);
  const libs = [];
  if (window.ScrollSmoother) libs.push('ScrollSmoother');
  if (window.__lenis || d.classList.contains('lenis') || window.Lenis) libs.push('Lenis');
  if (window.LocomotiveScroll || document.querySelector('[data-scroll-container]')) libs.push('Locomotive');
  if (window.locomotive) libs.push('locomotive-inst');
  if (window.gsap) libs.push('gsap');
  if (window.virtualScroll || window.VirtualScroll || window.__vs) libs.push('virtual-scroll');
  if (window.THREE) libs.push('THREE');
  if (window.OGL || window.ogl) libs.push('OGL');
  // largest canvas
  const canv = [...document.querySelectorAll('canvas')].map(c => {
    const r = c.getBoundingClientRect(); const s = cs(c);
    return { w: Math.round(r.width), h: Math.round(r.height), pos: s.position, z: s.zIndex };
  });
  // scrollable containers (overflow auto/scroll with content taller than client)
  const scrollers = [];
  for (const el of document.querySelectorAll('*')) {
    const s = cs(el);
    if (!/auto|scroll/.test(s.overflowY)) continue;
    if (el.scrollHeight > el.clientHeight + 50) {
      scrollers.push({ tag: el.tagName, id: el.id || '', cls: (el.className||'').toString().slice(0,30), sh: el.scrollHeight, ch: el.clientHeight });
    }
  }
  return {
    libs,
    docScrollHeight: d.scrollHeight, docClientHeight: d.clientHeight,
    bodyScrollHeight: b ? b.scrollHeight : 0,
    htmlOverflow: cs(d).overflowY, bodyOverflow: b ? cs(b).overflowY : '',
    bodyPosition: b ? cs(b).position : '',
    bodyHeight: b ? cs(b).height : '',
    innerH: window.innerHeight,
    maxScrollY: Math.max(0, d.scrollHeight - window.innerHeight),
    canvases: canv,
    scrollers: scrollers.slice(0, 8),
    htmlClass: (d.className||'').slice(0,80),
    bodyClass: (b && b.className||'').toString().slice(0,80),
  };
})()`;

// After dispatching wheel, what moved? Capture every signal that could represent
// "scroll progress" without the document height changing.
const PROGRESS = `(() => {
  const d = document.documentElement, b = document.body;
  const out = {
    scrollY: window.scrollY, pageYOffset: window.pageYOffset,
    docScrollTop: d.scrollTop, bodyScrollTop: b ? b.scrollTop : 0,
  };
  // transform translateY on the main wrapper(s)
  const wraps = [];
  for (const sel of ['#smooth-content','[data-scroll-container]','[data-smooth-content]','main','#app','#__next','.smooth-content']) {
    const el = document.querySelector(sel);
    if (el) { const t = getComputedStyle(el).transform; if (t && t !== 'none') wraps.push(sel + '=' + t.slice(0,40)); }
  }
  out.wraps = wraps;
  // lenis exposes scroll on instance
  try { if (window.__lenis) out.lenis = window.__lenis.scroll; } catch(e){}
  return out;
})()`;

async function run() {
  const browser = await chromium.launch({
    headless: true,
    args: ['--ignore-gpu-blocklist', '--use-gl=angle', '--use-angle=metal', '--no-sandbox'],
  });
  const ctx = await browser.newContext({
    viewport: { width: 1280, height: 900 },
    deviceScaleFactor: 1,
  });
  const pg = await ctx.newPage();
  await pg.goto(TARGET, { waitUntil: 'load', timeout: 45000 });
  await SLEEP(8000); // let intro finish

  console.log('=== INITIAL INSPECT ===');
  console.log(JSON.stringify(await pg.evaluate(INSPECT), null, 2));

  console.log('\n=== PROGRESS before scroll ===');
  console.log(JSON.stringify(await pg.evaluate(PROGRESS)));

  // Try window.scrollTo
  await pg.evaluate('window.scrollTo(0, 2000)');
  await SLEEP(1200);
  console.log('\n=== after window.scrollTo(0,2000) ===');
  console.log(JSON.stringify(await pg.evaluate(PROGRESS)));

  // Try dispatching wheel events on the page (real scroll-jack input)
  await pg.evaluate('window.scrollTo(0,0)');
  await SLEEP(500);
  for (let i = 0; i < 8; i++) {
    await pg.mouse.move(640, 450);
    await pg.mouse.wheel(0, 600);
    await SLEEP(400);
  }
  await SLEEP(1500);
  console.log('\n=== after 8× mouse.wheel(600) ===');
  console.log(JSON.stringify(await pg.evaluate(PROGRESS)));
  console.log('inspect after wheel:');
  const after = await pg.evaluate(INSPECT);
  console.log(
    JSON.stringify(
      {
        docScrollHeight: after.docScrollHeight,
        maxScrollY: after.maxScrollY,
        canvases: after.canvases,
      },
      null,
      2,
    ),
  );

  // keep wheeling a lot to find the end, logging body text near viewport
  let prevSig = '';
  for (let burst = 0; burst < 30; burst++) {
    for (let i = 0; i < 4; i++) {
      await pg.mouse.wheel(0, 800);
      await SLEEP(250);
    }
    await SLEEP(400);
    const sig = await pg
      .evaluate(
        `(() => {
      // sample visible text + first visible heading to detect section changes
      const vis = [];
      for (const el of document.querySelectorAll('h1,h2,h3,p,a,span')) {
        const r = el.getBoundingClientRect();
        if (r.top >= 0 && r.top < window.innerHeight && r.width > 0 && r.height > 0) {
          const t = (el.innerText||'').trim();
          if (t && t.length > 2 && t.length < 80) { vis.push(t); if (vis.length >= 3) break; }
        }
      }
      return vis.join(' | ');
    })()`,
      )
      .catch(() => '');
    if (sig !== prevSig) {
      console.log(`burst ${burst}: ${sig}`);
      prevSig = sig;
    }
  }
  console.log('\n=== FINAL PROGRESS ===');
  console.log(JSON.stringify(await pg.evaluate(PROGRESS)));

  await browser.close();
}

app.whenReady().then(async () => {
  try {
    await run();
  } catch (e) {
    console.error('FAILED', e);
  } finally {
    app.exit(0);
  }
});
