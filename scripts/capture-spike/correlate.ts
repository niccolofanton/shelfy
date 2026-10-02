// Certainty check for risk #1 (URL→post correlation), run AFTER a real sync with
// SHELFY_CAPTURE_MVP=1. Compares the image URLs the browser actually rendered
// (captured via CDP → capture-mvp/urls.txt) against the URLs stored in the DB,
// under several mediaKey strategies, and reports the match rate per platform.
//
// Read-only. Run headless: NODE_OPTIONS=--import=tsx node scripts/capture-spike/correlate.ts
// (uses the sqlite3 CLI, no native modules / no GUI).

import fs from 'fs';
import path from 'path';
import os from 'os';
import { execFileSync } from 'child_process';

const HOME = os.homedir();
const CDP_FILE =
  process.argv[2] || path.join(HOME, 'Library/Application Support/Shelfy/capture-mvp/urls.txt');
const DB = path.join(HOME, 'Library/Application Support/Shelfy/shelfy.sqlite');

type Platform = 'instagram' | 'twitter' | 'other';

function platformOf(url: string): Platform {
  if (/cdninstagram\.com|fbcdn\.net/.test(url)) return 'instagram';
  if (/twimg\.com/.test(url)) return 'twitter';
  return 'other';
}

// Is this a POST media image (correlatable) vs noise (avatars, UI icons)?
// IG: feed media live under t51.<n>-15/ ; profile pics are t51.2885-19/.
// TW: photos under /media/, video covers under /amplify_video_thumb/ ; avatars
//     under /profile_images/.
function isPostMedia(url: string): boolean {
  const p = platformOf(url);
  if (p === 'instagram') return /\/t51\.\d+-15\//.test(url);
  if (p === 'twitter')
    return /\/(media|amplify_video_thumb)\//.test(url) && !/profile_images/.test(url);
  return false;
}

// --- mediaKey strategies (the thing we want to pin down) ---
function basenameNoExt(url: string): string {
  try {
    const p = new URL(url).pathname;
    return (p.split('/').pop() || '').replace(/\.[a-z0-9]+$/i, '');
  } catch {
    return '';
  }
}
function igSecondGroup(url: string): string | null {
  const b = basenameNoExt(url); // e.g. 721466431_1732208121464604_7354..._n
  const m = b.match(/^(\d+)_(\d+)_/);
  return m ? m[2] : null;
}
function twId(url: string): string | null {
  try {
    const p = new URL(url).pathname;
    let m = p.match(/\/media\/([A-Za-z0-9_-]+?)(?:\.[a-z0-9]+)?$/);
    if (m) return m[1];
    m = p.match(/amplify_video_thumb\/(\d+)/);
    if (m) return m[1];
    return null;
  } catch {
    return null;
  }
}

const STRATEGIES: Record<string, (url: string) => string | null> = {
  'basename-full': (u) => basenameNoExt(u) || null,
  'ig-2nd-group': (u) =>
    platformOf(u) === 'instagram' ? igSecondGroup(u) : basenameNoExt(u) || null,
  'tw-mediaid': (u) => (platformOf(u) === 'twitter' ? twId(u) : basenameNoExt(u) || null),
};

function sql(q: string): string[] {
  const out = execFileSync('sqlite3', ['-readonly', DB, q], {
    encoding: 'utf8',
    maxBuffer: 256 * 1024 * 1024,
  });
  return out
    .split('\n')
    .map((s) => s.trim())
    .filter(Boolean);
}

function main(): void {
  if (!fs.existsSync(CDP_FILE)) {
    console.error(
      `No capture file at ${CDP_FILE} — run a real sync with SHELFY_CAPTURE_MVP=1 first.`,
    );
    process.exit(2);
  }

  // CDP-captured URLs (browser-rendered images). Format: mime\tbytes\tsafeUrl
  const cdpUrls = fs
    .readFileSync(CDP_FILE, 'utf8')
    .split('\n')
    .map((l) => l.split('\t').pop() || '')
    .filter((u) => /^https?:/.test(u));

  // DB URLs (what we'd correlate against).
  // Dump path-only (drop the signed query) to keep the dump small and token-free.
  const dbUrls = [
    ...sql(
      `SELECT substr(thumbnail_url,1,instr(thumbnail_url||'?','?')-1) FROM posts WHERE thumbnail_url LIKE 'http%';`,
    ),
    ...sql(
      `SELECT substr(source_url,1,instr(source_url||'?','?')-1) FROM post_media WHERE source_url LIKE 'http%';`,
    ),
  ];

  console.log(`CDP-captured images : ${cdpUrls.length}`);
  console.log(`DB image URLs       : ${dbUrls.length}`);

  for (const platform of ['instagram', 'twitter'] as const) {
    const all = cdpUrls.filter((u) => platformOf(u) === platform);
    const cdp = all.filter(isPostMedia); // drop avatars / UI icons
    const db = dbUrls.filter((u) => platformOf(u) === platform);
    if (cdp.length === 0) {
      console.log(
        `\n[${platform}] no post-media captured yet (${all.length} noise) — open its saved/bookmarks and scroll.`,
      );
      continue;
    }
    console.log(
      `\n========== ${platform.toUpperCase()} (post-media=${cdp.length}, noise-excluded=${all.length - cdp.length}, db=${db.length}) ==========`,
    );
    for (const [name, keyOf] of Object.entries(STRATEGIES)) {
      const dbKeys = new Set(db.map(keyOf).filter(Boolean) as string[]);
      let matched = 0;
      const misses: string[] = [];
      for (const u of cdp) {
        const k = keyOf(u);
        if (k && dbKeys.has(k)) matched += 1;
        else if (misses.length < 6) misses.push(`${k ?? '(no-key)'}  ←  ${u.slice(0, 90)}`);
      }
      const pct = ((matched / cdp.length) * 100).toFixed(1);
      console.log(`  ${name.padEnd(14)} : ${matched}/${cdp.length} matched (${pct}%)`);
      if (matched < cdp.length && misses.length) {
        for (const m of misses) console.log(`      miss: ${m}`);
      }
    }
  }
  console.log(
    '\nVerdict: a strategy at/near 100% on both platforms ⇒ risk #1 (URL→post) is closed.',
  );
}

main();
