'use strict';
// One-off verifier: opens an Instagram saved-collection grid in a window that
// REUSES the real logged-in 'persist:social' session (we do NOT override
// userData, so app.getPath resolves to the real Application Support/Shelfy),
// scrolls to the bottom while accumulating every post shortcode it sees —
// both from the DOM (visible tiles) and from the intercepted JSON network
// responses (the authoritative collection feed) — then opens shelfy.sqlite
// read-only and diffs the collection's posts against what Instagram actually
// shows. Prints: IG total, DB total, and the shortcodes missing on each side.
//
// Run:  npx electron scripts/verify-collection.cjs <collection-url>
// Default URL is the "motivational" collection.

import { app, BrowserWindow } from 'electron';
import type { WebContents } from 'electron';
import fs from 'fs';
import path from 'path';

// CRITICAL: running `electron <script>` defaults the app name to "Electron",
// so userData (and the persist:social partition) would point at an empty
// Application Support/Electron dir — i.e. a logged-OUT session. Force userData
// to the real app dir so we reuse the actual logged-in cookies + read the real
// shelfy.sqlite. (macOS FS is case-insensitive: shelfy == Shelfy == SHELFY.)
const REAL_USERDATA = path.join(app.getPath('appData'), 'Shelfy');
app.setPath('userData', REAL_USERDATA);
app.setPath('sessionData', REAL_USERDATA);

const URL_ARG =
  process.argv.find((a) => a.startsWith('http')) ||
  process.env.SHELFY_IG_COLLECTION_URL ||
  'https://www.instagram.com/<your-handle>/saved/<collection-name>/<collection-id>/';

