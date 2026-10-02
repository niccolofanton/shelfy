// DRY-RUN (read-only): for the images already captured this session, classify
// what saving them would do under the rule:
//   single image  → save as image_path + thumbnail_path  ⇒ COMPLETE offline
//   carousel/video → save cover as thumbnail_path only     ⇒ THUMB-ONLY (not complete)
// Writes NOTHING. Run: ./node_modules/.bin/tsx scripts/capture-spike/savecheck.ts

import fs from 'fs';
import path from 'path';
import os from 'os';
import { execFileSync } from 'child_process';

const HOME = os.homedir();
const CAP = path.join(HOME, 'Library/Application Support/Shelfy/capture-mvp/urls.txt');
const DB = path.join(HOME, 'Library/Application Support/Shelfy/shelfy.sqlite');

function platformOf(url: string): 'instagram' | 'twitter' | 'other' {
  if (/cdninstagram\.com|fbcdn\.net/.test(url)) return 'instagram';
  if (/twimg\.com/.test(url)) return 'twitter';
  return 'other';
}
function isPostMedia(url: string): boolean {
  const p = platformOf(url);
  if (p === 'instagram') return /\/t51\.\d+-15\//.test(url);
  if (p === 'twitter')
    return /\/(media|amplify_video_thumb)\//.test(url) && !/profile_images/.test(url);
  return false;
}
function basenameNoExt(url: string): string {
  try {
    return (new URL(url).pathname.split('/').pop() || '').replace(/\.[a-z0-9]+$/i, '');
  } catch {
    return '';
  }
}
function sql(q: string): string[] {
  return execFileSync('sqlite3', ['-readonly', '-separator', '\t', DB, q], {
    encoding: 'utf8',
    maxBuffer: 256 * 1024 * 1024,
  })
    .split('\n')
    .filter(Boolean);
}

// basename → {postId, mediaType, mediaCount}
const byBasename = new Map<string, { id: string; type: string; count: number }>();
function index(rows: string[]): void {
  for (const r of rows) {
    const [id, type, count, url] = r.split('\t');
    if (!url) continue;
    const b = basenameNoExt(url);
    if (b) byBasename.set(b, { id, type: type || '', count: Number(count) || 1 });
  }
}
index(
  sql(`SELECT id, media_type, media_count, substr(thumbnail_url,1,instr(thumbnail_url||'?','?')-1)
           FROM posts WHERE platform IN ('instagram','twitter') AND thumbnail_url LIKE 'http%';`),
);
index(
  sql(`SELECT p.id, p.media_type, p.media_count, substr(m.source_url,1,instr(m.source_url||'?','?')-1)
           FROM post_media m JOIN posts p ON p.id=m.post_id
           WHERE p.platform IN ('instagram','twitter') AND m.source_url LIKE 'http%';`),
);

const captured = fs
  .readFileSync(CAP, 'utf8')
  .split('\n')
  .map((l) => l.split('\t').pop() || '')
  .filter((u) => /^https?:/.test(u) && isPostMedia(u));

const isSingleImage = (t: string, c: number) => c <= 1 && (t === 'image' || t === 'images');

const complete = new Set<string>();
const thumbOnly = new Set<string>();
let unmatched = 0;
const byType: Record<string, number> = {};

for (const url of captured) {
  const hit = byBasename.get(basenameNoExt(url));
  if (!hit) {
    unmatched += 1;
    continue;
  }
  byType[hit.type] = (byType[hit.type] || 0) + 1;
  if (isSingleImage(hit.type, hit.count)) complete.add(hit.id);
  else thumbOnly.add(hit.id);
}

console.log('================ SAVE DRY-RUN (no writes) ================');
console.log(`captured post-media images : ${captured.length}`);
console.log(`  matched to a DB post      : ${captured.length - unmatched}`);
console.log(`  unmatched (feed/not saved): ${unmatched}`);
console.log('---------------------------------------------------------');
console.log(`distinct posts touched      : ${complete.size + thumbOnly.size}`);
console.log(`  → COMPLETE offline (single image)      : ${complete.size}`);
console.log(`  → THUMB-ONLY (carousel/video, cover)   : ${thumbOnly.size}`);
console.log('---------------------------------------------------------');
console.log('matched images by post media_type:');
for (const [t, n] of Object.entries(byType).sort((a, b) => b[1] - a[1])) {
  console.log(`  ${t.padEnd(10)} : ${n}`);
}
console.log('=========================================================');
