// Smoke test end-to-end della pipeline "siti web come reference".
// Esegue dentro Electron (serve BrowserWindow + binari nativi/ABI Electron).
//   electron scripts/smoke-web.cjs            (default URL)
//   SMOKE_URL=https://... electron scripts/smoke-web.cjs
// Scrive un post web di TEST nel DB reale (cancellabile dall'app).
const { app } = require('electron');
const fs = require('fs');
const path = require('path');

// Usa lo userData reale di "Shelfy" (modelli VLM, binari llama-server, DB).
app.setPath('userData', path.join(app.getPath('appData'), 'Shelfy'));

// Senza questo, Electron esce quando la finestra di cattura viene distrutta
// (dispose) → lo script morirebbe a metà. Nell'app reale la finestra principale
// resta sempre aperta, quindi questo è solo un artefatto dello smoke standalone.
app.on('window-all-closed', () => {});

const URL = process.env.SMOKE_URL || 'https://nextjs.org';
const MAX_PAGES = parseInt(process.env.SMOKE_MAXPAGES || '2', 10);
const log = (...a) => console.log('[smoke]', ...a);

async function main() {
  const orch = require('../electron/weborchestrator.js');
  const db = require('../electron/db.js');
  const analyzer = require('../electron/analyzer.js');

  // Il modulo db.js non si auto-inizializza: senza questa chiamata `db` resta
  // null e ogni funzione lancia 'Database not initialized' (cfr. smoke-web-ai.cjs).
  db.initialize();

  log('userData:', app.getPath('userData'));
  log('URL:', URL);
  try { log('model status:', JSON.stringify(analyzer.getModelStatus())); } catch (e) { log('model status n/d'); }

  const t0 = Date.now();
  let id = null;
  try {
    const res = await orch.captureWebReference(URL, {
      maxPages: MAX_PAGES,
      onProgress: (p) => log('progress', JSON.stringify(p)),
    });
    log('captureWebReference →', JSON.stringify(res));
    id = (res && (res.id || res.postId)) || null;
  } catch (e) {
    log('CAPTURE ERROR:', (e && e.stack) || e);
  }

  // Fallback: trova il post web più recente.
  if (!id) {
    try {
      const r = db.getPosts({ platform: 'web', source: 'web', limit: 1, sortOrder: 'newest' });
      const arr = (r && r.posts) || r || [];
      if (arr[0]) id = arr[0].id;
    } catch (e) { log('fallback getPosts err:', e.message); }
  }
  log('post id:', id);

  if (id) {
    const post = db.getPost(id);
    log('--- POST (dopo cattura/upsert) ---');
    if (post) {
      log('platform/mediaType:', post.platform, post.mediaType);
      log('domain:', post.webDomain, '| finalUrl:', post.webFinalUrl || post.postUrl);
      log('hero image_path:', post.imagePath, '| esiste:', !!(post.imagePath && fs.existsSync(post.imagePath)));
      log('media (slide pagine):', (post.media || []).length, '| webPages:', (post.webPages || []).length);
      log('webPalette:', JSON.stringify(post.webPalette));
      log('webFonts:', JSON.stringify(post.webFonts));
      log('webTech:', JSON.stringify(post.webTech));
      log('webAwards:', JSON.stringify(post.webAwards));
      log('ai_status:', post.aiStatus);
    }

    log('--- attendo analisi AI (VLM) ---');
    const deadline = Date.now() + 220000;
    let fin = null;
    while (Date.now() < deadline) {
      const p = db.getPost(id);
      if (p && (p.aiStatus === 'done' || p.aiStatus === 'error')) { fin = p; break; }
      await new Promise((r) => setTimeout(r, 3000));
    }
    if (fin) {
      log('=== AI status:', fin.aiStatus, '===');
      log('ai_description:', fin.aiDescription);
      log('purpose (ai_content_type):', fin.aiContentType);
      log('industry (ai_category):', fin.aiCategory);
      log('ai_tags:', JSON.stringify(fin.aiTags));
      log('ai_entities:', JSON.stringify(fin.aiEntities));
      log('ai_keywords:', JSON.stringify(fin.aiKeywords));
    } else {
      log('AI non conclusa entro il timeout (server/coda).');
    }
  }
  log('TOTALE', Math.round((Date.now() - t0) / 1000) + 's');
}

const hardStop = setTimeout(() => { console.error('[smoke] HARD TIMEOUT'); try { app.exit(2); } catch {} }, 320000);
app.whenReady()
  .then(main)
  .catch((e) => console.error('[smoke] FATAL', (e && e.stack) || e))
  .finally(() => { clearTimeout(hardStop); try { app.quit(); } catch {} });