// external_id of the IG folder, parsed from the URL (last numeric segment).
// Strip any query string / fragment first: real saved-collection URLs often
// carry tracking params or `?hl=...`, so end-anchoring on the raw URL would miss
// the id (e.g. `.../17950491527848169/?hl=it`) and silently zero the DB diff.
const URL_PATH = URL_ARG.split(/[?#]/)[0].replace(/\/+$/, '');
const EXTERNAL_ID = (URL_PATH.match(/\/(\d{6,})$/) || [])[1] || null;

const OUT = '/tmp/verify-collection';
fs.mkdirSync(OUT, { recursive: true });

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// ---- DB row shapes ----------------------------------------------------------
interface CollectionRow {
  id: number;
  name?: string | null;
  ig_name?: string | null;
  external_id?: string | number | null;
}
interface PostRow {
  id: string;
  shortcode: string | null;
  post_url: string | null;
}
interface DbInfo {
  collections: CollectionRow[];
  col: CollectionRow | null;
  posts: PostRow[];
}

// ---- accumulators -----------------------------------------------------------
const domCodes = new Set<string>(); // shortcodes seen as visible tiles
const netCodes = new Set<string>(); // shortcodes seen in intercepted JSON feed responses
let moreAvailable: boolean | null = null; // last `more_available` flag from REST feed responses

// ---- network interception via CDP ------------------------------------------
function isFeedUrl(u: string): boolean {
  return (
    u.includes('/graphql/query') ||
    u.includes('/api/v1/feed/saved/') ||
    u.includes('/api/v1/feed/collection/')
  );
}

// Walk a parsed JSON body and harvest post identifiers. IG marks posts with
// `code`/`shortcode` (REST: item.code, GraphQL: node.shortcode). We also read
// `more_available` to know when the collection feed is exhausted.
function harvest(obj: unknown, depth = 0): void {
  if (!obj || typeof obj !== 'object' || depth > 60) return;
  if (Array.isArray(obj)) {
    for (const v of obj) harvest(v, depth + 1);
    return;
  }
  const rec = obj as Record<string, unknown>;
  if (typeof rec.more_available === 'boolean') moreAvailable = rec.more_available;
  const code = rec.shortcode || rec.code;
  // A real post node always carries a pk/id alongside its code; this filters
  // out unrelated "code" fields (country codes, error codes, …).
  if (typeof code === 'string' && /^[A-Za-z0-9_-]{6,15}$/.test(code) && (rec.pk || rec.id)) {
    netCodes.add(code);
  }
  for (const k of Object.keys(rec)) harvest(rec[k], depth + 1);
}

function attachNetwork(wc: WebContents): void {
  const dbg = wc.debugger;
  try {
    dbg.attach('1.3');
  } catch (e) {
    console.warn('debugger attach failed:', (e as Error).message);
    return;
  }
  const pending = new Map<string, string>(); // requestId -> url
  dbg.on('message', (_e, method, params) => {
    if (method === 'Network.responseReceived' && isFeedUrl(params.response.url)) {
      pending.set(params.requestId, params.response.url);
    } else if (method === 'Network.loadingFinished' && pending.has(params.requestId)) {
      const url = pending.get(params.requestId);
      pending.delete(params.requestId);
      dbg
        .sendCommand('Network.getResponseBody', { requestId: params.requestId })
        .then(({ body, base64Encoded }) => {
          const text = base64Encoded ? Buffer.from(body, 'base64').toString('utf8') : body;
          try {
            harvest(JSON.parse(text));
          } catch (_) {
            /* non-JSON or partial; ignore */
          }
        })
        .catch(() => {});
    }
  });
  dbg.sendCommand('Network.enable').catch(() => {});
}

// ---- DOM accumulation -------------------------------------------------------
const COLLECT_DOM = `(() => {
  const sel = 'a[href*="/p/"], a[href*="/reel/"], a[href*="/tv/"]';
  const out = [];
  document.querySelectorAll(sel).forEach((a) => {
    const m = (a.getAttribute('href')||'').match(/\\/(?:p|reel|tv)\\/([^/?#]+)/);
    if (m) out.push(m[1]);
  });
  return out;
})()`;

async function collectDom(wc: WebContents): Promise<void> {
  try {
    const codes: string[] = await wc.executeJavaScript(COLLECT_DOM, true);
    codes.forEach((c) => domCodes.add(c));
  } catch (_) {}
}

async function scrollOnce(wc: WebContents): Promise<void> {
  await wc.executeJavaScript(
    `(() => {
       const t = document.querySelectorAll('a[href*="/p/"], a[href*="/reel/"], a[href*="/tv/"]');
       const el = t[t.length-1];
       if (el) el.scrollIntoView({ behavior:'instant', block:'end' });
       else window.scrollBy(0, window.innerHeight*0.9);
     })()`,
    true,
  );
}

// ---- DB read ----------------------------------------------------------------
function readDb(): DbInfo {
  const Database = require('better-sqlite3') as typeof import('better-sqlite3');
  const dbPath = path.join(app.getPath('userData'), 'shelfy.sqlite');
  const db = new Database(dbPath, { readonly: true, fileMustExist: true });

  const collections = db.prepare<unknown[], CollectionRow>('SELECT * FROM collections').all();
  const col =
    (EXTERNAL_ID && collections.find((c) => String(c.external_id) === String(EXTERNAL_ID))) ||
    collections.find((c) => /motivat/i.test(c.name || '') || /motivat/i.test(c.ig_name || ''));

  // Fail loudly: if we couldn't parse an external_id from the URL AND the name
  // fallback matched nothing, the diff below would falsely report the whole
  // collection as un-imported (dbTotal=0). Make that diagnostic-killing case
  // obvious instead of silent.
  if (!col) {
    console.warn(
      EXTERNAL_ID
        ? `\n!! No collection with external_id=${EXTERNAL_ID} in the DB — DB diff will be empty.`
        : '\n!! Could not parse an external_id from the URL and the name fallback matched no collection — DB diff will be empty.',
    );
  }

  let posts: PostRow[] = [];
  if (col) {
    posts = db
      .prepare<unknown[], PostRow>(
        `SELECT p.id, p.shortcode, p.post_url
           FROM posts p
           JOIN post_collections pc ON pc.post_id = p.id
          WHERE pc.collection_id = ?`,
      )
      .all(col.id);
  }
  db.close();
  return { collections, col: col || null, posts };
}

// ---- main flow --------------------------------------------------------------
let win: BrowserWindow | null = null;
let done = false;

async function run(): Promise<void> {
  if (done) return;
  done = true;
  const wc = win!.webContents;

  // Wait for the grid to mount tiles (or for a logged-out redirect).
  let ready = false;
  for (let i = 0; i < 50; i++) {
    const url = wc.getURL();
    if (/accounts\/login/.test(url)) {
      console.error('\n!! Redirected to login — the persist:social session is NOT logged in.');
      console.error('   Current URL:', url);
      setTimeout(() => app.quit(), 300);
      return;
    }
    const c = await wc.executeJavaScript(
      `document.querySelectorAll('a[href*="/p/"], a[href*="/reel/"], a[href*="/tv/"]').length`,
      true,
    );
    if (c > 0) {
      ready = true;
      break;
    }
    await sleep(500);
  }
  if (!ready) console.warn('No tiles appeared after wait; continuing anyway.');

  // Scroll until the DOM shortcode set stops growing for several rounds AND the
  // network feed reports no more pages.
  let stable = 0;
  let lastSize = -1;
  for (let i = 0; i < 400 && stable < 8; i++) {
    await collectDom(wc);
    await scrollOnce(wc);
    await sleep(550);
    const size = domCodes.size;
    if (size === lastSize) stable++;
    else {
      stable = 0;
      lastSize = size;
    }
    if (i % 5 === 0) {
      console.log(
        `[scroll ${i}] dom=${domCodes.size} net=${netCodes.size} more_available=${moreAvailable} stable=${stable}`,
      );
    }
    // If the feed explicitly says no more pages and DOM is steady, we can stop.
    if (moreAvailable === false && stable >= 4) break;
  }
  // A few extra passes from the top to re-mount any tile that unmounted while
  // we were at the bottom (virtualization can drop earlier rows).
  await wc.executeJavaScript(`window.scrollTo(0,0)`, true);
  await sleep(800);
  for (let i = 0; i < 30; i++) {
    await collectDom(wc);
    await scrollOnce(wc);
    await sleep(450);
  }
  await collectDom(wc);

  // Union of both signals = best estimate of what IG actually shows.
  const igCodes = new Set([...domCodes, ...netCodes]);

  // Diff against the DB.
  let dbInfo: DbInfo;
  try {
    dbInfo = readDb();
  } catch (e) {
    console.error('DB read failed:', (e as Error).message);
    dbInfo = { collections: [], col: null, posts: [] };
  }
  const dbCodes = new Set(dbInfo.posts.map((p) => p.shortcode).filter(Boolean) as string[]);

  const missingInDb = [...igCodes].filter((c) => !dbCodes.has(c)); // on IG, not imported
  const extraInDb = [...dbCodes].filter((c) => !igCodes.has(c)); // in DB, not seen on IG

  const report = {
    url: URL_ARG,
    externalId: EXTERNAL_ID,
    igTotal: igCodes.size,
    igFromDom: domCodes.size,
    igFromNetwork: netCodes.size,
    moreAvailableLast: moreAvailable,
    dbCollection: dbInfo.col
      ? {
          id: dbInfo.col.id,
          name: dbInfo.col.name,
          ig_name: dbInfo.col.ig_name,
          external_id: dbInfo.col.external_id,
        }
      : null,
    dbTotal: dbCodes.size,
    missingInDbCount: missingInDb.length,
    extraInDbCount: extraInDb.length,
    missingInDb,
    extraInDb,
    igCodes: [...igCodes],
    dbCodes: [...dbCodes],
    allCollections: dbInfo.collections.map((c) => ({
      id: c.id,
      name: c.name,
      ig_name: c.ig_name,
      external_id: c.external_id,
    })),
  };
  fs.writeFileSync(path.join(OUT, 'report.json'), JSON.stringify(report, null, 2));

  console.log('\n================ RISULTATO ================');
  console.log('URL            :', URL_ARG);
  console.log('external_id    :', EXTERNAL_ID);
  console.log('IG totale      :', igCodes.size, `(dom=${domCodes.size}, net=${netCodes.size})`);
  console.log('more_available :', moreAvailable);
  console.log(
    'DB collection  :',
    dbInfo.col
      ? `${dbInfo.col.name} (id=${dbInfo.col.id}, ext=${dbInfo.col.external_id})`
      : 'NON TROVATA',
  );
  console.log('DB totale      :', dbCodes.size);
  console.log('Mancanti in DB :', missingInDb.length, missingInDb.slice(0, 60).join(' '));
  console.log('In DB non su IG:', extraInDb.length, extraInDb.slice(0, 60).join(' '));
  console.log('Report -> ', path.join(OUT, 'report.json'));
  console.log('==========================================\n');

  setTimeout(() => app.quit(), 400);
}

app.whenReady().then(() => {
  console.log('userData:', app.getPath('userData'));
  win = new BrowserWindow({
    width: 1400,
    height: 1000,
    webPreferences: { partition: 'persist:social', contextIsolation: true, nodeIntegration: false },
  });
  attachNetwork(win.webContents);
  win.loadURL(URL_ARG);
  let started = false;
  win.webContents.on('did-stop-loading', async () => {
    if (started) return;
    started = true;
    await sleep(2000);
    run();
  });
  // Safety: if did-stop-loading never fires, kick off anyway.
  setTimeout(() => {
    if (!started) {
      started = true;
      run();
    }
  }, 12000);
});

app.on('window-all-closed', () => app.quit());
