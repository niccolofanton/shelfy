// Smoke test del modulo reale electron/analyzer.js (non lo script spike):
// mocka `electron` e `./db`, poi esercita la coda end-to-end su un video.
//
// Uso: node scripts/analyzer-smoke.cjs [video.mp4]

const Module = require('module');
const path = require('path');
const os = require('os');

// Match the real app: main.js calls app.setName('Shelfy') (capital).
const USERDATA = path.join(os.homedir(), 'Library/Application Support/Shelfy');
const persisted = [];

const origLoad = Module._load;
Module._load = function (request, parent, isMain) {
  if (request === 'electron') {
    return { app: { getPath: (k) => (k === 'userData' ? USERDATA : os.tmpdir()) } };
  }
  if (request === './db') {
    return {
      updateAiAnalysis: (id, patch) => persisted.push({ id, patch }),
      getFrequentTags: () => [],
    };
  }
  return origLoad.apply(this, arguments);
};

const analyzer = require(path.join(__dirname, '..', 'electron', 'analyzer.js'));

const video = process.argv[2] || path.join(os.homedir(), 'Desktop/eyes.mp4');
const post = {
  id: 'smoke-1',
  shortcode: 'abc',
  mediaType: 'video',
  videoPath: video,
  authorUsername: 'tester',
  // Real caption of twitter-2057495678893302253 — reveals the true subject
  // (a GSAP/SVG animation tutorial) that the frames alone misread as a map/event.
  text:
    'Big thanks to Tom Miller for this beautiful new tutorial on creating cinematic scroll-driven SVG map animations with GSAP 😊\n\n' +
    'Such a clever mix of SVG, MotionPath, ScrollTrigger, and lightweight storytelling techniques, all without relying on a map API: https://t.co/ShBzPy0cnS',
};

console.log('model status:', analyzer.getModelStatus());

analyzer.setProgressEmitter((job) => {
  console.log(`  [job] ${job.status}${job.error ? ' — ' + job.error : ''}`);
  if (job.status === 'done') {
    console.log('\n─── RISULTATO ───');
    console.log('description:', job.description);
    // category/contentType non viaggiano nel patchJob 'done' (vengono solo scritti
    // nel DB via updateAiAnalysis) e sul percorso social restano comunque undefined:
    // li leggiamo dall'ultimo patch persistito invece che da `job`, dove sono assenti.
    const lastPatch = persisted.length ? persisted[persisted.length - 1].patch : {};
    console.log('category:', lastPatch.category, '(undefined atteso per i contenuti social)');
    console.log('contentType:', lastPatch.contentType, '(undefined atteso per i contenuti social)');
    console.log('tags:', job.tags);
    console.log('entities:', job.entities);
    console.log('keywords:', job.keywords);
    console.log('saveReason:', job.saveReason);
    console.log('language:', job.language);
    console.log('\npersisted calls:', persisted.map((p) => p.patch.status));
    analyzer.shutdown();
    process.exit(0);
  }
  if (job.status === 'error' || job.status === 'cancelled') {
    analyzer.shutdown();
    process.exit(1);
  }
});

console.log('enqueue:', analyzer.enqueuePost(post));
setTimeout(() => { console.error('timeout'); analyzer.shutdown(); process.exit(2); }, 240000);
