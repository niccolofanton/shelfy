'use strict';
// One-off diagnostic: opens Instagram's saved grid in a window that REUSES the
// logged-in 'persist:social' session (copied into a temp userData), scrolls to
// force the virtualized "chunks" to mount/unmount, and snapshots the DOM so we
// can see why range/shift-select breaks. Writes raw HTML + a JSON report that
// records, for every visible /p|reel|tv/ tile: its DOM index, its on-screen
// rect, and the chain of ancestor wrappers (class/display/position) up to the
// scroller — i.e. the "chunk" containers.
//
// Run:  npx electron scripts/dump-ig.cjs
// Then navigate the window to your saved list; the dump fires automatically.

const { app, BrowserWindow } = require('electron');
const fs = require('fs');
const path = require('path');

const UD = '/tmp/ig-dump/ud';
const OUT = '/tmp/ig-dump/out';
app.setPath('userData', UD);
fs.mkdirSync(OUT, { recursive: true });

let win = null;
let dumped = false;

// Collector that runs in the PAGE (main world). Returns a structured report of
// every saved tile currently mounted, in DOM order, plus its visual rect and
// ancestor "chunk" chain — the data we need to compare DOM order vs visual order.
const COLLECT = `(() => {
  const sel = 'a[href*="/p/"], a[href*="/reel/"], a[href*="/tv/"]';
  const anchors = Array.prototype.slice.call(document.querySelectorAll(sel));
  function shortcodeOf(a){ const m=(a.getAttribute('href')||'').match(/\\/(?:p|reel|tv)\\/([^/?#]+)/); return m?m[1]:null; }
  function chainOf(a){
    const out=[]; let el=a;
    for(let i=0;i<8 && el && el!==document.body; i++){
      let cs={}; try{cs=getComputedStyle(el);}catch(_){}
      const r=el.getBoundingClientRect();
      out.push({
        tag: el.tagName.toLowerCase(),
        cls: (el.getAttribute('class')||'').slice(0,80),
        disp: cs.display||'',
        pos: cs.position||'',
        transform: (cs.transform&&cs.transform!=='none')? cs.transform.slice(0,40):'',
        w: Math.round(r.width), h: Math.round(r.height),
        top: Math.round(r.top), childTiles: el.querySelectorAll(sel).length,
      });
      el=el.parentElement;
    }
    return out;
  }
  const tiles = anchors.map((a,i)=>{
    const r=a.getBoundingClientRect();
    return { i, shortcode: shortcodeOf(a), top: Math.round(r.top), left: Math.round(r.left),
             w: Math.round(r.width), h: Math.round(r.height), chain: chainOf(a) };
  }).filter(t=>t.shortcode);
  // DOM order vs visual order check.
  const visual = tiles.slice().sort((p,q)=> (p.top-q.top) || (p.left-q.left));
  const domSeq = tiles.map(t=>t.shortcode);
  const visSeq = visual.map(t=>t.shortcode);
  let firstMismatch=-1;
  for(let i=0;i<domSeq.length;i++){ if(domSeq[i]!==visSeq[i]){ firstMismatch=i; break; } }
  // Find the nearest common scroll container (most frequent ancestor that holds many tiles).
  return JSON.stringify({
    url: location.href,
    count: tiles.length,
    domOrderEqualsVisual: firstMismatch===-1,
    firstMismatchAt: firstMismatch,
    domSeq, visSeq,
    tiles,
  });
})()`;

async function snapshot(tag) {
  const wc = win.webContents;
  const json = await wc.executeJavaScript(COLLECT, true);
  fs.writeFileSync(path.join(OUT, `report-${tag}.json`), json);
  const html = await wc.executeJavaScript(
    'document.querySelector("main") ? document.querySelector("main").outerHTML : document.body.outerHTML', true);
  fs.writeFileSync(path.join(OUT, `dom-${tag}.html`), html);
  const rep = JSON.parse(json);
  console.log(`[snap ${tag}] tiles=${rep.count} domOrder==visual? ${rep.domOrderEqualsVisual} mismatch@${rep.firstMismatchAt}`);
  return rep;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Scroll by repeatedly bringing the last mounted tile into view (same trick the
// app's sync uses), which is what drives Instagram's virtual loader.
async function scrollSteps(n, block) {
  for (let i = 0; i < n; i++) {
    await win.webContents.executeJavaScript(`(() => {
      const t = document.querySelectorAll('a[href*="/p/"], a[href*="/reel/"], a[href*="/tv/"]');
      const el = ${block === 'start' ? 't[0]' : 't[t.length-1]'};
      if (el) el.scrollIntoView({ behavior:'instant', block:'${block}' });
      else window.scrollBy(0, window.innerHeight*0.9);
    })()`, true);
    await sleep(450);
  }
}

async function runDump() {
  if (dumped) return;
  dumped = true;
  try {
    // Wait for the grid to actually have tiles.
    for (let i = 0; i < 40; i++) {
      const c = await win.webContents.executeJavaScript(
        'document.querySelectorAll(\'a[href*="/p/"], a[href*="/reel/"], a[href*="/tv/"]\').length', true);
      if (c > 0) break;
      await sleep(400);
    }
    await snapshot('A-initial');           // top of the list, fresh
    await scrollSteps(18, 'end');          // scroll deep down → mount later chunks, unmount early ones
    await snapshot('B-scrolled-down');     // bottom-ish
    await scrollSteps(10, 'start');        // scroll back UP toward the first tile
    await snapshot('C-scrolled-back-up');  // the exact pattern the user reports as broken
    console.log('DONE. Files in', OUT);
  } catch (e) {
    console.error('dump error:', e);
  } finally {
    setTimeout(() => app.quit(), 500);
  }
}

app.whenReady().then(() => {
  win = new BrowserWindow({
    width: 1440, height: 1000,
    webPreferences: { partition: 'persist:social', contextIsolation: true, nodeIntegration: false },
  });
  win.loadURL('https://www.instagram.com/');
  win.webContents.on('did-stop-loading', async () => {
    const url = win.webContents.getURL();
    console.log('loaded:', url);
    if (/instagram\.com\/(?:[^/?#]+\/)?saved/.test(url) && !dumped) {
      await sleep(1500);
      runDump();
    }
  });
});

app.on('window-all-closed', () => app.quit());
