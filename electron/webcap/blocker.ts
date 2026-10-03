// Content blocking for the web-reference capture: ads, trackers, cookie
// banners, chat launchers and pop-up vendors (Ghostery's FiltersEngine over the
// EasyList family + Shelfy's own rules, compiled at build time by
// build/prepare-adblock.ts). One engine instance is shared by both capture
// drivers: Playwright routes requests through matchRequest(); the Electron
// driver calls it from session.webRequest. Cosmetic CSS is computed per frame.
//
// If the compiled engine is missing (dev checkout without postinstall, corrupted
// install) the capture still works — only without blocking.

import fs from 'fs';
import path from 'path';
import { app } from 'electron';
import { FiltersEngine, Request } from '@ghostery/adblocker';
import { parse as parseDomain } from 'tldts-experimental';

let enginePromise: Promise<FiltersEngine | null> | null = null;

function enginePath(): string {
  // Injectable path for the capture service (plan §2.18: the prebuilt adblock
  // engine ships in the image); otherwise the packaged resources dir, else dev.
  if (process.env.SHELFY_ADBLOCK_ENGINE) return process.env.SHELFY_ADBLOCK_ENGINE;
  try {
    if (app.isPackaged) return path.join(process.resourcesPath || '', 'adblock', 'engine.bin');
  } catch {
    /* fall through to the dev path */
  }
  return path.join(__dirname, '..', '..', 'build', 'adblock', 'engine.bin');
}

export function getEngine(): Promise<FiltersEngine | null> {
  if (!enginePromise) {
    enginePromise = (async () => {
      try {
        const buf = await fs.promises.readFile(enginePath());
        return FiltersEngine.deserialize(new Uint8Array(buf));
      } catch (err) {
        console.warn(
          '[webcap/blocker] content-blocking engine unavailable — capturing without it:',
          (err as Error)?.message || err,
        );
        return null;
      }
    })();
  }
  return enginePromise;
}

// Playwright / CDP resource types → the adblocker's request types.
const TYPE_MAP: Record<string, string> = {
  document: 'main_frame',
  stylesheet: 'stylesheet',
  image: 'image',
  media: 'media',
  font: 'font',
  script: 'script',
  texttrack: 'other',
  xhr: 'xmlhttprequest',
  fetch: 'xmlhttprequest',
  eventsource: 'xmlhttprequest',
  websocket: 'websocket',
  manifest: 'other',
  ping: 'ping',
  cspreport: 'csp_report',
  other: 'other',
  // Electron webRequest resourceType values
  mainFrame: 'main_frame',
  subFrame: 'sub_frame',
  cspReport: 'csp_report',
  object: 'object',
};

// True when the request should be blocked. Main-frame documents are never
// blocked (the user asked for that page); everything else is matched.
export function shouldBlock(
  engine: FiltersEngine | null,
  url: string,
  sourceUrl: string,
  resourceType: string,
  isMainFrame: boolean,
): boolean {
  if (!engine || isMainFrame) return false;
  try {
    const type = TYPE_MAP[resourceType] || 'other';
    const req = Request.fromRawDetails({
      url,
      sourceUrl: sourceUrl || undefined,
      type: type as Parameters<typeof Request.fromRawDetails>[0]['type'],
    });
    const { match, redirect } = engine.match(req);
    return !!match && !redirect;
  } catch {
    return false;
  }
}

// DOM features the cosmetic engine keys element-hiding rules on.
export const JS_COSMETIC_FEATURES = `(() => {
  const ids = new Set(), classes = new Set(), hrefs = new Set();
  let n = 0;
  for (const el of document.querySelectorAll('[id], [class], a[href]')) {
    if (n++ > 8000) break;
    if (el.id) ids.add(el.id);
    if (el.classList) for (const c of el.classList) { if (classes.size < 6000) classes.add(c); }
    if (el.tagName === 'A') { const h = el.getAttribute('href'); if (h && hrefs.size < 3000) hrefs.add(h); }
  }
  return { ids: Array.from(ids).slice(0, 4000), classes: Array.from(classes), hrefs: Array.from(hrefs) };
})()`;

// Element-hiding CSS for a page URL + its DOM features. Scriptlets are NOT
// injected: they patch page JavaScript and are only needed against anti-adblock,
// which a reference capture does not care about.
export function cosmeticCss(
  engine: FiltersEngine | null,
  url: string,
  features: { ids?: string[]; classes?: string[]; hrefs?: string[] } | null,
): string {
  if (!engine) return '';
  try {
    const parsed = parseDomain(url);
    const { styles } = engine.getCosmeticsFilters({
      url,
      hostname: parsed.hostname || '',
      domain: parsed.domain || '',
      ids: features?.ids || [],
      classes: features?.classes || [],
      hrefs: features?.hrefs || [],
      getBaseRules: true,
      getInjectionRules: false,
      getExtendedRules: false,
      getRulesFromHostname: true,
      getRulesFromDOM: true,
    });
    return styles || '';
  } catch {
    return '';
  }
}
