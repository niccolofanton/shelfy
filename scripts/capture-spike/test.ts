// Self-contained validation of the ONE assumption behind capture-on-view:
// CDP Network.getResponseBody returns the INTACT bytes of a cross-origin <img>,
// exactly where the renderer <canvas> path fails with a CORS SecurityError.
//
// No internet, no login: an image is served from one localhost port and the page
// from another → genuine cross-origin (mirrors instagram.com vs cdninstagram.com).
// We load it in a hidden Electron window, then compare, on the SAME image:
//   - canvas readback  (expected: TAINTED → SecurityError)
//   - CDP getResponseBody (expected: bytes byte-identical to the source file)
//
// Run: NODE_OPTIONS=--import=tsx ./node_modules/.bin/electron scripts/capture-spike/test.ts

import { app, BrowserWindow } from 'electron';
import http from 'http';
import fs from 'fs';
import os from 'os';
import path from 'path';
import crypto from 'crypto';
import { AddressInfo } from 'net';

// Launched from the repo root (electron <script>); avoid __dirname (undefined in
// the ESM scope tsx loads this in).
const IMG_FILE = path.join(process.cwd(), 'build', 'icon.png');
const OUT_DIR = path.join(os.tmpdir(), 'shelfy-capture-spike');

function listen(server: http.Server): Promise<number> {
  return new Promise((resolve) =>
    server.listen(0, '127.0.0.1', () => resolve((server.address() as AddressInfo).port)),
  );
}

