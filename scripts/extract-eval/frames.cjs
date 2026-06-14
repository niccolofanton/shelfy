// Exports the exact visual inputs the analyzer sees (4 evenly-spaced 448px JPEG
// frames per video, up to 4 carousel slides, 1 image) + the caption, into
// .scratch/gt-inputs/<postId>/ — so a vision agent can build the ground-truth
// oracle from the same evidence the local model gets.
//
// Plain node (ffmpeg only; no Electron/better-sqlite3). Usage: node scripts/extract-eval/frames.cjs

const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const HERE = __dirname;
const CASES = JSON.parse(fs.readFileSync(path.join(HERE, 'cases.json'), 'utf8'));
const OUT = path.join(HERE, '.scratch', 'gt-inputs');
const FFMPEG = process.env.FFMPEG_BIN || '/opt/homebrew/bin/ffmpeg';
const N_FRAMES = 4; // must match electron/analyzer.js (keyframes sampled per video)
const SCALE = 448;
const CAROUSEL_MAX = 4;

function exists(p) { try { return p && fs.statSync(p).isFile(); } catch { return false; } }

function probeDuration(file) {
  // Read duration from ffmpeg's banner (no ffprobe dependency).
  const r = spawnSync(FFMPEG, ['-i', file], { encoding: 'utf8' });
  const m = /Duration:\s*(\d+):(\d+):(\d+(?:\.\d+)?)/.exec(r.stderr || '');
  if (!m) return 0;
  return (+m[1]) * 3600 + (+m[2]) * 60 + parseFloat(m[3]);
}

function grabFrame(file, t, out) {
  const r = spawnSync(FFMPEG, ['-ss', t.toFixed(2), '-i', file, '-frames:v', '1',
    '-vf', `scale=${SCALE}:${SCALE}:force_original_aspect_ratio=decrease`,
    '-q:v', '4', '-f', 'image2', '-y', out], { stdio: 'ignore' });
  return r.status === 0 && exists(out);
}

function scaleImage(file, out) {
  const r = spawnSync(FFMPEG, ['-i', file, '-frames:v', '1',
    '-vf', `scale=${SCALE}:${SCALE}:force_original_aspect_ratio=decrease`,
    '-q:v', '4', '-f', 'image2', '-y', out], { stdio: 'ignore' });
  return r.status === 0 && exists(out);
}

const manifest = [];
for (const p of CASES.posts) {
  const dir = path.join(OUT, String(p.id));
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, 'caption.txt'), p.text || '');
  const frames = [];

  if (p.mediaType === 'video' && exists(p.videoPath)) {
    const dur = probeDuration(p.videoPath);
    for (let i = 0; i < N_FRAMES; i++) {
      const t = dur ? (dur * (i + 0.5)) / N_FRAMES : 0;
      const out = path.join(dir, `f${i}.jpg`);
      if (grabFrame(p.videoPath, t, out)) frames.push(path.basename(out));
      if (!dur) break;
    }
  } else if (p.mediaType === 'carousel' || p.mediaType === 'images' || (p.media || []).length > 1) {
    const slides = (p.media || []).filter((m) => (m.type === 'image' || !m.type) && exists(m.localPath)).slice(0, CAROUSEL_MAX);
    slides.forEach((m, i) => {
      const out = path.join(dir, `f${i}.jpg`);
      if (scaleImage(m.localPath, out)) frames.push(path.basename(out));
    });
  } else {
    const single = [p.imagePath, p.thumbnailPath, ...((p.media || []).map((m) => m.localPath))].find(exists);
    if (single) {
      const out = path.join(dir, 'f0.jpg');
      if (scaleImage(single, out)) frames.push(path.basename(out));
    }
  }

  manifest.push({ id: String(p.id), mediaType: p.mediaType, caption: p.text || '', frames });
  console.log(`${p.mediaType.padEnd(9)} ${String(p.id).slice(0, 24).padEnd(24)} → ${frames.length} frame${frames.length === 1 ? '' : 's'}`);
}

fs.writeFileSync(path.join(OUT, 'manifest.json'), JSON.stringify(manifest, null, 2));
console.log(`\nwrote ${manifest.length} posts to ${OUT}`);
