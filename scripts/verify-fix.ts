'use strict';
// End-to-end verification of the fix. Mirrors the PATCHED Browser.jsx startSync:
// the fetch/XHR hook is injected on dom-ready (late, missing the SSR-inline first
// page, exactly like the app), then sync turns ON and we run IG_FEED_REPLAY +
// the scroll loop. We assert every relayed shortcode — crucially the 12 that the
// old path dropped — now arrives.

import { app, BrowserWindow } from 'electron';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

app.setPath('userData', path.join(app.getPath('appData'), 'Shelfy'));
app.setPath('sessionData', path.join(app.getPath('appData'), 'Shelfy'));

const URL =
  process.argv.find((a) => a.startsWith('http')) ||
  process.env.SHELFY_IG_COLLECTION_URL ||
  'https://www.instagram.com/<your-handle>/saved/<collection-name>/<collection-id>/';
const INJECTED_SRC = fs.readFileSync(
  path.join(__dirname, '..', 'dist-electron', 'webview-injected.js'),
  'utf8',
);
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const SETUP = `
  window.__cap = window.__cap || [];
  window.__socialSavedBridge = { send: (items, hasNext) => { for (const it of (items||[])) window.__cap.push(it.shortcode || String(it.id||'')); if (hasNext === false) window.__sawEnd = true; } };
`;

// Copied verbatim from Browser.jsx IG_FEED_REPLAY (same template-literal escaping).
const IG_FEED_REPLAY = `
  (async () => {
    try {
      const seg = location.pathname.split('/').filter(Boolean);
      const si = seg.indexOf('saved');
      if (si < 0) return;
      let base = null;
      const id = seg[si + 2];
      if (id && /^[0-9]+$/.test(id)) base = '/api/v1/feed/collection/' + id + '/posts/';
      else if (seg[si + 1] === 'all-posts') base = '/api/v1/feed/saved/posts/';
      if (!base) return;
      const html = document.documentElement.innerHTML;
      const m = html.match(/"X-IG-App-ID"\\s*:\\s*"(\\d+)"/) || html.match(/"APP_ID"\\s*:\\s*"(\\d+)"/);
      const appId = (m && m[1]) || '936619743392459';
      let maxId = '';
      for (let i = 0; i < 100 && !window.__syncStop; i++) {
        let r;
        try { r = await fetch(base + '?max_id=' + encodeURIComponent(maxId), { headers: { 'X-IG-App-ID': appId }, credentials: 'include' }); }
        catch (_) { break; }
        if (!r.ok) break;
        const j = await r.json().catch(() => null);
        if (!j || j.more_available !== true || !j.next_max_id) break;
        maxId = j.next_max_id;
        await new Promise((res) => setTimeout(res, 700));
      }
    } catch (_) {}
  })()
`;

const SCROLL_IG = `
  window.__syncStop = false;
  window.__lastInterceptAt = window.__lastInterceptAt || Date.now();
  (async () => {
    const STALL_MS = 20000, MAX_STALLS = 3; let stalls = 0;
    while (!window.__syncStop) {
      const loader = document.querySelector('[data-visualcompletion="loading-state"]');
      if (loader) loader.scrollIntoView({ behavior:'instant', block:'center' });
      else { const p = document.querySelectorAll('a[href^="/p/"]'); const l = p[p.length-1]; if (l) l.scrollIntoView({ behavior:'instant', block:'start' }); else window.scrollBy(0, innerHeight*0.8); }
      await new Promise(r => setTimeout(r, 1000));
      if (Date.now() - window.__lastInterceptAt > STALL_MS) { if (++stalls >= MAX_STALLS) break; } else stalls = 0;
    }
  })()
`;

const MISSING = [
  'DZMIIWFEZ5G',
  'DZMBhdotuBU',
  'DZGjjMfP4nc',
  'DZEbaw2CH7e',
  'DYu6WwdMA0E',
  'DYsJ4_qyKxm',
  'DYh8SRCRKmC',
  'DYdk9vNAgYw',
  'DYVZr43iuhy',
  'DYKTNxRgHbK',
  'DYBDNvwmgq5',
  'DQ8s6nSE4C6',
];

let win: BrowserWindow,
  injected = false,
  done = false;
function inject(): void {
  if (injected) return;
  injected = true;
  win.webContents.executeJavaScript(SETUP + '\n' + INJECTED_SRC).catch(() => {});
}

async function run(): Promise<void> {
  if (done) return;
  done = true;
  const wc = win.webContents;
  if (/accounts\/login/.test(wc.getURL())) {
    console.error('NOT logged in');
    setTimeout(() => app.quit(), 200);
    return;
  }
  for (let i = 0; i < 40; i++) {
    if (
      (await wc.executeJavaScript(`document.querySelectorAll('a[href*="/p/"]').length`, true)) > 0
    )
      break;
    await sleep(400);
  }

  console.log('sync ON → running IG_FEED_REPLAY + scroll …');
  wc.executeJavaScript(IG_FEED_REPLAY).catch(() => {});
  wc.executeJavaScript(SCROLL_IG).catch(() => {});

  let last = -1,
    stable = 0;
  for (let i = 0; i < 90 && stable < 8; i++) {
    await sleep(1000);
    const n = await wc.executeJavaScript('new Set(window.__cap||[]).size', true);
    const end = await wc.executeJavaScript('!!window.__sawEnd', true);
    if (n === last) stable++;
    else {
      stable = 0;
      last = n;
    }
    if (i % 4 === 0) console.log(`[t+${i}s] unique relayed=${n} sawEnd=${end}`);
    if (end && stable >= 3) break;
  }
  await wc.executeJavaScript('window.__syncStop = true').catch(() => {});

  const cap = new Set<string>(await wc.executeJavaScript('window.__cap || []', true));
  const recovered = MISSING.filter((s) => cap.has(s));
  console.log('\n================ VERIFICA FIX ================');
  console.log('unique relayed (app dovrebbe salvarli):', cap.size);
  console.log('dei 12 prima persi, ora catturati       :', recovered.length, '/ 12');
  console.log(
    'ancora mancanti                          :',
    MISSING.filter((s) => !cap.has(s)).join(' ') || '(nessuno)',
  );
  console.log('=============================================\n');
  setTimeout(() => app.quit(), 300);
}

app.whenReady().then(() => {
  win = new BrowserWindow({
    width: 1400,
    height: 1000,
    webPreferences: { partition: 'persist:social', contextIsolation: true, nodeIntegration: false },
  });
  win.webContents.on('dom-ready', inject);
  win.webContents.on('did-finish-load', inject);
  win.loadURL(URL);
  let started = false;
  win.webContents.on('did-stop-loading', async () => {
    if (started) return;
    started = true;
    await sleep(2500);
    run();
  });
  setTimeout(() => {
    if (!started) {
      started = true;
      run();
    }
  }, 15000);
});
app.on('window-all-closed', () => app.quit());
