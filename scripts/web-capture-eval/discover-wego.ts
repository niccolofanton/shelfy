'use strict';

// Harness fittizio di DISCOVERY (F1) — non cattura nulla, esercita SOLO la pipeline
// `webcapture.discoverPages()` su un sito reale (default: we-go.it) e mostra:
//   • quali pagine la codebase seleziona e in che ordine (pages[0] = "home"/hero)
//   • qual è la home REALE risolta dai redirect e dalla sitemap
//   • un confronto che smaschera l'errore di rilevamento della home.
//
// Gira in Node puro (no Electron): `electron` è richiesto da webcapture.js solo per
// la cattura (BrowserWindow/session), MAI dalla discovery — lo stubbiamo via Module._load.
//
//   node scripts/web-capture-eval/discover-wego.cjs            # we-go.it
//   node scripts/web-capture-eval/discover-wego.cjs example.com # altro sito

import Module from 'module';
import path from 'path';

// Module._load è API interna non esposta dai tipi pubblici di node: la descriviamo
// localmente per poterla intercettare con tipi reali.
type ModuleLoad = (request: string, parent: unknown, isMain: boolean) => unknown;
interface ModuleWithLoad {
  _load: ModuleLoad;
}
const ModuleInternal = Module as unknown as ModuleWithLoad;

const origLoad = ModuleInternal._load;
ModuleInternal._load = function (this: unknown, request: string): unknown {
  if (request === 'electron') {
    // Stub minimale: la discovery non tocca nulla di tutto questo.
    return {
      app: { getPath: () => '/tmp/shelfy-discover-harness' },
      session: { fromPartition: () => ({}) },
      BrowserWindow: function () {},
    };
  }
  // eslint-disable-next-line prefer-rest-params
  return origLoad.apply(this, arguments as unknown as Parameters<ModuleLoad>);
};

const webcapture = require(
  path.join(__dirname, '..', '..', 'electron', 'webcapture'),
) as typeof import('../../electron/webcapture');

const INPUT = process.argv[2] || 'we-go.it';

function pad(s: string | number, n: number): string {
  s = String(s);
  return s.length >= n ? s : s + ' '.repeat(n - s.length);
}

(async () => {
  console.log(`\n=== DISCOVERY HARNESS — input utente: "${INPUT}" ===\n`);

  const res = await webcapture.discoverPages(INPUT, { maxPages: 6 });

  console.log('Risolto (finalUrl):', res.finalUrl);
  console.log('Origin            :', res.origin);
  console.log('Domain            :', res.domain);
  console.log('Source            :', res.source);
  console.log(`Pagine selezionate (${res.pages.length}, in ordine — pages[0] = hero/"home"):\n`);

  res.pages.forEach((p, i) => {
    let pathname = p.url;
    try {
      pathname = new URL(p.url).pathname;
    } catch {}
    const homeTag = i === 0 ? '  ← pages[0] (usata come HOME/hero)' : '';
    console.log(
      `  ${i}. ${pad(p.templateHint, 12)} score=${pad(p.score, 4)} ${pad(pathname, 30)}${homeTag}`,
    );
  });

  // Qual è la home REALE? È il finalUrl a cui i redirect dell'origine portano.
  let realHomePath = '/';
  try {
    realHomePath = new URL(res.finalUrl).pathname;
  } catch {}

  const heroUrl = res.pages[0] ? res.pages[0].url : null;
  let heroPath: string | null = null;
  try {
    if (heroUrl) heroPath = new URL(heroUrl).pathname;
  } catch {}

  const homeInList = res.pages.find((p) => {
    try {
      return new URL(p.url).pathname.replace(/\/$/, '') === realHomePath.replace(/\/$/, '');
    } catch {
      return false;
    }
  });

  console.log('\n--- CONFRONTO: home reale vs scelta della codebase ---\n');
  console.log('Home REALE (da redirect):', realHomePath, `(${res.finalUrl})`);
  console.log('pages[0] scelta come hero:', heroPath, `(${heroUrl})`);
  console.log(
    'La home reale è presente nella lista selezionata?',
    homeInList
      ? `SÌ (score ${homeInList.score}, hint "${homeInList.templateHint}")`
      : 'NO — scartata',
  );

  const heroIsHome =
    heroPath && realHomePath && heroPath.replace(/\/$/, '') === realHomePath.replace(/\/$/, '');
  console.log('\nESITO:', heroIsHome ? '✅ hero == home' : '❌ BUG: hero ≠ home reale');
})().catch((e) => {
  console.error('FATAL', e && e.stack ? e.stack : e);
  process.exit(2);
});
