// Smoke mirato: valida enrich (F3/F4-tech/F7) + persistenza (F6) + AI VLM web (F5)
// SENZA dipendere dalla cattura grafica (non disponibile headless). Usa un'immagine
// reale come screenshot surrogato, ma il TESTO reale del sito come autorità AI.
//   electron scripts/smoke-web-ai.cjs
import { app } from 'electron';
import fs from 'fs';
import path from 'path';
import * as db from '../electron/db';
import * as enrich from '../electron/web-enrich';
import * as analyzer from '../electron/analyzer';

app.setPath('userData', path.join(app.getPath('appData'), 'Shelfy'));
app.on('window-all-closed', () => {});

const TARGET_URL = process.env.SMOKE_URL || 'https://nextjs.org';
const SURROGATE =
  process.env.SMOKE_IMG ||
  path.join(app.getPath('appData'), 'Shelfy', 'assets', 'images', 'instagram-DPpgYcJgQAi-3.jpg');
const log = (...a: unknown[]) => console.log('[smoke-ai]', ...a);

const UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36';

async function main(): Promise<void> {
  db.initialize();
  log('db inizializzato; surrogate screenshot:', SURROGATE, 'esiste:', fs.existsSync(SURROGATE));

  // 1) ENRICH su HTML reale
  log('fetch', TARGET_URL);
  const resp = await fetch(TARGET_URL, { headers: { 'user-agent': UA } });
  const html = await resp.text();
  const headers = Object.fromEntries(resp.headers.entries());
  log('HTML bytes:', html.length, '| status:', resp.status);

  const content = enrich.extractContent(html, TARGET_URL);
  log('title:', content.title);
  log('lang:', content.lang, '| mainText len:', (content.mainText || '').length);
  log('mainText snippet:', JSON.stringify((content.mainText || '').slice(0, 180)));
  const tech = await enrich.detectTechStack(html, headers);
  log('techStack:', JSON.stringify(tech));
  const awards = enrich.detectAwards([{ url: TARGET_URL, html }], TARGET_URL);
  log('awards:', JSON.stringify(awards));
  const agg = enrich.aggregateSiteText([{ title: content.title, content }]);
  log('aggregated contentText len:', (agg.contentText || '').length);

  // 2) PERSISTENZA: costruisci WebReference (screenshot surrogato) e upsert
  const ref = {
    url: TARGET_URL,
    finalUrl: TARGET_URL,
    domain: new URL(TARGET_URL).hostname,
    title: content.title,
    description: content.metaDescription,
    lang: content.lang,
    pages: [
      {
        url: TARGET_URL,
        pageType: 'home',
        screenshotPath: SURROGATE,
        contentText: agg.contentText,
        title: content.title,
      },
    ],
    palette: [],
    fonts: [],
    techStack: tech,
    awards,
    capturedAt: Math.floor(Date.now() / 1000),
  };
  const up = db.upsertWebReference(ref, { overwriteAi: false });
  const id = (up && up.id) || (db.webPostId ? db.webPostId(TARGET_URL) : null);
  log('upsertWebReference →', JSON.stringify(up));

  const post = db.getPost(id!)!;
  log('--- POST dopo upsert ---');
  log('platform/mediaType:', post.platform, post.mediaType);
  log(
    'domain:',
    post.webDomain,
    '| image_path:',
    post.imagePath,
    '| esiste:',
    !!(post.imagePath && fs.existsSync(post.imagePath)),
  );
  log(
    'media slide:',
    (post.media || []).length,
    '| webTech:',
    JSON.stringify(post.webTech),
    '| webAwards:',
    JSON.stringify(post.webAwards),
  );
  log('text (caption) len:', (post.text || '').length, '| ai_status:', post.aiStatus);

  // 3) AI VLM web: accoda e attendi
  log('--- analisi AI (VLM web, kind=web) ---');
  analyzer.enqueuePost(post);
  const deadline = Date.now() + 240000;
  let fin: Shelfy.Post | null = null;
  while (Date.now() < deadline) {
    const p = db.getPost(id!);
    if (p && (p.aiStatus === 'done' || p.aiStatus === 'error')) {
      fin = p;
      break;
    }
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
    log('AI non conclusa entro il timeout.');
  }
  log('POST DI TEST id:', id, "(cancellabile dall'app)");
}

const hardStop = setTimeout(() => {
  console.error('[smoke-ai] HARD TIMEOUT');
  try {
    app.exit(2);
  } catch {}
}, 320000);
app
  .whenReady()
  .then(main)
  .catch((e) => console.error('[smoke-ai] FATAL', (e && e.stack) || e))
  .finally(() => {
    clearTimeout(hardStop);
    try {
      app.quit();
    } catch {}
  });
