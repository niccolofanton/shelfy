// Smoke test del modulo reale electron/analyzer.js (non lo script spike):
// mocka `electron` e `./db`, poi esercita la coda end-to-end su un video.
//
// Uso: node scripts/analyzer-smoke.cjs [video.mp4]

import Module, { createRequire } from 'module';
import path from 'path';
import os from 'os';
import { fileURLToPath } from 'url';
import type { app as ElectronApp } from 'electron';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

type Analyzer = typeof import('../electron/analyzer');

// Node exposes the internal CommonJS loader on the Module object; @types/node does
// not declare it, so we describe just the slice we monkeypatch.
type ModuleLoad = (request: string, parent?: unknown, isMain?: boolean) => unknown;
interface ModuleWithLoad {
  _load: ModuleLoad;
}

// The subset of the AI patch this smoke reads back from the persisted calls.
interface AiPatch {
  status?: string | null;
  category?: string | null;
  contentType?: string | null;
  [k: string]: unknown;
}

// Match the real app: main.js calls app.setName('Shelfy') (capital).
const USERDATA = path.join(os.homedir(), 'Library/Application Support/Shelfy');
const persisted: Array<{ id: string; patch: AiPatch }> = [];

const origLoad = (Module as unknown as ModuleWithLoad)._load;
(Module as unknown as ModuleWithLoad)._load = function (this: unknown, request: string): unknown {
  if (request === 'electron') {
    return {
      app: { getPath: (k: string) => (k === 'userData' ? USERDATA : os.tmpdir()) },
    } satisfies { app: Pick<typeof ElectronApp, 'getPath'> };
  }
  if (request === './db') {
    return {
      updateAiAnalysis: (id: string, patch: AiPatch) => persisted.push({ id, patch }),
      getFrequentTags: () => [],
    };
  }
  // eslint-disable-next-line prefer-rest-params
  return origLoad.apply(this, arguments as unknown as Parameters<ModuleLoad>);
};

const analyzer = require(path.join(__dirname, '..', 'electron', 'analyzer')) as Analyzer;

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
    const lastPatch: AiPatch = persisted.length ? persisted[persisted.length - 1].patch : {};
    console.log('category:', lastPatch.category, '(undefined atteso per i contenuti social)');
    console.log('contentType:', lastPatch.contentType, '(undefined atteso per i contenuti social)');
    console.log('tags:', job.tags);
    console.log('entities:', job.entities);
    console.log('keywords:', job.keywords);
    console.log('saveReason:', job.saveReason);
    console.log('language:', job.language);
    console.log(
      '\npersisted calls:',
      persisted.map((p) => p.patch.status),
    );
    analyzer.shutdown();
    process.exit(0);
  }
  if (job.status === 'error' || job.status === 'cancelled') {
    analyzer.shutdown();
    process.exit(1);
  }
});

console.log(
  'enqueue:',
  analyzer.enqueuePost(post as unknown as Parameters<Analyzer['enqueuePost']>[0]),
);
setTimeout(() => {
  console.error('timeout');
  analyzer.shutdown();
  process.exit(2);
}, 240000);
