'use strict';

// Build prep: compile the content-blocking engine the web-capture pipeline uses
// (ads, trackers, cookie banners, newsletter/chat pop-ups) into a single
// serialized file, so the packaged app ships it as extraResources
// (→ resources/adblock/engine.bin) and never touches the network at runtime.
//
// Lists: the EasyList family (EasyList, EasyPrivacy, EasyList Cookie — dual
// licensed GPLv3 / CC BY-SA 3.0), Peter Lowe's ad server list, plus Shelfy's own
// rules for chat and pop-up vendors (below). The engine file is distributed as a
// separate data file alongside the app (see THIRD-PARTY-NOTICES.md).
//
// Idempotent: skips the download when an engine for the current library version
// already exists. Run automatically by the `build`/`release` scripts and by
// postinstall (best-effort), or manually: tsx build/prepare-adblock.ts [--force]

import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import { FiltersEngine, ENGINE_VERSION } from '@ghostery/adblocker';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, '..');
const DEST_DIR = path.join(ROOT, 'build', 'adblock');
const DEST = path.join(DEST_DIR, 'engine.bin');
const STAMP = path.join(DEST_DIR, 'engine.json');

const ASSETS =
  'https://raw.githubusercontent.com/ghostery/adblocker/master/packages/adblocker/assets';
const LISTS = [
  `${ASSETS}/easylist/easylist.txt`,
  `${ASSETS}/easylist/easyprivacy.txt`,
  `${ASSETS}/easylist/easylist-cookie.txt`,
  `${ASSETS}/peter-lowe/serverlist.txt`,
];

// Chat widgets, newsletter/exit-intent pop-ups and survey overlays: they cover
// the design in a reference screenshot and are never part of the site's own look.
const SHELFY_RULES = [
  // Live chat / support widgets
  '||widget.intercom.io^$third-party',
  '||js.intercomcdn.com^$third-party',
  '||js.driftt.com^$third-party',
  '||client.crisp.chat^$third-party',
  '||code.tidio.co^$third-party',
  '||js.hs-scripts.com^$third-party',
  '||js.usemessages.com^$third-party',
  '||static.zdassets.com^$third-party',
  '||cdn.livechatinc.com^$third-party',
  '||embed.tawk.to^$third-party',
  '||config.gorgias.chat^$third-party',
  '||wchat.freshchat.com^$third-party',
  '||beacon-v2.helpscout.net^$third-party',
  '||widget.trengo.eu^$third-party',
  '||chat.olark.com^$third-party',
  '||cdn.voiceflow.com^$third-party',
  // Newsletter / exit-intent / spin-to-win pop-ups
  '||static.klaviyo.com/onsite/$third-party',
  '||widget.privy.com^$third-party',
  '||a.omappapi.com^$third-party',
  '||load.sumo.com^$third-party',
  '||sumo.com/api/load^$third-party',
  '||cdn.justuno.com^$third-party',
  '||wisepops.com^$third-party',
  '||cdn.attn.tv^$third-party',
  '||popupsmart.com^$third-party',
  '||poptin.com^$third-party',
  '||sleeknote.com^$third-party',
  // Accessibility overlay widgets (floating launcher buttons)
  '||cdn.userway.org^$third-party',
  '||acsbapp.com^$third-party',
  '||acsbace.com^$third-party',
  '||cdn.equalweb.com^$third-party',
  '||ws.audioeye.com^$third-party',
  '||cdn.enable.co.il^$third-party',
  '||static.recite.me^$third-party',
  '##.uwy',
  '##.acsb-trigger',
  // Feedback / survey overlays
  '||static.hotjar.com^$third-party',
  '||widget.usersnap.com^$third-party',
  '||survey.survicate.com^$third-party',
  '||cdn.userpilot.io^$third-party',
  // Generic cosmetic fallbacks for widgets loaded first-party
  '##.intercom-lightweight-app',
  '##iframe[title="Intercom live chat"]',
  '###hubspot-messages-iframe-container',
  '##.crisp-client',
  '###tidio-chat',
  '##iframe#launcher[title*="chat" i]',
  '##.klaviyo-form[role="dialog"]',
];

async function fetchList(url: string): Promise<string> {
  const res = await fetch(url, { signal: AbortSignal.timeout(60_000) });
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
  return res.text();
}

async function main(): Promise<void> {
  const force = process.argv.includes('--force');
  if (!force && fs.existsSync(DEST) && fs.existsSync(STAMP)) {
    try {
      const stamp = JSON.parse(fs.readFileSync(STAMP, 'utf8')) as { engineVersion?: number };
      if (stamp.engineVersion === ENGINE_VERSION) {
        console.log(`[prepare-adblock] engine up to date (v${ENGINE_VERSION}) — skip`);
        return;
      }
    } catch {
      /* rebuild below */
    }
  }
  console.log(`[prepare-adblock] downloading ${LISTS.length} filter lists…`);
  const texts = await Promise.all(LISTS.map(fetchList));
  const engine = FiltersEngine.parse([...texts, SHELFY_RULES.join('\n')].join('\n'), {
    loadCosmeticFilters: true,
    loadNetworkFilters: true,
    enableCompression: true,
    enableHtmlFiltering: false,
  });
  fs.mkdirSync(DEST_DIR, { recursive: true });
  const buf = engine.serialize();
  fs.writeFileSync(DEST, buf);
  fs.writeFileSync(
    STAMP,
    JSON.stringify(
      { engineVersion: ENGINE_VERSION, lists: LISTS, builtAt: new Date().toISOString() },
      null,
      2,
    ) + '\n',
  );
  console.log(`[prepare-adblock] wrote ${DEST} (${(buf.length / 1024).toFixed(0)} KB)`);
}

main().catch((err) => {
  console.error('[prepare-adblock] failed:', err instanceof Error ? err.message : err);
  process.exitCode = 1;
});