async function main(): Promise<void> {
  const imgBytes = fs.readFileSync(IMG_FILE);
  fs.mkdirSync(OUT_DIR, { recursive: true });

  // Server A: serves the image (the "CDN"). Counts every HTTP hit so we can prove
  // getResponseBody reads from the browser's buffer and never re-fetches.
  let imgHits = 0;
  const imgServer = http.createServer((req, res) => {
    imgHits += 1;
    console.log(`[spike] IMG-SERVER HIT #${imgHits} ${req.url}`);
    res.writeHead(200, { 'Content-Type': 'image/png', 'Content-Length': imgBytes.length });
    res.end(imgBytes);
  });
  // Server B: serves the page (the "site"). Different port ⇒ cross-origin.
  let imgPort = 0;
  const pageServer = http.createServer((_req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html' });
    res.end(`<!doctype html><html><body>
      <img id="t" src="http://127.0.0.1:${imgPort}/icon.png">
      <script>
        const img = document.getElementById('t');
        function report(o){ console.log('CANVAS_RESULT ' + JSON.stringify(o)); }
        img.onerror = () => report({ loaded:false });
        img.onload = () => {
          try {
            const c = document.createElement('canvas');
            c.width = img.naturalWidth; c.height = img.naturalHeight;
            c.getContext('2d').drawImage(img, 0, 0);
            const url = c.toDataURL('image/png');   // throws if tainted
            report({ loaded:true, canvasOk:true, bytes:url.length });
          } catch (e) {
            report({ loaded:true, canvasOk:false, error:(e && e.name) || String(e) });
          }
        };
      </script>
    </body></html>`);
  });

  imgPort = await listen(imgServer);
  const pagePort = await listen(pageServer);

  console.log('[spike] servers up: page', pagePort, 'img', imgPort);
  const win = new BrowserWindow({
    show: false,
    width: 800,
    height: 600,
    webPreferences: {
      nodeIntegration: false,
      contextIsolation: true,
      sandbox: false,
      webSecurity: true,
    },
  });
  const wc = win.webContents;
  console.log('[spike] window created');

  // Warm up the renderer/target BEFORE attaching — sendCommand on a never-loaded
  // about:blank target never resolves in this headless context.
  await wc.loadURL('about:blank');
  console.log('[spike] blank loaded');

  // ---- CDP capture (the path we want to prove) ----
  const cdp = { got: false, bytes: 0, identical: false, base64: false, mime: '' };
  wc.debugger.attach('1.3');
  console.log('[spike] debugger attached');
  const pending = new Map<string, { url: string; mime: string }>();
  let lastReqId = '';
  wc.debugger.on('message', (_e, method, params) => {
    if (method === 'Network.responseReceived') {
      const p = params as {
        requestId: string;
        type?: string;
        response?: { url?: string; mimeType?: string };
      };
      const url = p.response?.url || '';
      if (/\/icon\.png$/.test(url))
        pending.set(p.requestId, { url, mime: p.response?.mimeType || '' });
    } else if (method === 'Network.loadingFinished') {
      const p = params as { requestId: string };
      const meta = pending.get(p.requestId);
      if (!meta) return;
      pending.delete(p.requestId);
      lastReqId = p.requestId;
      wc.debugger
        .sendCommand('Network.getResponseBody', { requestId: p.requestId })
        .then((res: { body: string; base64Encoded: boolean }) => {
          const buf = Buffer.from(res.body, res.base64Encoded ? 'base64' : 'utf8');
          cdp.got = true;
          cdp.bytes = buf.length;
          cdp.base64 = res.base64Encoded;
          cdp.mime = meta.mime;
          cdp.identical = buf.length === imgBytes.length && buf.equals(imgBytes);
          fs.writeFileSync(path.join(OUT_DIR, 'cdp-captured.png'), buf);
        })
        .catch((err: unknown) =>
          console.log('CDP getResponseBody ERROR', String((err as Error)?.message || err)),
        );
    }
  });
  await wc.debugger.sendCommand('Network.enable', { maxTotalBufferSize: 64 * 1024 * 1024 });
  console.log('[spike] network enabled');

  // ---- canvas result from the renderer ----
  let canvas: { loaded?: boolean; canvasOk?: boolean; error?: string; bytes?: number } = {};
  wc.on('console-message', (_e, _level, message) => {
    if (message.startsWith('CANVAS_RESULT ')) {
      try {
        canvas = JSON.parse(message.slice('CANVAS_RESULT '.length));
      } catch {}
    }
  });

  console.log('[spike] loading page...');
  await wc.loadURL(`http://127.0.0.1:${pagePort}/`);
  console.log('[spike] page loaded, waiting for probes...');

  // Give image load + both probes time to settle.
  await new Promise((r) => setTimeout(r, 2500));

  // ---- RE-FETCH CHECK: hammer getResponseBody; the image server must NOT see
  // any new request (the bytes come from Chromium's network buffer, not the net) ----
  const hitsAfterLoad = imgHits;
  for (let i = 0; i < 3; i++) {
    try {
      await wc.debugger.sendCommand('Network.getResponseBody', { requestId: lastReqId });
    } catch {}
  }
  await new Promise((r) => setTimeout(r, 300));
  const hitsAfterReread = imgHits;
  const noRefetch = hitsAfterLoad === 1 && hitsAfterReread === 1;

  const srcSha = crypto.createHash('sha1').update(imgBytes).digest('hex').slice(0, 12);
  const cdpSha = cdp.got
    ? crypto
        .createHash('sha1')
        .update(fs.readFileSync(path.join(OUT_DIR, 'cdp-captured.png')))
        .digest('hex')
        .slice(0, 12)
    : 'n/a';

  const canvasTainted = canvas.loaded === true && canvas.canvasOk === false;
  const pass = cdp.got && cdp.identical && canvasTainted && noRefetch;

  console.log('\n================ CAPTURE SPIKE RESULT ================');
  console.log(`source image      : ${IMG_FILE}`);
  console.log(`source size/sha1  : ${imgBytes.length}B / ${srcSha}`);
  console.log(
    `cross-origin setup: page :${pagePort}  ⇄  image :${imgPort}  (different ports ⇒ cross-origin)`,
  );
  console.log('-----------------------------------------------------');
  console.log(
    `CANVAS readback   : loaded=${canvas.loaded} canvasOk=${canvas.canvasOk} error=${canvas.error || '-'}`,
  );
  console.log(
    `                    → ${canvasTainted ? 'TAINTED (as predicted, canvas unusable)' : 'NOT tainted'}`,
  );
  console.log('-----------------------------------------------------');
  console.log(
    `CDP getResponseBody: got=${cdp.got} bytes=${cdp.bytes} base64=${cdp.base64} mime=${cdp.mime}`,
  );
  console.log(
    `                    captured sha1=${cdpSha}  ${cdp.identical ? 'BYTE-IDENTICAL to source ✓' : 'DIFFERS ✗'}`,
  );
  console.log(`                    saved → ${path.join(OUT_DIR, 'cdp-captured.png')}`);
  console.log('-----------------------------------------------------');
  console.log(`RE-FETCH CHECK    : image-server hits after page load    = ${hitsAfterLoad}`);
  console.log(`                    after 3× extra getResponseBody calls  = ${hitsAfterReread}`);
  console.log(
    `                    → ${noRefetch ? 'NO RE-FETCH ✓ (bytes from browser buffer, zero network)' : 'RE-FETCH DETECTED ✗'}`,
  );
  console.log('-----------------------------------------------------');
  console.log(
    `VERDICT           : ${pass ? '✅ PASS — intact bytes from the browser buffer; no canvas, no re-fetch' : '❌ FAIL — see above'}`,
  );
  console.log('=====================================================\n');

  try {
    wc.debugger.detach();
  } catch {}
  win.destroy();
  imgServer.close();
  pageServer.close();
  app.exit(pass ? 0 : 1);
}

console.log('[spike] script loaded');
app.disableHardwareAcceleration();
app.commandLine.appendSwitch('disable-gpu');
app
  .whenReady()
  .then(() => {
    console.log('[spike] app ready');
    return main();
  })
  .catch((err) => {
    console.error('SPIKE CRASH', err);
    app.exit(2);
  });

// Safety net: never hang.
setTimeout(() => {
  console.error('SPIKE TIMEOUT');
  app.exit(3);
}, 15000);
